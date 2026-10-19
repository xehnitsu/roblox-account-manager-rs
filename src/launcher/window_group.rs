//! Tiles Roblox client windows in a grid and moves or minimizes them together.
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic,
    IsWindowVisible, SW_SHOWMINNOACTIVE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos, ShowWindow,
};
use windows::core::BOOL;

/// Height of each cell relative to its width.
const ASPECT: f32 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

pub fn columns(n: usize) -> usize {
    (n as f32).sqrt().ceil().max(1.0) as usize
}

/// Cells for `n` windows, filled row by row from (`x`, `y`), spanning `width`.
pub fn grid(n: usize, x: i32, y: i32, width: i32) -> Vec<Cell> {
    let cols = columns(n);
    let w = width / cols as i32;
    let h = (w as f32 * ASPECT).round() as i32;
    (0..n).map(|i| Cell { x: x + (i % cols) as i32 * w, y: y + (i / cols) as i32 * h, w, h }).collect()
}

struct MainCtx {
    pid: u32,
    best: Option<(HWND, i64)>,
}

unsafe extern "system" fn main_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam is the &mut MainCtx passed to EnumWindows below and
    // outlives the enumeration.
    let ctx = unsafe { &mut *(lparam.0 as *mut MainCtx) };
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|o| !o.is_invalid());
        if pid != ctx.pid || owned || !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return BOOL(1);
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_ok() {
            let area = i64::from(r.right - r.left) * i64::from(r.bottom - r.top);
            if ctx.best.is_none_or(|(_, a)| area > a) {
                ctx.best = Some((hwnd, area));
            }
        }
    }
    BOOL(1)
}

/// The client's game window: its largest visible, titled, unowned window.
pub fn main_window(pid: u32) -> Option<HWND> {
    let mut ctx = MainCtx { pid, best: None };
    // SAFETY: main_proc only dereferences lparam as MainCtx for this call.
    unsafe {
        let _ = EnumWindows(Some(main_proc), LPARAM(&mut ctx as *mut MainCtx as isize));
    }
    ctx.best.map(|(h, _)| h)
}

/// Width of the invisible resize border on each side, so visible edges meet.
fn invisible_border(hwnd: HWND) -> RECT {
    let mut outer = RECT::default();
    let mut visible = RECT::default();
    // SAFETY: both RECTs are valid for writes; a stale handle just fails.
    let ok = unsafe {
        GetWindowRect(hwnd, &mut outer).is_ok()
            && DwmGetWindowAttribute(
                hwnd,
                DWMWA_EXTENDED_FRAME_BOUNDS,
                &mut visible as *mut RECT as *mut _,
                std::mem::size_of::<RECT>() as u32,
            )
            .is_ok()
    };
    if !ok {
        return RECT::default();
    }
    RECT {
        left: visible.left - outer.left,
        top: visible.top - outer.top,
        right: outer.right - visible.right,
        bottom: outer.bottom - visible.bottom,
    }
}

/// Restores and places each window in its cell without taking focus.
pub fn tile(hwnds: &[HWND], x: i32, y: i32, width: i32) {
    for (&hwnd, c) in hwnds.iter().zip(grid(hwnds.len(), x, y, width)) {
        // SAFETY: calls on a stale handle just fail.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            let b = invisible_border(hwnd);
            let _ = SetWindowPos(
                hwnd,
                None,
                c.x - b.left,
                c.y - b.top,
                c.w + b.left + b.right,
                c.h + b.top + b.bottom,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
    }
}

pub fn minimize(hwnds: &[HWND]) {
    for &hwnd in hwnds {
        // SAFETY: ShowWindow on a stale handle just fails.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_fills_rows() {
        assert_eq!(columns(1), 1);
        assert_eq!(columns(4), 2);
        assert_eq!(columns(5), 3);
        let cells = grid(3, 10, 20, 800);
        assert_eq!(cells[0], Cell { x: 10, y: 20, w: 400, h: 300 });
        assert_eq!(cells[1], Cell { x: 410, y: 20, w: 400, h: 300 });
        assert_eq!(cells[2], Cell { x: 10, y: 320, w: 400, h: 300 });
    }
}
