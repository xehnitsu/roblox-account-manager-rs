//! Launching Roblox clients for stored accounts.

pub mod anti_afk;
pub mod multi_instance;
pub mod watchdog;

use std::collections::HashSet;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

use crate::accounts::Account;
use crate::applog::log;
use crate::roblox_api::{auth, games};

pub const PLAYER_EXE: &str = "RobloxPlayerBeta.exe";

/// Where a client should end up. `place_id` is required for everything but Home.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchTarget {
    Home,
    Place { place_id: u64 },
    PrivateServer { place_id: u64, link_code: String },
    Job { place_id: u64, job_id: String },
}

impl LaunchTarget {
    pub fn place_id(&self) -> Option<u64> {
        match self {
            LaunchTarget::Home => None,
            LaunchTarget::Place { place_id }
            | LaunchTarget::PrivateServer { place_id, .. }
            | LaunchTarget::Job { place_id, .. } => Some(*place_id),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            LaunchTarget::Home => "Home".into(),
            LaunchTarget::Place { place_id } => format!("place {place_id}"),
            LaunchTarget::PrivateServer { place_id, .. } => format!("private server in {place_id}"),
            LaunchTarget::Job { place_id, job_id } => format!("place {place_id}, job {job_id}"),
        }
    }
}

/// What the user typed in the launch panel, before resolving share links.
#[derive(Debug, Clone)]
pub enum TargetInput {
    Home,
    Place(String),
    PrivateServer(String),
    Job { place_id: String, job_id: String },
}

fn parse_place(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Ok(id) = s.parse() {
        return Ok(id);
    }
    // Accept a game URL too: https://www.roblox.com/games/<id>/name
    let re = regex::Regex::new(r"/games/(\d+)").unwrap();
    re.captures(s).and_then(|c| c[1].parse().ok()).ok_or_else(|| anyhow!("'{s}' is not a place ID or game URL"))
}

/// Pulls `(place_id, link_code)` out of a classic private-server URL, or the
/// share code out of a `/share?code=...&type=Server` link.
pub enum PrivateLink {
    Direct { place_id: u64, link_code: String },
    Share { code: String },
}

pub fn parse_private_link(input: &str) -> Result<PrivateLink> {
    let input = input.trim();
    let parsed = url::Url::parse(input).context("paste the full private server link")?;
    let q = |k: &str| parsed.query_pairs().find(|(key, _)| key == k).map(|(_, v)| v.into_owned());
    if let Some(link_code) = q("privateServerLinkCode") {
        return Ok(PrivateLink::Direct { place_id: parse_place(parsed.path())?, link_code });
    }
    if let Some(code) = q("code") {
        return Ok(PrivateLink::Share { code });
    }
    bail!("link has neither privateServerLinkCode nor a share code")
}

pub async fn resolve_target(cookie: &str, input: &TargetInput) -> Result<LaunchTarget> {
    Ok(match input {
        TargetInput::Home => LaunchTarget::Home,
        TargetInput::Place(p) => LaunchTarget::Place { place_id: parse_place(p)? },
        TargetInput::Job { place_id, job_id } => {
            let job_id = job_id.trim();
            if job_id.is_empty() {
                bail!("job ID is empty");
            }
            LaunchTarget::Job { place_id: parse_place(place_id)?, job_id: job_id.to_owned() }
        }
        TargetInput::PrivateServer(link) => match parse_private_link(link)? {
            PrivateLink::Direct { place_id, link_code } => LaunchTarget::PrivateServer { place_id, link_code },
            PrivateLink::Share { code } => {
                let (place_id, link_code) = games::resolve_share_link(cookie, &code).await?;
                LaunchTarget::PrivateServer { place_id, link_code }
            }
        },
    })
}

/// Builds the `roblox-player:` URI the website's Play button uses.
pub fn build_launch_uri(ticket: &str, target: &LaunchTarget, launch_time_ms: u128, tracker_id: u64) -> String {
    let tail = format!("+browsertrackerid:{tracker_id}+robloxLocale:en_us+gameLocale:en_us");
    let Some(place_id) = target.place_id() else {
        return format!("roblox-player:1+launchmode:play+gameinfo:{ticket}+launchtime:{launch_time_ms}{tail}");
    };
    let mut placelauncher = format!(
        "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&browserTrackerId={tracker_id}&placeId={place_id}&isPlayTogetherGame=false"
    );
    match target {
        LaunchTarget::PrivateServer { link_code, .. } => placelauncher.push_str(&format!("&linkCode={link_code}")),
        LaunchTarget::Job { job_id, .. } => placelauncher.push_str(&format!("&gameId={job_id}")),
        _ => {}
    }
    let placelauncher: String = url::form_urlencoded::byte_serialize(placelauncher.as_bytes()).collect();
    format!(
        "roblox-player:1+launchmode:play+gameinfo:{ticket}+launchtime:{launch_time_ms}+placelauncherurl:{placelauncher}{tail}"
    )
}

