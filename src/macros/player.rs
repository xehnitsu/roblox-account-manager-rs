//! Plays a macro back on its own thread, with real input (`SendInput`).
//! Roblox ignores input sent to a window that isn't in front, so there is no
//! way to drive several Roblox windows at once; this plays the macro in one
//! window at a time instead.
//!
//! - Screen: plays once, on whatever window is currently in front.
//! - EachWindow: brings each of `pids`'s windows to the front in turn, plays
//!   the macro there, then moves to the next. The previously active window
//!   is restored once every window has had its turn.
//!
//! Keys and buttons still held when playback stops are released.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetSystemMetrics, IsIconic, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
    SM_YVIRTUALSCREEN, SW_MINIMIZE, SW_RESTORE, SetForegroundWindow, ShowWindow,
};

use super::{Button, Kind, Macro};
use crate::launcher::anti_afk::windows_for;
use crate::macros::recorder;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Screen,
    EachWindow,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub mode: Mode,
    /// 0 repeats until stopped.
    pub loops: u32,
    pub speed: f32,
}

/// A running playback. Dropping it does not stop it, call `stop`.
pub struct Playback {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    run: Arc<AtomicU32>,
}

impl Playback {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    /// Which repeat is playing, from 1.
    pub fn run(&self) -> u32 {
        self.run.load(Ordering::Relaxed)
    }
}

/// Starts playing `m`. In `EachWindow` mode it cycles through the windows of
/// `pids`, one at a time; `pids` is unused in `Screen` mode.
pub fn play(m: Macro, opts: Options, pids: HashSet<u32>, ctx: egui::Context) -> Playback {
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    let run = Arc::new(AtomicU32::new(0));
    let playback = Playback { stop: stop.clone(), done: done.clone(), run: run.clone() };
    std::thread::spawn(move || {
        let speed = opts.speed.clamp(0.1, 10.0);
        let mut n = 0;
        // SAFETY: a read-only window query.
        let original = unsafe { GetForegroundWindow() };
        'outer: while opts.loops == 0 || n < opts.loops {
            n += 1;
            run.store(n, Ordering::Relaxed);
            ctx.request_repaint();
            match opts.mode {
                Mode::Screen => {
                    if !play_once(&m, speed, &stop, None) {
                        break 'outer;
                    }
                }
                Mode::EachWindow => {
                    let hwnds = windows_for(&pids);
                    if hwnds.is_empty() {
                        break;
                    }
                    for hwnd in hwnds {
                        if stop.load(Ordering::Relaxed) {
                            break 'outer;
                        }
                        if !with_focus(hwnd, || play_once(&m, speed, &stop, Some(hwnd))) {
                            break 'outer;
                        }
                    }
                }
            }
        }
        if opts.mode == Mode::EachWindow && !original.is_invalid() {
            // SAFETY: restoring whatever window had focus before playback.
            unsafe {
                let _ = SetForegroundWindow(original);
            }
        }
        done.store(true, Ordering::Relaxed);
        ctx.request_repaint();
    });
    playback
}

/// Plays every event of `m` once, at `speed`, into `target` (or, when None,
/// the first window other than this app that is in front). Returns false if
/// `stop` fired or another window came to the front partway through; the
/// latter also sets `stop`, so the macro never types into the wrong window.
fn play_once(m: &Macro, speed: f32, stop: &AtomicBool, mut target: Option<HWND>) -> bool {
    let mut held = Held::default();
    let start = Instant::now();
    for e in &m.events {
        let due = start + Duration::from_secs_f32(e.t as f32 / 1000.0 / speed);
        while Instant::now() < due {
            if stop.load(Ordering::Relaxed) {
                held.release();
                return false;
            }
            std::thread::sleep((due - Instant::now()).min(Duration::from_millis(15)));
        }
        // SAFETY: a read-only window query.
        let front = unsafe { GetForegroundWindow() };
        match target {
            Some(t) if t != front => {
                held.release();
                stop.store(true, Ordering::Relaxed);
                crate::applog::log("Macro stopped: another window came to the front");
                return false;
            }
            None => target = recorder::target_window(),
            _ => {}
        }
        held.track(e.kind);
        send(e.kind);
    }
    held.release();
    true
}

