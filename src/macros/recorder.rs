//! Records input with low-level keyboard and mouse hooks and listens for the
//! global hotkeys (F8 record and F9 play by default). Both run on one thread with its own
//! message loop. Hooks are installed only while recording.
//!
//! Only input into a Roblox window is recorded: clicks outside its client area,
//! injected input (including our own playback), the Windows key and system
//! shortcuts like Alt+Tab are left out.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
    QueryFullProcessImageNameW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey, VIRTUAL_KEY, VK_APPS,
    VK_CONTROL, VK_ESCAPE, VK_F4, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT, VK_TAB,
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
/// Last foreground process checked, and whether it's Roblox.
static ROBLOX_PID: Mutex<(u32, bool)> = Mutex::new((0, false));
static CTX: OnceLock<egui::Context> = OnceLock::new();
static RECORDING: Mutex<Option<Recording>> = Mutex::new(None);

struct Recording {
    start: Instant,
    events: Vec<Event>,
    last_move: u32,
    /// Keys from a hotkey press whose releases are left out too.
    swallowed: HashSet<u16>,
}

/// Left and right Shift, Ctrl and Alt, as the keyboard hook reports them.
const MODIFIERS: std::ops::RangeInclusive<u16> = 0xA0..=0xA5;

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
    *RECORDING.lock().unwrap() =
        Some(Recording { start: Instant::now(), events: Vec::new(), last_move: 0, swallowed: HashSet::new() });
    post(START);
}

/// Stops recording and returns the events, timed from the first one.
pub fn finish() -> Vec<Event> {
    post(STOP);
    let events = RECORDING.lock().unwrap().take().map(|r| r.events).unwrap_or_default();
    let mut events = drop_unpaired(events);
    let first = events.first().map_or(0, |e| e.t);
    for e in &mut events {
        e.t -= first;
    }
    events
}

/// Drops presses that were never released and releases that were never
/// pressed, e.g. the hotkey's modifiers or an Alt that switched windows.
/// Playback would otherwise leave those keys stuck or send stray releases.
fn drop_unpaired(events: Vec<Event>) -> Vec<Event> {
    #[derive(PartialEq, Eq, Hash)]
    enum Id {
        Key(u16),
        Button(Button),
    }
    let id = |k: &Kind| match *k {
        Kind::Key { vk, up, .. } => Some((Id::Key(vk), up)),
        Kind::Button { button, up, .. } => Some((Id::Button(button), up)),
        _ => None,
    };
    let mut keep = vec![true; events.len()];
    let mut down = HashSet::new();
    for (i, e) in events.iter().enumerate() {
        match id(&e.kind) {
            Some((k, false)) => {
                down.insert(k);
            }
            Some((k, true)) => keep[i] = down.remove(&k),
            None => {}
        }
    }
    let mut released = HashSet::new();
    for (i, e) in events.iter().enumerate().rev() {
        match id(&e.kind) {
            Some((k, true)) if keep[i] => {
                released.insert(k);
            }
            Some((k, false)) => keep[i] = released.contains(&k),
            _ => {}
        }
    }
    events.into_iter().zip(keep).filter(|&(_, k)| k).map(|(e, _)| e).collect()
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

/// The foreground window, if it belongs to a Roblox client.
fn roblox_window() -> Option<HWND> {
    // SAFETY: read-only window and process queries; the handle is closed.
    unsafe {
        let hwnd = GetForegroundWindow();
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if hwnd.is_invalid() || pid == 0 {
            return None;
        }
        let mut cache = ROBLOX_PID.lock().unwrap();
        if cache.0 != pid {
            let mut is = false;
            if let Ok(h) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                let mut buf = [0u16; 512];
                let mut len = buf.len() as u32;
                let path = windows::core::PWSTR(buf.as_mut_ptr());
                if QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, path, &mut len).is_ok() {
                    let path = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                    is = path.ends_with(&crate::launcher::PLAYER_EXE.to_lowercase());
                }
                let _ = CloseHandle(h);
            }
            *cache = (pid, is);
        }
        cache.1.then_some(hwnd)
    }
}

fn held(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: no preconditions.
    unsafe { GetAsyncKeyState(i32::from(vk.0)) < 0 }
}

