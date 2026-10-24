//! Tracks launched clients, auto-rejoins on unexpected exit, runs anti-AFK.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::client_log::{self, Drop, Event};
use super::process::ProcessHandle;
use super::{LaunchTarget, anti_afk, kill_process, launch, roblox_processes};
use crate::app::Shared;
use crate::applog::log;
use crate::roblox_api::presence::{PresenceType, presence};

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Launching,
    Running,
    NoProcess,
    WaitingRetry,
    GaveUp,
    /// The client exited cleanly (exit code 0), e.g. Alt+F4 or leaving the game.
    Closed,
    Stopped,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: u64,
    pub account_id: Uuid,
    pub label: String,
    pub target: LaunchTarget,
    /// Server the client was last seen in (from presence), used to rejoin it.
    pub job_id: Option<String>,
    /// Place of `job_id`. Differs from the target place after an in-game teleport.
    pub job_place_id: Option<u64>,
    pub process: Option<(u32, u64)>,
    pub handle: Option<ProcessHandle>,
    pub restarting: bool,
    pub status: Status,
    pub retries: u32,
    pub next_retry: Option<Instant>,
    pub running_since: Option<Instant>,
    pub last_presence: Option<Instant>,
    pub presence_inflight: bool,
    pub presence_misses: u32,
    /// The client's own log, once found. While it is, presence isn't polled.
    pub log: Option<client_log::Reader>,
    pub next_log_search: Option<Instant>,
    pub in_game: bool,
    pub left_game: bool,
    pub teleport_at: Option<Instant>,
    /// A disconnect waiting to be confirmed (a teleport also logs one).
    pub pending_drop: Option<(Instant, Drop)>,
    pub kick_message: Option<String>,
    pub last_drop: Option<String>,
}

const DROP_GRACE: Duration = Duration::from_secs(8);

impl Session {
    /// Target for the next (re)launch: the same server while retries are
    /// fresh, falling back to the original target if that keeps failing.
    fn relaunch_target(&self) -> LaunchTarget {
        match (&self.target, &self.job_id) {
            (LaunchTarget::Place { place_id } | LaunchTarget::Follow { place_id, .. }, Some(job))
                if self.retries <= 2 =>
            {
                LaunchTarget::Job { place_id: self.job_place_id.unwrap_or(*place_id), job_id: job.clone() }
            }
            _ => self.target.clone(),
        }
    }
}