/// Brings `hwnd` to the front, runs `f`, then restores its minimized state.
/// Does nothing (and `f` plays wherever focus already is) if the switch fails.
fn with_focus(hwnd: HWND, f: impl FnOnce() -> bool) -> bool {
    // SAFETY: plain window calls; a stale handle just makes them fail.
    unsafe {
        let was_min = IsIconic(hwnd).as_bool();
        if was_min {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let switched = SetForegroundWindow(hwnd).as_bool();
        std::thread::sleep(Duration::from_millis(120));
        // A window that won't come to the front is skipped.
        let result = if switched && GetForegroundWindow() == hwnd { f() } else { true };
        if was_min {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
        result
    }
}

/// Keys and mouse buttons a macro is currently holding down, so they can be
/// released if playback stops, or a window switch happens, partway through.
#[derive(Default)]
struct Held {
    keys: HashSet<(u16, u16, bool)>,
    buttons: HashSet<Button>,
}

impl Held {
    fn track(&mut self, kind: Kind) {
        match kind {
            Kind::Key { vk, scan, extended, up } => {
                if up {
                    self.keys.remove(&(vk, scan, extended));
                } else {
                    self.keys.insert((vk, scan, extended));
                }
            }
            Kind::Button { button, up, .. } => {
                if up {
                    self.buttons.remove(&button);
                } else {
                    self.buttons.insert(button);
                }
            }
            _ => {}
        }
    }

    fn release(&mut self) {
        for (vk, scan, extended) in std::mem::take(&mut self.keys) {
            send(Kind::Key { vk, scan, extended, up: true });
        }
        for button in std::mem::take(&mut self.buttons) {
            send(Kind::Button { button, up: true, x: -1.0, y: -1.0 });
        }
    }
}

fn send(mut kind: Kind) {
    // Never click or type into this app, e.g. right after pressing Play. Key
    // and button releases still go out so nothing stays held down.
    if recorder::target_window().is_none() {
        match &mut kind {
            Kind::Key { up: true, .. } => {}
            Kind::Button { up: true, x, .. } => *x = -1.0,
            _ => return,
        }
    }
    let input = match kind {
        Kind::Key { vk, scan, extended, up } => {
            // Games read scan codes, so send those when the key has one.
            let mut flags = if scan != 0 { KEYEVENTF_SCANCODE } else { KEYBD_EVENT_FLAGS(0) };
            if extended {
                flags |= KEYEVENTF_EXTENDEDKEY;
            }
            if up {
                flags |= KEYEVENTF_KEYUP;
            }
            let vk = if scan != 0 { 0 } else { vk };
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
                },
            }
        }
        Kind::Move { x, y } => mouse(x, y, MOUSEEVENTF_MOVE, 0),
        Kind::Button { button, up, x, y } => {
            let flag = match (button, up) {
                (Button::Left, false) => MOUSEEVENTF_LEFTDOWN,
                (Button::Left, true) => MOUSEEVENTF_LEFTUP,
                (Button::Right, false) => MOUSEEVENTF_RIGHTDOWN,
                (Button::Right, true) => MOUSEEVENTF_RIGHTUP,
                (Button::Middle, false) => MOUSEEVENTF_MIDDLEDOWN,
                (Button::Middle, true) => MOUSEEVENTF_MIDDLEUP,
            };
            // A release at the end keeps the cursor where it is.
            if x < 0.0 { mouse_in_place(flag) } else { mouse(x, y, flag | MOUSEEVENTF_MOVE, 0) }
        }
        Kind::Wheel { delta, x, y } => mouse(x, y, MOUSEEVENTF_WHEEL | MOUSEEVENTF_MOVE, delta as i32),
    };
    // SAFETY: one initialized INPUT.
    unsafe {
        SendInput(&[input], std::mem::size_of::<INPUT>() as i32);
    }
}

/// Mouse input at a fraction of the foreground window's client area.
fn mouse(x: f32, y: f32, flags: MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    // SAFETY: read-only window and screen queries.
    let (dx, dy) = unsafe {
        let hwnd = GetForegroundWindow();
        let mut p = to_fraction_point(hwnd, x, y);
        let _ = ClientToScreen(hwnd, &mut p);
        let (vx, vy) = (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_YVIRTUALSCREEN));
        let (vw, vh) = (GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1), GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1));
        (((p.x - vx) * 65535 / vw), ((p.y - vy) * 65535 / vh))
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_in_place(flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// Client-area point for a fraction of the window's client size. A negative
/// fraction (a release at the end) uses the window's center.
fn to_fraction_point(hwnd: HWND, x: f32, y: f32) -> windows::Win32::Foundation::POINT {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
    let mut rect = RECT::default();
    // SAFETY: read-only window query.
    let _ = unsafe { GetClientRect(hwnd, &mut rect) };
    let (x, y) = if x < 0.0 { (0.5, 0.5) } else { (x, y) };
    POINT { x: (x * rect.right as f32) as i32, y: (y * rect.bottom as f32) as i32 }
}
