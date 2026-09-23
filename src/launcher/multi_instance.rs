//! Multi-instance support, based on evanovar/RobloxAccountManager:
//!
//! 1. Hold the Roblox singleton mutexes (`ROBLOX_singletonEvent` and friends)
//!    so each new client fails to claim them and runs anyway.
//! 2. Optional handle64 monitor: every ~400 ms, find `RobloxPlayerBeta.exe`
//!    processes and force-close their singleton handles with Sysinternals
//!    `handle64.exe` (requires admin).
//! 3. Error 773 fix: hold a non-blocking exclusive lock on `RobloxCookies.dat`
//!    so simultaneous clients don't overwrite each other's cookie state.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::HSTRING;

use crate::applog::log;
use crate::launcher::roblox_processes;

/// Names Roblox has used for its single-instance object across versions.
pub const SINGLETON_NAMES: [&str; 3] = ["ROBLOX_singletonEvent", "ROBLOX_SingletonEvent", "ROBLOX_singletonMutex"];

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------- mutexes --

pub struct SingletonGuard {
    handles: Vec<HANDLE>,
    /// True when we created (and therefore own) at least one of the objects.
    /// False means Roblox was already running and owns them.
    pub owned: bool,
}

// SAFETY: the mutex handles are only closed in Drop; kernel handles may be
// used from any thread.
unsafe impl Send for SingletonGuard {}

impl SingletonGuard {
    pub fn acquire() -> Result<Self> {
        let mut handles = Vec::new();
        let mut owned = false;
        for name in SINGLETON_NAMES {
            // SAFETY: plain Win32 call with a valid, NUL-terminated name.
            let created = unsafe { CreateMutexW(None, true, &HSTRING::from(name)) };
            // GetLastError must be read right after CreateMutexW.
            let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            match created {
                Ok(h) => {
                    owned |= !already;
                    handles.push(h);
                }
                Err(e) => log(format!("[multi] CreateMutexW({name}) failed: {e}")),
            }
        }
        if handles.is_empty() {
            bail!("could not create any Roblox singleton mutex");
        }
        Ok(Self { handles, owned })
    }
}

impl Drop for SingletonGuard {
    fn drop(&mut self) {
        for h in self.handles.drain(..) {
            // SAFETY: each handle came from CreateMutexW and is closed once.
            unsafe {
                let _ = CloseHandle(h);
            }
        }
    }
}

// ------------------------------------------------------- error 773 lock --

pub struct CookieLock {
    _file: File,
    pub path: PathBuf,
}

pub fn cookie_file_candidates() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // Current client location.
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        v.push(PathBuf::from(local).join(r"Roblox\LocalStorage\RobloxCookies.dat"));
    }
    // Older client location.
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        v.push(PathBuf::from(profile).join(r"AppData\LocalLow\Roblox\RobloxCookies.dat"));
    }
    v
}

impl CookieLock {
    /// Non-blocking exclusive lock on the first `RobloxCookies.dat` found.
    pub fn acquire() -> Result<Self> {
        let path = cookie_file_candidates()
            .into_iter()
            .find(|p| p.exists())
            .ok_or_else(|| anyhow!("RobloxCookies.dat not found (start Roblox once first)"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("opening {}", path.display()))?;
        file.try_lock().map_err(|e| anyhow!("could not lock {}: {e}", path.display()))?;
        Ok(Self { _file: file, path })
    }
}

// ------------------------------------------------------- handle64 monitor --

pub fn is_admin() -> bool {
    // SAFETY: no arguments, no side effects.
    unsafe { windows::Win32::UI::Shell::IsUserAnAdmin().as_bool() }
}

/// Looks for `handle64.exe` in `<exe dir>\utils\`, next to the exe, and in the data dir.
pub fn find_handle64() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join("utils").join("handle64.exe"));
        candidates.push(dir.join("handle64.exe"));
    }
    candidates.push(crate::config::data_dir().join("handle64.exe"));
    candidates.into_iter().find(|p| p.exists())
}