/// Crashes end with a Windows error status (0xC0000005 access violation,
/// 0xC0000409 fast fail, ...). Small codes mean the client was closed on
/// purpose: 0 for Alt+F4 or leaving the game, 1 for a force close (Task
/// Manager, taskkill, "super Alt+F4").
pub fn is_crash(exit_code: u32) -> bool {
    exit_code >= 0x8000_0000
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn start_session(shared: &Arc<Shared>, account_id: Uuid, target: LaunchTarget) {
    let Some(acc) = shared.account(account_id) else { return };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    shared.sessions.lock().unwrap().push(Session {
        id,
        account_id,
        label: acc.label(),
        job_id: match &target {
            LaunchTarget::Job { job_id, .. } => Some(job_id.clone()),
            _ => None,
        },
        job_place_id: target.place_id(),
        target,
        process: None,
        handle: None,
        restarting: false,
        status: Status::Launching,
        retries: 0,
        next_retry: None,
        running_since: None,
        last_presence: None,
        presence_inflight: false,
        presence_misses: 0,
        log: None,
        next_log_search: None,
        in_game: false,
        left_game: false,
        teleport_at: None,
        pending_drop: None,
        kick_message: None,
        last_drop: None,
    });
    let s = shared.clone();
    shared.rt.spawn(async move { run_launch(s, id).await });
}

async fn run_launch(shared: Arc<Shared>, sid: u64) {
    let Some(sess) = shared.session(sid) else { return };
    let Some(acc) = shared.account(sess.account_id) else {
        shared.update_session(sid, |s| s.status = Status::Failed("account was removed".into()));
        return;
    };
    let target = sess.relaunch_target();
    let result = launch(&shared, &acc, &target).await;
    let cfg = shared.config.lock().unwrap().auto_rejoin.clone();
    shared.update_session(sid, |s| {
        if s.status == Status::Stopped {
            return;
        }
        match result {
            Ok(Some(p)) => {
                s.process = Some(p);
                s.handle = ProcessHandle::open(p.0);
                s.restarting = false;
                s.status = Status::Running;
                s.running_since = Some(Instant::now());
                s.last_presence = None;
                s.presence_misses = 0;
                s.log = None;
                s.next_log_search = None;
                s.in_game = false;
                s.left_game = false;
                s.teleport_at = None;
                s.pending_drop = None;
                s.kick_message = None;
            }
            Ok(None) => s.status = Status::NoProcess,
            Err(e) => {
                log(format!("{}: launch failed: {e:#}", s.label));
                if s.retries > 0 && cfg.enabled && s.retries < cfg.max_retries {
                    schedule_retry(s, &cfg);
                } else {
                    s.status = Status::Failed(format!("{e:#}"));
                }
            }
        }
    });
    shared.ctx.request_repaint();
}

fn schedule_retry(s: &mut Session, cfg: &crate::config::AutoRejoinConfig) {
    let delay = cfg.base_delay_secs.saturating_mul(1u64 << s.retries.min(16)).min(cfg.max_delay_secs);
    s.retries += 1;
    s.status = Status::WaitingRetry;
    s.next_retry = Some(Instant::now() + Duration::from_secs(delay));
    s.process = None;
    s.handle = None;
    log(format!("{}: rejoin {}/{} in {delay} s", s.label, s.retries, cfg.max_retries));
}

pub fn stop_session(shared: &Shared, sid: u64) {
    shared.update_session(sid, |s| {
        s.status = Status::Stopped;
        s.next_retry = None;
        s.handle = None;
        if let Some((pid, _)) = s.process.take() {
            kill_process(pid);
        }
    });
}

pub fn rejoin_now(shared: &Arc<Shared>, sid: u64) {
    shared.update_session(sid, |s| {
        s.retries = 0;
        s.status = Status::Launching;
        s.next_retry = None;
    });
    let s = shared.clone();
    shared.rt.spawn(async move { run_launch(s, sid).await });
}

pub async fn run(shared: Arc<Shared>) {
    let mut last_afk = Instant::now();
    let mut mutex_retry = MutexRetry::default();
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let running = tick(&shared);
        mutex_retry.on_tick(&shared, running);
        let afk = shared.config.lock().unwrap().anti_afk.clone();
        if afk.enabled && last_afk.elapsed() >= Duration::from_secs(afk.interval_secs.max(30)) {
            last_afk = Instant::now();
            let pids: HashSet<u32> = shared.claimed_pids();
            if !pids.is_empty() {
                let n =
                    tokio::task::spawn_blocking(move || anti_afk::nudge(&pids, afk.key, afk.mode)).await.unwrap_or(0);
                log(format!("anti-AFK: nudged {n} window(s)"));
            }
        }
    }
}

/// Retakes the singleton mutex once the last Roblox client closes, if the app
/// couldn't take it earlier (e.g. it was started while Roblox was running).
#[derive(Default)]
struct MutexRetry {
    last_running: usize,
    attempts_left: u32,
    last_attempt: Option<Instant>,
}

impl MutexRetry {
    fn on_tick(&mut self, shared: &Shared, running: usize) {
        if running == 0 && self.last_running > 0 {
            self.attempts_left = 3;
        }
        self.last_running = running;
        if running > 0 || self.attempts_left == 0 {
            return;
        }
        if self.last_attempt.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        let cfg = shared.config.lock().unwrap().multi_instance.clone();
        let mut multi = shared.multi.lock().unwrap();
        if !cfg.enabled || multi.mutex_held() {
            self.attempts_left = 0;
            return;
        }
        self.attempts_left -= 1;
        self.last_attempt = Some(Instant::now());
        log("[multi] last Roblox client closed; retrying singleton mutex");
        multi.apply(&cfg);
        if multi.mutex_held() {
            self.attempts_left = 0;
            shared.ctx.request_repaint();
        }
    }
}

