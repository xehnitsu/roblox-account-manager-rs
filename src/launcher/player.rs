//! Starting `RobloxPlayerBeta.exe` directly with command-line arguments, the
//! way Bloxstrap does, instead of going through the `roblox-player:` link.

use std::path::{Path, PathBuf};

use super::{LaunchTarget, placelauncher_url};

/// The player the `roblox-player:` handler points to, i.e. the installed one.
pub fn installed_player() -> Option<PathBuf> {
    let command =
        windows_registry::CLASSES_ROOT.open(r"roblox-player\shell\open\command").and_then(|k| k.get_string("")).ok()?;
    let exe = exe_from_command(&command)?;
    exe.is_file().then_some(exe)
}

/// `"C:\...\RobloxPlayerBeta.exe" %1` -> the exe path.
fn exe_from_command(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    let path = match command.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => command.split(" %").next()?.trim(),
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// `version-...` folder name the player lives in.
pub fn version_of(exe: &Path) -> Option<String> {
    Some(exe.parent()?.file_name()?.to_str()?.to_owned())
}

/// Arguments equivalent to the launch link (Bloxstrap's mapping):
/// `launchmode:play` -> `--app`, `gameinfo` -> `-t`, `placelauncherurl` -> `-j`.
pub fn player_args(ticket: &str, target: &LaunchTarget, launch_time_ms: u128, tracker_id: u64) -> Vec<String> {
    let mut args = vec!["--app".to_owned(), "-t".to_owned(), ticket.to_owned()];
    if let Some(url) = placelauncher_url(target, tracker_id) {
        args.extend(["-j".to_owned(), url]);
    }
    args.extend([
        "-b".to_owned(),
        tracker_id.to_string(),
        format!("--launchtime={launch_time_ms}"),
        "--rloc".to_owned(),
        "en_us".to_owned(),
        "--gloc".to_owned(),
        "en_us".to_owned(),
    ]);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_handler_command() {
        let c = r#""C:\Users\a b\Roblox\Versions\version-1\RobloxPlayerBeta.exe" %1"#;
        assert_eq!(
            exe_from_command(c),
            Some(PathBuf::from(r"C:\Users\a b\Roblox\Versions\version-1\RobloxPlayerBeta.exe"))
        );
        assert_eq!(
            exe_from_command(r"C:\R\RobloxPlayerBeta.exe %1"),
            Some(PathBuf::from(r"C:\R\RobloxPlayerBeta.exe"))
        );
        assert_eq!(
            version_of(Path::new(r"C:\R\Versions\version-abc\RobloxPlayerBeta.exe")).as_deref(),
            Some("version-abc")
        );
    }

    #[test]
    fn job_args() {
        let t = LaunchTarget::Job { place_id: 1, job_id: "j".into() };
        let a = player_args("T", &t, 5, 7);
        assert_eq!(a[..3], ["--app", "-t", "T"]);
        assert_eq!(a[3], "-j");
        assert!(
            a[4].starts_with("https://assetgame.roblox.com/game/PlaceLauncher.ashx?") && a[4].ends_with("&gameId=j")
        );
        assert!(a.contains(&"--launchtime=5".to_owned()));
        let home = player_args("T", &LaunchTarget::Home, 5, 7);
        assert!(!home.contains(&"-j".to_owned()));
    }
}
