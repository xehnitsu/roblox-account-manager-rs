//! Records input with low-level keyboard and mouse hooks and listens for the
//! global hotkeys (F8 record and F9 play by default). Both run on one thread with its own
//! message loop. Hooks are installed only while recording.
//!
//! Injected input (including our own playback) and input while this app's
//! window is in front (e.g. pressing Stop) are not recorded.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_CONTROL,
    VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetClientRect, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, HHOOK, KBDLLHOOKSTRUCT,
    LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_HOTKEY, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYUP,
};

use super::{Button, Event, Kind};

const START: u32 = WM_APP + 1;
const STOP: u32 = WM_APP + 2;
const REBIND: u32 = WM_APP + 3;
const HOTKEY_RECORD: i32 = 1;
const HOTKEY_PLAY: i32 = 2;
/// Mouse moves closer together than this are dropped to keep recordings small.
const MOVE_INTERVAL_MS: u32 = 15;

pub const RECORD_PRESSED: u8 = 1;
pub const PLAY_PRESSED: u8 = 2;

static THREAD: AtomicU32 = AtomicU32::new(0);
static HOTKEYS: AtomicU8 = AtomicU8::new(0);
/// Hotkeys another app already holds, as RECORD_PRESSED | PLAY_PRESSED bits.
static TAKEN: AtomicU8 = AtomicU8::new(0);
static RECORD_KEY: AtomicU32 = AtomicU32::new(0);
static PLAY_KEY: AtomicU32 = AtomicU32::new(0);
static CTX: OnceLock<egui::Context> = OnceLock::new();
static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

struct Recording {
    start: Instant,
    events: Vec<Event>,
    last_move: u32,
}

/// A key plus modifiers, like Ctrl+Shift+R. `mods` uses the RegisterHotKey
/// bits (ALT, CTRL, SHIFT).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub mods: u8,
    pub vk: u16,
}

pub const ALT: u8 = 1;
pub const CTRL: u8 = 2;
pub const SHIFT: u8 = 4;

impl Hotkey {
    pub const NONE: Hotkey = Hotkey { mods: 0, vk: 0 };

    pub fn from_egui(key: egui::Key, m: egui::Modifiers) -> Option<Self> {
        let mods = (if m.alt { ALT } else { 0 }) | (if m.ctrl { CTRL } else { 0 }) | (if m.shift { SHIFT } else { 0 });
        Some(Hotkey { mods, vk: vk_of(key)? })
    }

    pub fn name(self) -> String {
        let mut parts: Vec<String> = [(CTRL, "Ctrl"), (SHIFT, "Shift"), (ALT, "Alt")]
            .into_iter()
            .filter(|&(bit, _)| self.mods & bit != 0)
            .map(|(_, n)| n.to_owned())
            .collect();
        parts.push(key_name(self.vk));
        parts.join("+")
    }

    fn pack(self) -> u32 {
        (u32::from(self.mods) << 16) | u32::from(self.vk)
    }

    fn unpack(v: u32) -> Self {
        Hotkey { mods: (v >> 16) as u8, vk: v as u16 }
    }
}