fn tick(shared: &Arc<Shared>) -> usize {
    let cfg = shared.config.lock().unwrap().auto_rejoin.clone();
    let alive = roblox_processes();
    let now = Instant::now();
    let mut relaunch = Vec::new();
    let mut presence_jobs = Vec::new();
    let mut changed = false;
    let mut to_kill = Vec::new();
    find_logs(shared, now);

    {
        let mut sessions = shared.sessions.lock().unwrap();
        for s in sessions.iter_mut() {
            match s.status {
                Status::Running => {
                    let Some(p) = s.process else { continue };
                    if !alive.contains(&p) {
                        changed = true;
                        let code = s.handle.take().and_then(|h| h.exit_code());
                        if code.is_some_and(|c| !is_crash(c)) && !s.restarting {
                            log(format!("{}: Roblox (PID {}) closed, not rejoining", s.label, p.0));
                            s.status = Status::Closed;
                            s.process = None;
                            continue;
                        }
                        let code = code.map_or("unknown".to_owned(), |c| format!("{c:#x}"));
                        log(format!("{}: Roblox (PID {}) exited (code {code})", s.label, p.0));
                        if cfg.enabled && s.retries < cfg.max_retries {
                            schedule_retry(s, &cfg);
                        } else {
                            s.status = Status::GaveUp;
                            s.process = None;
                        }
                        continue;
                    }
                    let up = s.running_since.map(|t| now - t).unwrap_or_default();
                    // A client that stayed up 5 minutes counts as healthy again.
                    if up > Duration::from_secs(300) && s.retries > 0 {
                        s.retries = 0;
                    }
                    if read_log(s, now, cfg.enabled, &mut to_kill) {
                        changed = true;
                    }
                    if s.target != LaunchTarget::Home && s.log.is_none() && !s.presence_inflight {
                        // First look after 20 s to learn the job ID, then on the configured interval.
                        let every = if s.last_presence.is_none() {
                            Duration::from_secs(20)
                        } else {
                            Duration::from_secs(cfg.presence_interval_secs.max(15))
                        };
                        let since = s.last_presence.map(|t| now - t).unwrap_or(up);
                        if since >= every {
                            s.presence_inflight = true;
                            presence_jobs.push((s.id, s.account_id));
                        }
                    }
                }
                Status::WaitingRetry if s.next_retry.is_some_and(|t| t <= now) => {
                    s.status = Status::Launching;
                    s.next_retry = None;
                    relaunch.push(s.id);
                    changed = true;
                }
                _ => {}
            }
        }
    }

    // Killing makes the next tick see an exit and schedule a rejoin.
    for pid in to_kill {
        kill_process(pid);
    }
    for sid in relaunch {
        let s = shared.clone();
        shared.rt.spawn(async move { run_launch(s, sid).await });
    }
    for (sid, account_id) in presence_jobs {
        let s = shared.clone();
        shared.rt.spawn(async move { check_presence(s, sid, account_id).await });
    }
    if changed {
        shared.ctx.request_repaint();
    }
    alive.len()
}

fn find_logs(shared: &Shared, now: Instant) {
    let (pids, claimed): (Vec<u32>, Vec<std::path::PathBuf>) = {
        let mut sessions = shared.sessions.lock().unwrap();
        let claimed = sessions.iter().filter_map(|s| s.log.as_ref().map(|l| l.path.clone())).collect();
        let pids = sessions
            .iter_mut()
            .filter(|s| s.status == Status::Running && s.log.is_none())
            .filter(|s| s.next_log_search.is_none_or(|t| t <= now))
            .filter_map(|s| {
                // A new client writes its log within seconds, so look often at first.
                let young = s.running_since.is_some_and(|t| now.saturating_duration_since(t) < Duration::from_secs(60));
                s.next_log_search = Some(now + Duration::from_secs(if young { 2 } else { 10 }));
                s.process.map(|p| p.0)
            })
            .collect();
        (pids, claimed)
    };
    if pids.is_empty() {
        return;
    }
    let found = client_log::find_logs(&pids, &claimed);
    let mut sessions = shared.sessions.lock().unwrap();
    for s in sessions.iter_mut() {
        if let Some(path) = s.process.and_then(|p| found.get(&p.0)) {
            s.log = Some(client_log::Reader::new(path.clone()));
        }
    }
}

/// Applies what the client logged since last time. Queues the client for a
/// restart when it dropped out of a game and auto-rejoin is on. Returns true
/// if something the Sessions tab shows changed.
fn read_log(s: &mut Session, now: Instant, rejoin: bool, to_kill: &mut Vec<u32>) -> bool {
    let Some(reader) = s.log.as_mut() else { return false };
    let mut changed = false;
    for line in reader.read_new() {
        match client_log::classify(&line) {
            Some(Event::Joined { place_id, job_id }) => {
                s.in_game = true;
                s.left_game = false;
                s.pending_drop = None;
                s.kick_message = None;
                s.job_id = Some(job_id);
                s.job_place_id = Some(place_id);
                changed = true;
            }
            Some(Event::Teleporting) => s.teleport_at = Some(now),
            Some(Event::Left) => {
                s.in_game = false;
                s.left_game = true;
                s.pending_drop = None;
                changed = true;
            }
            Some(Event::KickMessage(msg)) => s.kick_message = Some(msg),
            Some(Event::Disconnected(code)) => {
                if let Some(drop) = Drop::from_code(code)
                    && !s.left_game
                    && s.pending_drop.is_none()
                {
                    s.pending_drop = Some((now, drop));
                }
            }
            None => {}
        }
    }
    let Some((at, drop)) = s.pending_drop else { return changed };
    // A teleport logs a disconnect too, so give it longer to show up in the new game.
    let teleporting = s.teleport_at.is_some_and(|t| now.saturating_duration_since(t) < DROP_GRACE * 2);
    let grace = if teleporting { DROP_GRACE * 2 } else { DROP_GRACE };
    if now.saturating_duration_since(at) < grace {
        return changed;
    }
    s.pending_drop = None;
    s.in_game = false;
    let mut reason = drop.describe();
    if drop == Drop::Kicked
        && let Some(msg) = &s.kick_message
    {
        reason = format!("{reason}: {msg}");
    }
    s.last_drop = Some(reason.clone());
    // A kick comes from the game or a moderator on purpose, so it isn't fought.
    if rejoin && drop != Drop::Kicked {
        log(format!("{}: {reason}; restarting client", s.label));
        if let Some(p) = s.process {
            to_kill.push(p.0);
            s.restarting = true;
        }
    } else {
        log(format!("{}: {reason}", s.label));
    }
    true
}

