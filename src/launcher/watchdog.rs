//! Tracks launched clients, auto-rejoins on unexpected exit, runs anti-AFK.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use uuid::Uuid;

use super::process::ProcessHandle;
use super::{LaunchTarget, anti_afk, kill_process, launch, roblox_processes};
use crate::app::Shared;
use crate::applog::log;
use crate::roblox_api::presence::{PresenceType, presence};

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Launching,
    Running,
    /// Launched, but no new client process was detected, so it can't be watched.
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
    /// What the user asked for.
    pub target: LaunchTarget,
    /// Server the client was last seen in (from presence); used to rejoin the same server.
    pub job_id: Option<String>,
    /// Place of `job_id`. Differs from the target place after an in-game teleport.
    pub job_place_id: Option<u64>,
    pub process: Option<(u32, u64)>,
    /// Kept open to read the exit code once the client exits.
    pub handle: Option<ProcessHandle>,
    /// Set when the app kills the client itself to restart it.
    pub restarting: bool,
    pub status: Status,
    pub retries: u32,
    pub next_retry: Option<Instant>,
    pub running_since: Option<Instant>,
    pub last_presence: Option<Instant>,
    pub presence_inflight: bool,
    pub presence_misses: u32,
}

impl Session {
    /// Target for the next (re)launch: the same server while retries are
    /// fresh, falling back to the original target if that keeps failing.
    fn relaunch_target(&self) -> LaunchTarget {
        match (&self.target, &self.job_id) {
            (LaunchTarget::Place { place_id }, Some(job)) if self.retries <= 2 => {
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

/// Manual rejoin, e.g. after `GaveUp`.
pub fn rejoin_now(shared: &Arc<Shared>, sid: u64) {
    shared.update_session(sid, |s| {
        s.retries = 0;
        s.status = Status::Launching;
        s.next_retry = None;
    });
    let s = shared.clone();
    shared.rt.spawn(async move { run_launch(s, sid).await });
}

/// Main watchdog loop: one tick per second.
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
    /// Attempts left after the last client closed.
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

/// Returns how many Roblox clients are running.
fn tick(shared: &Arc<Shared>) -> usize {
    let cfg = shared.config.lock().unwrap().auto_rejoin.clone();
    let alive = roblox_processes();
    let now = Instant::now();
    let mut relaunch = Vec::new();
    let mut presence_jobs = Vec::new();
    let mut changed = false;

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
                    if s.target != LaunchTarget::Home && !s.presence_inflight {
                        // First look after 20 s to learn the job ID; then on the configured interval.
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