/// Downloads Handle.zip from Microsoft Sysinternals and extracts handle64.exe
/// into the data dir. Only called when the user clicks the download button.
pub async fn download_handle64() -> Result<PathBuf> {
    let bytes = crate::roblox_api::client()
        .get("https://download.sysinternals.com/files/Handle.zip")
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let dest = crate::config::data_dir().join("handle64.exe");
    let dest2 = dest.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes))?;
        let mut entry = zip.by_name("handle64.exe")?;
        std::fs::create_dir_all(dest2.parent().expect("has parent"))?;
        let mut out = File::create(&dest2)?;
        std::io::copy(&mut entry, &mut out)?;
        Ok(())
    })
    .await??;
    Ok(dest)
}

fn handle_regexes() -> &'static (Regex, Regex) {
    static RE: std::sync::OnceLock<(Regex, Regex)> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        (
            Regex::new(r"(?i)(?:^|[\s\\])(ROBLOX_(?:singletonevent|singletonmutex))\s*$").unwrap(),
            Regex::new(r"(?i)(?:^|\s)(?:0x)?([0-9A-F]+):").unwrap(),
        )
    })
}

/// Parses `handle64 -p <pid> -a` output into `(handle_hex, object_name)` pairs.
pub fn parse_handles(output: &str) -> Vec<(String, String)> {
    let (obj_re, handle_re) = handle_regexes();
    let mut out: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        let Some(obj) = obj_re.captures(line.trim()) else { continue };
        let Some(h) = handle_re.captures(line) else { continue };
        let hv = h[1].to_owned();
        if !out.iter().any(|(v, _)| v.eq_ignore_ascii_case(&hv)) {
            out.push((hv, obj[1].to_owned()));
        }
    }
    out
}

fn run_handle64(exe: &Path, args: &[&str]) -> Result<(i32, String)> {
    let out = Command::new(exe)
        .args(["-accepteula", "-nobanner"])
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .with_context(|| format!("running {}", exe.display()))?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.code().unwrap_or(-1), text))
}