async fn check_presence(shared: Arc<Shared>, sid: u64, account_id: Uuid) {
    let Some(acc) = shared.account(account_id) else { return };
    let result = presence(&acc.cookie, acc.user_id).await;
    let check_enabled = shared.config.lock().unwrap().auto_rejoin.presence_check;
    let mut to_kill = None;
    shared.update_session(sid, |s| {
        s.presence_inflight = false;
        s.last_presence = Some(Instant::now());
        if s.status != Status::Running {
            return;
        }
        match result {
            Ok(p) if p.kind == PresenceType::InGame => {
                s.presence_misses = 0;
                if let Some(job) = p.job_id {
                    s.job_id = Some(job);
                    s.job_place_id = p.place_id.or(s.job_place_id);
                }
            }
            Ok(p) => {
                let loaded = s.running_since.is_some_and(|t| t.elapsed() > Duration::from_secs(60));
                if check_enabled && loaded {
                    s.presence_misses += 1;
                    if s.presence_misses >= 2 {
                        log(format!("{}: presence is {:?}, not in game; restarting client", s.label, p.kind));
                        to_kill = s.process.map(|p| p.0);
                        s.restarting = true;
                    }
                }
            }
            Err(e) => log(format!("{}: presence check failed: {e:#}", s.label)),
        }
    });
    // Killing makes the next tick see an exit and schedule a rejoin.
    if let Some(pid) = to_kill {
        kill_process(pid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(target: LaunchTarget, job: Option<&str>, retries: u32) -> Session {
        Session {
            id: 1,
            account_id: Uuid::nil(),
            label: "x".into(),
            target,
            job_id: job.map(str::to_owned),
            job_place_id: None,
            process: None,
            handle: None,
            restarting: false,
            status: Status::Running,
            retries,
            next_retry: None,
            running_since: None,
            last_presence: None,
            presence_inflight: false,
            presence_misses: 0,
            log: None,
            next_log_search: None,
            in_game: false,
            left_game: false,
            teleport_at: None,
            pending_drop: None,
            kick_message: None,
            last_drop: None,
        }
    }

    #[test]
    fn rejoins_same_server_then_falls_back() {
        let place = LaunchTarget::Place { place_id: 5 };
        let s = session(place.clone(), Some("job"), 1);
        assert_eq!(s.relaunch_target(), LaunchTarget::Job { place_id: 5, job_id: "job".into() });
        let s = session(place.clone(), Some("job"), 3);
        assert_eq!(s.relaunch_target(), place);
        let s = session(place.clone(), None, 1);
        assert_eq!(s.relaunch_target(), place);
        // After an in-game teleport the job belongs to another place.
        let mut s = session(place.clone(), Some("job"), 1);
        s.job_place_id = Some(9);
        assert_eq!(s.relaunch_target(), LaunchTarget::Job { place_id: 9, job_id: "job".into() });
    }

    #[test]
    fn crash_codes() {
        assert!(!is_crash(0));
        assert!(!is_crash(1));
        assert!(is_crash(0xC000_0005));
        assert!(is_crash(0xC000_0409));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        let cfg = crate::config::AutoRejoinConfig { base_delay_secs: 5, max_delay_secs: 30, ..Default::default() };
        let mut s = session(LaunchTarget::Home, None, 0);
        let mut delays = Vec::new();
        for _ in 0..4 {
            let before = Instant::now();
            schedule_retry(&mut s, &cfg);
            delays.push((s.next_retry.unwrap() - before).as_secs_f64().round() as u64);
        }
        assert_eq!(delays, vec![5, 10, 20, 30]);
    }
}
