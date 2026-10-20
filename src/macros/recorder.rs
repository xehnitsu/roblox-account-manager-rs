//! Records input with low-level keyboard and mouse hooks, and listens for the
//! global hotkeys (F8 record, F9 play). Both run on one thread with its own
//! message loop. The hooks are only installed while recording.
//!
//! Input injected by other programs (including our own playback) and input
//! while this app's window is in front (e.g. pressing Stop) is not recorded.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_NOREPEAT, RegisterHotKey, VK_F8, VK_F9};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClientRect, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_HOTKEY, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYUP,
};

use super::{Button, Event, Kind};

const START: u32 = WM_APP + 1;
const STOP: u32 = WM_APP + 2;
const HOTKEY_RECORD: i32 = 1;
const HOTKEY_PLAY: i32 = 2;
/// Mouse moves closer together than this are dropped to keep recordings small.
const MOVE_INTERVAL_MS: u32 = 15;

pub const RECORD_PRESSED: u8 = 1;
pub const PLAY_PRESSED: u8 = 2;

static THREAD: AtomicU32 = AtomicU32::new(0);
static HOTKEYS: AtomicU8 = AtomicU8::new(0);
static CTX: OnceLock<egui::Context> = OnceLock::new();
static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

struct Recording {
    start: Instant,
    events: Vec<Event>,
    last_move: u32,
}

/// Starts the hotkey and recording thread. Call once.
pub fn start(ctx: egui::Context) {
    let _ = CTX.set(ctx);
    std::thread::spawn(|| {
        // SAFETY: plain Win32 calls on this thread's own message queue.
        unsafe {
            THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            let _ = RegisterHotKey(None, HOTKEY_RECORD, MOD_NOREPEAT, VK_F8.0 as u32);
            let _ = RegisterHotKey(None, HOTKEY_PLAY, MOD_NOREPEAT, VK_F9.0 as u32);
            let mut hooks: Option<(HHOOK, HHOOK)> = None;
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                match msg.message {
                    WM_HOTKEY => {
                        let flag = if msg.wParam.0 as i32 == HOTKEY_RECORD { RECORD_PRESSED } else { PLAY_PRESSED };
                        HOTKEYS.fetch_or(flag, Ordering::Relaxed);
                        if let Some(ctx) = CTX.get() {
                            ctx.request_repaint();
                        }
                    }
                    START if hooks.is_none() => {
                        let module = GetModuleHandleW(None).ok().map(|m| m.into());
                        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_hook), module, 0);
                        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_hook), module, 0);
                        if let (Ok(k), Ok(m)) = (keyboard, mouse) {
                            hooks = Some((k, m));
                        }
                    }
                    STOP => {
                        if let Some((k, m)) = hooks.take() {
                            let _ = UnhookWindowsHookEx(k);
                            let _ = UnhookWindowsHookEx(m);
                        }
                    }
                    _ => {}
                }
            }
        }
    });
}

/// Hotkeys pressed since the last call (`RECORD_PRESSED`, `PLAY_PRESSED`).
pub fn take_hotkeys() -> u8 {
    HOTKEYS.swap(0, Ordering::Relaxed)
}

pub fn begin() {
    *RECORDING.lock().unwrap() = Some(Recording { start: Instant::now(), events: Vec::new(), last_move: 0 });
    post(START);
}

/// Stops recording and returns the events, timed from the first one.
pub fn finish() -> Vec<Event> {
    post(STOP);
    let mut events = RECORDING.lock().unwrap().take().map(|r| r.events).unwrap_or_default();
    let first = events.first().map_or(0, |e| e.t);
    for e in &mut events {
        e.t -= first;
    }
    events
}

pub fn is_recording() -> bool {
    RECORDING.lock().unwrap().is_some()
}

pub fn event_count() -> usize {
    RECORDING.lock().unwrap().as_ref().map_or(0, |r| r.events.len())
}

fn post(msg: u32) {
    let thread = THREAD.load(Ordering::Relaxed);
    if thread != 0 {
        // SAFETY: posting to our own thread; a dead thread just fails.
        let _ = unsafe { PostThreadMessageW(thread, msg, WPARAM(0), LPARAM(0)) };
    }
}

fn push(kind: Kind) {
    let Ok(mut guard) = RECORDING.try_lock() else { return };
    let Some(rec) = guard.as_mut() else { return };
    let t = rec.start.elapsed().as_millis() as u32;
    if matches!(kind, Kind::Move { .. }) {
        if t.saturating_sub(rec.last_move) < MOVE_INTERVAL_MS && rec.events.last().is_some() {
            return;
        }
        rec.last_move = t;
    }
    rec.events.push(Event { t, kind });
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// The foreground window, unless it belongs to this app.
pub(super) fn target_window() -> Option<HWND> {
    // SAFETY: read-only window queries.
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (!hwnd.is_invalid() && pid != GetCurrentProcessId()).then_some(hwnd)
    }
}

/// Screen point as a fraction of the window's client area.
pub(super) fn to_fraction(hwnd: HWND, pt: POINT) -> (f32, f32) {
    let mut p = pt;
    let mut rect = RECT::default();
    // SAFETY: read-only window queries.
    unsafe {
        let _ = ScreenToClient(hwnd, &mut p);
        let _ = GetClientRect(hwnd, &mut rect);
    }
    let w = (rect.right - rect.left).max(1) as f32;
    let h = (rect.bottom - rect.top).max(1) as f32;
    (p.x as f32 / w, p.y as f32 / h)
}

unsafe extern "system" fn keyboard_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_KEYBOARD_LL, lparam points to a KBDLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let hotkey = info.vkCode == VK_F8.0 as u32 || info.vkCode == VK_F9.0 as u32;
        if !hotkey && (info.flags & LLKHF_INJECTED).0 == 0 && target_window().is_some() {
            let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
            push(Kind::Key {
                vk: info.vkCode as u16,
                scan: info.scanCode as u16,
                extended: (info.flags & LLKHF_EXTENDED).0 != 0,
                up,
            });
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn mouse_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        // SAFETY: for WH_MOUSE_LL, lparam points to an MSLLHOOKSTRUCT.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if info.flags & LLMHF_INJECTED == 0
            && let Some(hwnd) = target_window()
        {
            let (x, y) = to_fraction(hwnd, info.pt);
            let button = |button, up| Some(Kind::Button { button, up, x, y });
            let kind = match wparam.0 as u32 {
                WM_MOUSEMOVE => Some(Kind::Move { x, y }),
                WM_LBUTTONDOWN => button(Button::Left, false),
                WM_LBUTTONUP => button(Button::Left, true),
                WM_RBUTTONDOWN => button(Button::Right, false),
                WM_RBUTTONUP => button(Button::Right, true),
                WM_MBUTTONDOWN => button(Button::Middle, false),
                WM_MBUTTONUP => button(Button::Middle, true),
                WM_MOUSEWHEEL => Some(Kind::Wheel { delta: (info.mouseData >> 16) as u16 as i16, x, y }),
                _ => None,
            };
            if let Some(kind) = kind {
                push(kind);
            }
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}
