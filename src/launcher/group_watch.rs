//! Freeze watchdog for a window group. A background thread checks that the
//! UI thread keeps running and that the bar and each client answer messages.
//! When something stalls it logs who holds mouse capture, focus and menu or
//! move mode, which is what usually explains a freeze.
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GUI_INMENUMODE, GUI_INMOVESIZE, GUI_POPUPMENUMODE, GUI_SYSTEMMENUMODE, GUITHREADINFO, GetClipCursor,
    GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
    WM_NULL,
};

use crate::applog::{debug, log};

const POLL: Duration = Duration::from_millis(500);
const UI_STALL: Duration = Duration::from_millis(1500);
const REPLY_MS: u32 = 1000;

fn start_time() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// Milliseconds since start when the UI thread last ran a frame.
static HEARTBEAT: AtomicU64 = AtomicU64::new(0);

/// Called once per UI frame.
pub fn beat() {
    HEARTBEAT.store(start_time().elapsed().as_millis() as u64, Ordering::Relaxed);
}

/// Window handles are kept as isize so they can cross threads.
type Targets = Vec<(String, isize)>;

pub struct Watch {
    stop: Arc<AtomicBool>,
    targets: Arc<Mutex<Targets>>,
}

impl Watch {
    pub fn start() -> Self {
        beat();
        let stop = Arc::new(AtomicBool::new(false));
        let targets = Arc::new(Mutex::new(Vec::new()));
        let (s, t) = (stop.clone(), targets.clone());
        let _ = std::thread::Builder::new().name("group-watch".into()).spawn(move || run(&s, &t));
        Self { stop, targets }
    }

    /// The windows to check, with a name for the log.
    pub fn set_targets(&self, targets: Vec<(String, HWND)>) {
        *self.targets.lock().unwrap() = targets.into_iter().map(|(n, h)| (n, h.0 as isize)).collect();
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

fn answers(h: HWND) -> bool {
    let mut result = 0usize;
    // SAFETY: WM_NULL does nothing; the call times out if the window's
    // thread is busy, and fails on a stale handle.
    let r =
        unsafe { SendMessageTimeoutW(h, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, REPLY_MS, Some(&mut result)) };
    r.0 != 0
}

fn run(stop: &AtomicBool, targets: &Mutex<Targets>) {
    let mut ui_stalled = false;
    let mut hung: HashSet<String> = HashSet::new();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(POLL);
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let age = start_time().elapsed().as_millis() as u64 - HEARTBEAT.load(Ordering::Relaxed);
        if !ui_stalled && age > UI_STALL.as_millis() as u64 {
            ui_stalled = true;
            log(format!("Client group: UI thread stalled for {age} ms"));
            dump(targets);
        } else if ui_stalled && age < POLL.as_millis() as u64 {
            ui_stalled = false;
            log("Client group: UI thread running again");
        }

        let list = targets.lock().unwrap().clone();
        let mut now_hung = HashSet::new();
        for (name, h) in &list {
            // SAFETY: IsWindow accepts any handle value.
            let exists = unsafe { IsWindow(Some(hwnd(*h))) }.as_bool();
            if exists && !answers(hwnd(*h)) {
                now_hung.insert(name.clone());
            }
        }
        for name in now_hung.difference(&hung) {
            log(format!("Client group: {name} not responding"));
        }
        for name in hung.difference(&now_hung) {
            log(format!("Client group: {name} responding again"));
        }
        if now_hung.difference(&hung).next().is_some() {
            dump(targets);
        }
        hung = now_hung;
    }
}

fn flag_names(info: &GUITHREADINFO) -> String {
    let names = [
        (GUI_INMOVESIZE, "move/size"),
        (GUI_INMENUMODE, "menu"),
        (GUI_SYSTEMMENUMODE, "system menu"),
        (GUI_POPUPMENUMODE, "popup menu"),
    ];
    let set: Vec<&str> = names.iter().filter(|(f, _)| info.flags.contains(*f)).map(|&(_, n)| n).collect();
    if set.is_empty() { "none".into() } else { set.join(", ") }
}

/// Logs input state for each target's thread and the foreground window.
fn dump(targets: &Mutex<Targets>) {
    let mut list = targets.lock().unwrap().clone();
    // SAFETY: no preconditions.
    let fg = unsafe { GetForegroundWindow() };
    list.push(("foreground".into(), fg.0 as isize));
    let mut seen = HashSet::new();
    for (name, h) in list {
        let mut pid = 0u32;
        // SAFETY: pid is valid for writes; a stale handle returns 0.
        let tid = unsafe { GetWindowThreadProcessId(hwnd(h), Some(&mut pid)) };
        if tid == 0 || !seen.insert(tid) {
            continue;
        }
        let mut info = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        // SAFETY: info is sized for the call.
        if unsafe { GetGUIThreadInfo(tid, &mut info) }.is_err() {
            debug(format!("  {name}: pid {pid} thread {tid}, no GUI info"));
            continue;
        }
        debug(format!(
            "  {name}: pid {pid} thread {tid} window {h:#x}, modes {}, active {:?}, focus {:?}, capture {:?}, menu owner {:?}, moving {:?}",
            flag_names(&info),
            info.hwndActive.0,
            info.hwndFocus.0,
            info.hwndCapture.0,
            info.hwndMenuOwner.0,
            info.hwndMoveSize.0,
        ));
    }
    let mut clip = RECT::default();
    // SAFETY: clip is valid for writes.
    if unsafe { GetClipCursor(&mut clip) }.is_ok() {
        debug(format!("  cursor clip: {},{} to {},{}", clip.left, clip.top, clip.right, clip.bottom));
    }
}