fn query_handles(exe: &Path, pid: u32) -> Result<Vec<(String, String)>> {
    let (code, text) = run_handle64(exe, &["-p", &pid.to_string(), "-a"])?;
    if code != 0 && parse_handles(&text).is_empty() && !text.contains("No matching handles") {
        bail!("handle64 query exit {code}: {}", text.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    Ok(parse_handles(&text))
}

fn close_handles(exe: &Path, pid: u32, handles: &[(String, String)]) -> bool {
    let mut ok = true;
    for (hv, name) in handles {
        match run_handle64(exe, &["-p", &pid.to_string(), "-c", hv, "-y"]) {
            Ok((0, _)) => {}
            Ok((code, text)) => {
                ok = false;
                let tail: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
                log(format!("[multi] closing {name}:{hv} in PID {pid} failed (exit {code}): {tail}"));
            }
            Err(e) => {
                ok = false;
                log(format!("[multi] closing {name}:{hv} in PID {pid} failed: {e}"));
            }
        }
    }
    ok
}

#[derive(Default)]
struct ProcState {
    clean_scans: u32,
    required_scans: u32,
    done: bool,
    failures: u32,
}

pub struct Handle64Monitor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Handle64Monitor {
    pub fn start(exe: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let thread = std::thread::Builder::new()
            .name("handle64-monitor".into())
            .spawn(move || monitor_loop(exe, stop2))
            .expect("spawn handle64 monitor");
        Self { stop, thread: Some(thread) }
    }
}

impl Drop for Handle64Monitor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn monitor_loop(exe: PathBuf, stop: Arc<AtomicBool>) {
    log(format!("[multi] handle64 monitor started ({})", exe.display()));
    // Keyed by (pid, start_time) so a reused PID counts as a new process.
    let mut states: HashMap<(u32, u64), ProcState> = HashMap::new();
    let mut first = true;
    while !stop.load(Ordering::Relaxed) {
        let procs = roblox_processes();
        states.retain(|k, _| procs.contains(k));
        for &(pid, start) in &procs {
            let st = states.entry((pid, start)).or_insert_with(|| ProcState {
                // Processes that already existed when monitoring began only
                // need a quick check; new ones are watched for a few seconds
                // because Roblox creates the singleton shortly after start.
                required_scans: if first { 2 } else { 15 },
                ..Default::default()
            });
            if st.done || stop.load(Ordering::Relaxed) {
                continue;
            }
            match query_handles(&exe, pid) {
                Ok(h) if h.is_empty() => {
                    st.clean_scans += 1;
                    if st.clean_scans >= st.required_scans {
                        st.done = true;
                    }
                }
                Ok(h) => {
                    if close_handles(&exe, pid, &h) && query_handles(&exe, pid).map(|v| v.is_empty()).unwrap_or(false) {
                        log(format!("[multi] closed singleton handle(s) in PID {pid}"));
                        st.done = true;
                    }
                }
                Err(e) => {
                    st.failures += 1;
                    if st.failures == 1 || st.failures.is_multiple_of(25) {
                        log(format!("[multi] PID {pid}: {e}"));
                    }
                }
            }
        }
        first = false;
        std::thread::sleep(Duration::from_millis(400));
    }
    log("[multi] handle64 monitor stopped");
}

// ------------------------------------------------------------------ facade --

/// Everything multi-instance holds for the app's lifetime.
#[derive(Default)]
pub struct MultiInstance {
    pub guard: Option<SingletonGuard>,
    pub cookie_lock: Option<CookieLock>,
    pub monitor: Option<Handle64Monitor>,
}

impl MultiInstance {
    /// (Re)applies the configuration. Safe to call repeatedly, e.g. before each launch.
    pub fn apply(&mut self, cfg: &crate::config::MultiInstanceConfig) {
        if !cfg.enabled {
            if self.guard.is_some() || self.monitor.is_some() || self.cookie_lock.is_some() {
                log("[multi] disabled");
            }
            *self = Self::default();
            return;
        }

        if !self.guard.as_ref().is_some_and(|g| g.owned) {
            self.guard = None;
            match SingletonGuard::acquire() {
                Ok(g) if g.owned => {
                    log("[multi] singleton mutex acquired");
                    self.guard = Some(g);
                }
                Ok(g) => {
                    // Roblox already owns it. Drop our reference right away: holding
                    // it would keep the object alive after Roblox exits and block
                    // the next normal launch. We retry on the next apply().
                    drop(g);
                    if !cfg.handle64 {
                        log("[multi] Roblox already owns the singleton; close all Roblox windows, then launch again");
                    }
                }
                Err(e) => log(format!("[multi] {e}")),
            }
        }

        if cfg.error_773_fix {
            if self.cookie_lock.is_none() {
                match CookieLock::acquire() {
                    Ok(l) => {
                        log(format!("[multi] OK: locked {} (prevents Roblox error 773)", l.path.display()));
                        self.cookie_lock = Some(l);
                    }
                    Err(e) => log(format!("[multi] cookie lock for error 773 skipped: {e}")),
                }
            }
        } else {
            self.cookie_lock = None;
        }

        if cfg.handle64 {
            if self.monitor.is_none() {
                if !is_admin() {
                    log("[multi] handle64 mode needs the app to run as administrator");
                } else if let Some(exe) = find_handle64() {
                    self.monitor = Some(Handle64Monitor::start(exe));
                } else {
                    log("[multi] handle64.exe not found (Settings > Download handle64.exe)");
                }
            }
        } else {
            self.monitor = None;
        }
    }

    pub fn mutex_held(&self) -> bool {
        self.guard.as_ref().is_some_and(|g| g.owned)
    }

    pub fn status(&self) -> String {
        let mutex = match &self.guard {
            Some(g) if g.owned => "mutex held",
            Some(_) => "mutex not held",
            None => "mutex not held",
        };
        let lock = if self.cookie_lock.is_some() { "cookie lock on" } else { "cookie lock off" };
        let mon = if self.monitor.is_some() { "handle64 on" } else { "handle64 off" };
        format!("{mutex} | {lock} | {mon}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_handle64_output() {
        let out = "\
RobloxPlayerBeta.exe pid: 1234 type: Event          1A4: \\Sessions\\1\\BaseNamedObjects\\ROBLOX_singletonEvent
  2B0: Mutant        \\Sessions\\1\\BaseNamedObjects\\ROBLOX_singletonMutex
  2C0: Event         \\Sessions\\1\\BaseNamedObjects\\SomethingElse
";
        let h = parse_handles(out);
        assert_eq!(
            h,
            vec![
                ("1A4".to_string(), "ROBLOX_singletonEvent".to_string()),
                ("2B0".to_string(), "ROBLOX_singletonMutex".to_string()),
            ]
        );
    }

    #[test]
    fn mutex_guard_creates_and_releases() {
        let g = SingletonGuard::acquire();
        assert!(g.is_ok());
    }
}