/// Keys that belong to Windows rather than the game: the Windows and menu
/// keys, anything pressed with Windows held, Alt+Tab, Alt+Esc, Alt+F4 and
/// Ctrl+Esc.
fn system_key(vk: u16) -> bool {
    let vk = VIRTUAL_KEY(vk);
    if [VK_LWIN, VK_RWIN, VK_APPS].contains(&vk) || held(VK_LWIN) || held(VK_RWIN) {
        return true;
    }
    (held(VK_MENU) && [VK_TAB, VK_ESCAPE, VK_F4].contains(&vk)) || (held(VK_CONTROL) && vk == VK_ESCAPE)
}

/// Whether `vk` with the modifiers held right now is one of our hotkeys.
fn is_hotkey(vk: u16) -> bool {
    let mods = (if held(VK_MENU) { ALT } else { 0 })
        | (if held(VK_CONTROL) { CTRL } else { 0 })
        | (if held(VK_SHIFT) { SHIFT } else { 0 });
    let now = Hotkey { mods, vk };
    [&RECORD_KEY, &PLAY_KEY].iter().any(|k| Hotkey::unpack(k.load(Ordering::Relaxed)) == now)
}

/// A hotkey was pressed: takes the modifiers held for it back out of the
/// recording and leaves out the releases of them and of the key itself.
/// Modifiers pressed on their own are recorded as usual.
fn swallow_hotkey(vk: u16) {
    let Ok(mut guard) = RECORDING.try_lock() else { return };
    let Some(rec) = guard.as_mut() else { return };
    rec.swallowed.insert(vk);
    for m in MODIFIERS {
        let is_m = |e: &Event| matches!(e.kind, Kind::Key { vk, .. } if vk == m);
        let is_up = |e: &Event| matches!(e.kind, Kind::Key { up: true, .. });
        // The presses since its last release, including key repeats.
        let since = rec.events.iter().rposition(|e| is_m(e) && is_up(e)).map_or(0, |i| i + 1);
        let before = rec.events.len();
        let mut i = 0;
        rec.events.retain(|e| {
            i += 1;
            i <= since || !is_m(e)
        });
        if rec.events.len() != before {
            rec.swallowed.insert(m);
        }
    }
}

fn take_swallowed(vk: u16) -> bool {
    let Ok(mut guard) = RECORDING.try_lock() else { return false };
    guard.as_mut().is_some_and(|rec| rec.swallowed.remove(&vk))
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
        let injected = (info.flags & LLKHF_INJECTED).0 != 0;
        let hotkey = !up && !injected && is_hotkey(vk);
        if hotkey {
            swallow_hotkey(vk);
        }
        // Other releases always count; unmatched ones are dropped when recording ends.
        let skip = hotkey || (!up && system_key(vk)) || (up && take_swallowed(vk));
        if !skip && !injected && roblox_window().is_some() {
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
            && let Some(hwnd) = roblox_window()
        {
            let (x, y) = to_fraction(hwnd, info.pt);
            let inside = (0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y);
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
            // Outside the client area only releases count, so a drag that
            // ends outside doesn't leave the button held.
            if let Some(kind) = kind
                && (inside || matches!(kind, Kind::Button { up: true, .. }))
            {
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

    #[test]
    fn hotkey_modifiers_swallowed() {
        let key = |t, vk, up| Event { t, kind: Kind::Key { vk, scan: 0, extended: false, up } };
        // Shift tapped on its own, then Ctrl held (with a repeat) for Ctrl+R.
        let events = vec![key(0, 0xA0, false), key(1, 0xA0, true), key(2, 0xA2, false), key(3, 0xA2, false)];
        *RECORDING.lock().unwrap() =
            Some(Recording { start: Instant::now(), events, last_move: 0, swallowed: HashSet::new() });
        swallow_hotkey(0x52);
        assert!(take_swallowed(0xA2));
        assert!(take_swallowed(0x52));
        assert!(!take_swallowed(0xA0));
        let kept: Vec<u32> = RECORDING.lock().unwrap().take().unwrap().events.iter().map(|e| e.t).collect();
        assert_eq!(kept, vec![0, 1]);
    }

    #[test]
    fn unpaired_presses_dropped() {
        let key = |t, vk, up| Event { t, kind: Kind::Key { vk, scan: 0, extended: false, up } };
        // Ctrl let go from the start hotkey, A pressed and released, Alt
        // pressed but released in another window.
        let events = vec![key(0, 0x11, true), key(1, 0x41, false), key(2, 0x41, true), key(3, 0x12, false)];
        let kept: Vec<u32> = drop_unpaired(events).iter().map(|e| e.t).collect();
        assert_eq!(kept, vec![1, 2]);
    }
}