/// `(pid, start_time)` for every running Roblox player. Start time
/// distinguishes a reused PID from the original process.
pub fn roblox_processes() -> HashSet<(u32, u64)> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    sys.processes()
        .values()
        .filter(|p| p.name().eq_ignore_ascii_case(PLAYER_EXE))
        .map(|p| (p.pid().as_u32(), p.start_time()))
        .collect()
}

pub fn kill_process(pid: u32) -> bool {
    let mut sys = System::new();
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).map(|p| p.kill()).unwrap_or(false)
}

/// Serializes launches so each new process can be attributed to one account.
static LAUNCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Gets a ticket, opens the launch URI, and waits for the new client process.
/// Returns the `(pid, start_time)` of the client, if one appeared in time.
pub async fn launch(
    shared: &crate::app::Shared,
    account: &Account,
    target: &LaunchTarget,
) -> Result<Option<(u32, u64)>> {
    let _guard = LAUNCH_LOCK.lock().await;
    {
        let cfg = shared.config.lock().unwrap().multi_instance.clone();
        shared.multi.lock().unwrap().apply(&cfg);
    }

    let ticket =
        auth::auth_ticket(&account.cookie).await.with_context(|| format!("auth ticket for {}", account.username))?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let tracker_id: u64 = {
        let b: [u8; 8] = crate::accounts::crypto::random_bytes()?;
        1_000_000_000_000_000 + u64::from_le_bytes(b) % 8_000_000_000_000_000
    };
    let uri = build_launch_uri(&ticket, target, now, tracker_id);

    let before = roblox_processes();
    log(format!("Launching {} into {}", account.username, target.describe()));
    open::that_detached(&uri).context("no handler for roblox-player: (is Roblox installed?)")?;

    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let claimed = shared.claimed_pids();
        if let Some(p) = roblox_processes().into_iter().find(|p| !before.contains(p) && !claimed.contains(&p.0)) {
            log(format!("{} is running as PID {}", account.username, p.0));
            // Let the client settle before the next queued launch starts.
            let delay = shared.config.lock().unwrap().launch_delay_secs;
            tokio::time::sleep(Duration::from_secs(delay)).await;
            return Ok(Some(p));
        }
    }
    log(format!("{}: no new Roblox process seen within 60 s", account.username));
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_uri() {
        let u = build_launch_uri("TICKET", &LaunchTarget::Home, 1, 2);
        assert_eq!(
            u,
            "roblox-player:1+launchmode:play+gameinfo:TICKET+launchtime:1+browsertrackerid:2+robloxLocale:en_us+gameLocale:en_us"
        );
    }

    #[test]
    fn job_uri_contains_encoded_placelauncher() {
        let t = LaunchTarget::Job { place_id: 606849621, job_id: "abc-123".into() };
        let u = build_launch_uri("T", &t, 1, 2);
        assert!(u.contains("+placelauncherurl:https%3A%2F%2Fassetgame.roblox.com"));
        assert!(u.contains("placeId%3D606849621"));
        assert!(u.contains("gameId%3Dabc-123"));
        assert!(!u.contains("&"), "raw & would be split by the shell handler");
    }

    #[test]
    fn parses_private_links() {
        match parse_private_link("https://www.roblox.com/games/123/Name?privateServerLinkCode=987").unwrap() {
            PrivateLink::Direct { place_id, link_code } => {
                assert_eq!(place_id, 123);
                assert_eq!(link_code, "987");
            }
            _ => panic!(),
        }
        assert!(matches!(
            parse_private_link("https://www.roblox.com/share?code=abcd&type=Server").unwrap(),
            PrivateLink::Share { .. }
        ));
        assert_eq!(parse_place(" 42 ").unwrap(), 42);
        assert_eq!(parse_place("https://www.roblox.com/games/42/x").unwrap(), 42);
    }
}
