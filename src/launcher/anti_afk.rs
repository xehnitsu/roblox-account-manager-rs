//! Anti-AFK: taps one key in each Roblox window.
use std::collections::HashSet;
use std::time::Duration;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    MAPVK_VK_TO_VSC, MOUSEEVENTF_MOVE, MOUSEINPUT, MapVirtualKeyW, SendInput, VIRTUAL_KEY, VK_RIGHT, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindowVisible, PostMessageW, SW_MINIMIZE,
    SW_RESTORE, SetForegroundWindow, ShowWindow, WA_ACTIVE, WA_INACTIVE, WM_ACTIVATE, WM_KEYDOWN, WM_KEYUP,
};
use windows::core::BOOL;

use crate::config::{AfkKey, AfkMode};

fn vk(key: AfkKey) -> (VIRTUAL_KEY, bool) {
    match key {
        AfkKey::Space => (VK_SPACE, false),
        AfkKey::W => (VIRTUAL_KEY(0x57), false),
        AfkKey::RightArrow => (VK_RIGHT, true),
    }
}

fn key_input(vk: VIRTUAL_KEY, up: bool, extended: bool) -> INPUT {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    if up {
        flags |= KEYEVENTF_KEYUP;
    }
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// A zero-distance mouse move. Windows lets the process that sent the last
/// input event change the foreground window, and this has no visible effect.
fn null_mouse_move() -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: 0, dwFlags: MOUSEEVENTF_MOVE, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn send(inputs: &[INPUT]) {
    // SAFETY: `inputs` is a valid slice of initialized INPUT structs.
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

struct EnumCtx {
    pids: HashSet<u32>,
    found: Vec<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut EnumCtx passed to EnumWindows below and
    // outlives the enumeration.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx) };
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if ctx.pids.contains(&pid) && IsWindowVisible(hwnd).as_bool() {
            ctx.found.push(hwnd);
        }
    }
    BOOL(1)
}

pub(crate) fn windows_for(pids: &HashSet<u32>) -> Vec<HWND> {
    let mut ctx = EnumCtx { pids: pids.clone(), found: Vec::new() };
    // SAFETY: enum_proc only dereferences lparam as EnumCtx for this call.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut EnumCtx as isize));
    }
    ctx.found
}

pub fn nudge(pids: &HashSet<u32>, key: AfkKey, mode: AfkMode) -> usize {
    let hwnds = windows_for(pids);
    if hwnds.is_empty() {
        return 0;
    }
    match mode {
        AfkMode::Background => nudge_background(&hwnds, key),
        AfkMode::Focus => nudge_focus(&hwnds, key),
    }
}

/// lParam for WM_KEYDOWN / WM_KEYUP: repeat count 1, scan code, extended bit,
/// and for key-up the previous-state and transition bits.
pub(crate) fn key_lparam(scan: u32, extended: bool, up: bool) -> LPARAM {
    let mut v: u32 = 1 | (scan << 16);
    if extended {
        v |= 1 << 24;
    }
    if up {
        v |= (1 << 30) | (1 << 31);
    }
    LPARAM(v as i32 as isize)
}

fn nudge_background(hwnds: &[HWND], key: AfkKey) -> usize {
    let (vk, extended) = vk(key);
    // SAFETY: MapVirtualKeyW is a pure lookup. PostMessageW on a stale
    // handle just fails.
    let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) };
    let mut sent = 0;
    for &hwnd in hwnds {
        unsafe {
            // Roblox drops keys while it thinks it is inactive, so tell it it
            // is active for the duration of the key press.
            let _ = PostMessageW(Some(hwnd), WM_ACTIVATE, WPARAM(WA_ACTIVE as usize), LPARAM(0));
            let down = PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(vk.0 as usize), key_lparam(scan, extended, false));
            std::thread::sleep(Duration::from_millis(80));
            let _ = PostMessageW(Some(hwnd), WM_KEYUP, WPARAM(vk.0 as usize), key_lparam(scan, extended, true));
            let _ = PostMessageW(Some(hwnd), WM_ACTIVATE, WPARAM(WA_INACTIVE as usize), LPARAM(0));
            if down.is_ok() {
                sent += 1;
            }
        }
    }
    sent
}

fn nudge_focus(hwnds: &[HWND], key: AfkKey) -> usize {
    let (vk, extended) = vk(key);
    // SAFETY: plain Win32 window calls on handles we just enumerated. A
    // handle that died meanwhile just makes the call fail.
    unsafe {
        let original = GetForegroundWindow();
        for &hwnd in hwnds {
            let was_min = IsIconic(hwnd).as_bool();
            if was_min {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            send(&[null_mouse_move()]);
            let _ = SetForegroundWindow(hwnd);
            std::thread::sleep(Duration::from_millis(120));
            // Never type into whatever else has focus if the switch failed.
            if GetForegroundWindow() == hwnd {
                send(&[key_input(vk, false, extended)]);
                std::thread::sleep(Duration::from_millis(60));
                send(&[key_input(vk, true, extended)]);
                std::thread::sleep(Duration::from_millis(60));
            }
            if was_min {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
        }
        if !original.is_invalid() {
            send(&[null_mouse_move()]);
            let _ = SetForegroundWindow(original);
        }
    }
    hwnds.len()
}