pub fn start(ctx: egui::Context, record: Hotkey, play: Hotkey) {
    let _ = CTX.set(ctx);
    RECORD_KEY.store(record.pack(), Ordering::Relaxed);
    PLAY_KEY.store(play.pack(), Ordering::Relaxed);
    std::thread::spawn(|| {
        // SAFETY: plain Win32 calls on this thread's own message queue.
        unsafe {
            THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
            register_hotkeys();
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
                    REBIND => register_hotkeys(),
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

/// Registers the current keys on the recorder thread. A key of 0 is left
/// unregistered.
fn register_hotkeys() {
    let mut taken = 0;
    for (id, key, flag) in [(HOTKEY_RECORD, &RECORD_KEY, RECORD_PRESSED), (HOTKEY_PLAY, &PLAY_KEY, PLAY_PRESSED)] {
        let key = Hotkey::unpack(key.load(Ordering::Relaxed));
        let mods = HOT_KEY_MODIFIERS(u32::from(key.mods)) | MOD_NOREPEAT;
        // SAFETY: hotkeys belong to this thread; unregistering a missing one just fails.
        unsafe {
            let _ = UnregisterHotKey(None, id);
            if key.vk != 0 && RegisterHotKey(None, id, mods, u32::from(key.vk)).is_err() {
                taken |= flag;
            }
        }
    }
    TAKEN.store(taken, Ordering::Relaxed);
}

/// Swaps the hotkeys. Pass `Hotkey::NONE` for both to pause them, e.g. while
/// picking a new key.
pub fn set_keys(record: Hotkey, play: Hotkey) {
    RECORD_KEY.store(record.pack(), Ordering::Relaxed);
    PLAY_KEY.store(play.pack(), Ordering::Relaxed);
    post(REBIND);
}

pub fn taken() -> u8 {
    TAKEN.load(Ordering::Relaxed)
}

/// Virtual-key code for an egui key, for the keys that can be hotkeys.
pub fn vk_of(key: egui::Key) -> Option<u16> {
    let name = key.name();
    if let Some(n) = name.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return (1..=24).contains(&n).then_some(0x6F + n);
    }
    if let [c] = name.as_bytes()
        && c.is_ascii_alphanumeric()
    {
        return Some(u16::from(c.to_ascii_uppercase()));
    }
    Some(match key {
        egui::Key::Insert => 0x2D,
        egui::Key::Delete => 0x2E,
        egui::Key::Home => 0x24,
        egui::Key::End => 0x23,
        egui::Key::PageUp => 0x21,
        egui::Key::PageDown => 0x22,
        _ => return None,
    })
}

pub fn key_name(vk: u16) -> String {
    egui::Key::ALL
        .iter()
        .find(|&&k| vk_of(k) == Some(vk))
        .map_or_else(|| format!("key {vk:#x}"), |k| k.name().to_owned())
}

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

pub(super) fn target_window() -> Option<HWND> {
    // SAFETY: read-only window queries.
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        (!hwnd.is_invalid() && pid != GetCurrentProcessId()).then_some(hwnd)
    }
}

fn held(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: no preconditions.
    unsafe { GetAsyncKeyState(i32::from(vk.0)) < 0 }
}

/// Whether `vk` with the modifiers held right now is one of our hotkeys.
fn is_hotkey(vk: u16) -> bool {
    let mods = (if held(VK_MENU) { ALT } else { 0 })
        | (if held(VK_CONTROL) { CTRL } else { 0 })
        | (if held(VK_SHIFT) { SHIFT } else { 0 });
    let now = Hotkey { mods, vk };
    [&RECORD_KEY, &PLAY_KEY].iter().any(|k| Hotkey::unpack(k.load(Ordering::Relaxed)) == now)
}

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
        let vk = info.vkCode as u16;
        let up = matches!(wparam.0 as u32, WM_KEYUP | WM_SYSKEYUP);
        let skip = !up && is_hotkey(vk);
        if !skip && (info.flags & LLKHF_INJECTED).0 == 0 && target_window().is_some() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_codes() {
        assert_eq!(vk_of(egui::Key::F8), Some(0x77));
        assert_eq!(vk_of(egui::Key::F24), Some(0x87));
        assert_eq!(vk_of(egui::Key::A), Some(0x41));
        assert_eq!(vk_of(egui::Key::Num5), Some(0x35));
        assert_eq!(vk_of(egui::Key::Escape), None);
        assert_eq!(key_name(0x78), "F9");
        let mods = egui::Modifiers { ctrl: true, shift: true, ..Default::default() };
        let h = Hotkey::from_egui(egui::Key::R, mods).unwrap();
        assert_eq!(h, Hotkey { mods: CTRL | SHIFT, vk: 0x52 });
        assert_eq!(h.name(), "Ctrl+Shift+R");
        assert_eq!(Hotkey::unpack(h.pack()), h);
    }
}
