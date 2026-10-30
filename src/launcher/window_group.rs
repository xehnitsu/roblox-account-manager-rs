//! Tiles Roblox client windows in a borderless grid and moves, minimizes and raises them together.
//! The bar never owns the windows: owning another process's windows merges input handling,
//! so one busy client stalls everything. They become tool windows instead, and the bar shows and raises them itself.

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DEFAULT, DWMWCP_DONOTROUND,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowW, GW_OWNER, GWL_EXSTYLE, GWL_STYLE, GWLP_HWNDPARENT, GetForegroundWindow, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowThreadProcessId, IsIconic, IsWindow,
    IsWindowVisible, SW_HIDE, SW_SHOWNA, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, ShowWindow, ShowWindowAsync,
    WINDOW_EX_STYLE, WINDOW_STYLE, WS_CAPTION, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW, WS_OVERLAPPEDWINDOW, WS_POPUP,
    WS_THICKFRAME,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

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
        let owned = owner(hwnd).is_some();
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

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    // SAFETY: pid is valid for writes. A stale handle leaves it 0.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

pub fn alive(hwnd: HWND, pid: u32) -> bool {
    // SAFETY: IsWindow accepts any handle value.
    let exists = unsafe { IsWindow(Some(hwnd)).as_bool() };
    exists && pid_of(hwnd) == pid
}

pub fn own_window(title: &str) -> Option<HWND> {
    // SAFETY: the HSTRING outlives the call.
    let hwnd = unsafe { FindWindowW(PCWSTR::null(), &HSTRING::from(title)) }.ok()?;
    // SAFETY: GetCurrentProcessId has no preconditions.
    (pid_of(hwnd) == unsafe { GetCurrentProcessId() }).then_some(hwnd)
}

fn owner(hwnd: HWND) -> Option<HWND> {
    // SAFETY: GetWindow on a stale handle just fails.
    unsafe { GetWindow(hwnd, GW_OWNER) }.ok().filter(|o| !o.is_invalid())
}

fn clear_owner(hwnd: HWND) {
    // SAFETY: GetCurrentProcessId has no preconditions.
    let me = unsafe { GetCurrentProcessId() };
    if owner(hwnd).is_some_and(|o| pid_of(o) == me) {
        // SAFETY: setting GWLP_HWNDPARENT to 0 unowns a top-level window.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, 0) };
    }
}

thread_local! {
    static TASKBAR: Option<ITaskbarList> = {
        // SAFETY: COM is set up once for this thread, and the object is
        // created and used only here.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER)
                .ok()
                .filter(|t| t.HrInit().is_ok())
        }
    };
}

/// Marks the window as a tool window while grouped, which drops it from
/// alt-tab, and removes its taskbar button. The window is never hidden, so
/// a client that's still loading can't end up invisible.
pub fn set_grouped(hwnd: HWND, on: bool) {
    // SAFETY: style calls on a stale handle just fail.
    unsafe {
        let ex = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let new = if on { (ex | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW } else { ex & !WS_EX_TOOLWINDOW };
        if new == ex {
            return;
        }
        crate::applog::debug(format!("group: {:#x} grouped {on}", hwnd.0 as isize));
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, new.0 as isize);
        TASKBAR.with(|t| {
            if let Some(t) = t {
                let _ = if on { t.DeleteTab(hwnd) } else { t.AddTab(hwnd) };
            }
        });
    }
}

pub fn hide(hwnds: &[HWND]) {
    for &hwnd in hwnds {
        // SAFETY: ShowWindowAsync on a stale handle just fails.
        unsafe {
            let _ = ShowWindowAsync(hwnd, SW_HIDE);
        }
    }
}

pub fn show_all(hwnds: &[HWND]) {
    for &hwnd in hwnds {
        show(hwnd);
    }
}

pub fn square_corners(hwnd: HWND, square: bool) {
    let pref: DWM_WINDOW_CORNER_PREFERENCE = if square { DWMWCP_DONOTROUND } else { DWMWCP_DEFAULT };
    // SAFETY: the pointer and size describe `pref`, which outlives the call.
    // Windows 10 doesn't know the attribute and just fails.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            (&raw const pref).cast(),
            size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
    }
}

pub fn is_minimized(hwnd: HWND) -> bool {
    // SAFETY: IsIconic on a stale handle returns false.
    unsafe { IsIconic(hwnd).as_bool() }
}

pub fn foreground() -> HWND {
    // SAFETY: no preconditions.
    unsafe { GetForegroundWindow() }
}

pub fn raise_under(top: HWND, hwnds: &[HWND]) {
    for &hwnd in hwnds.iter().filter(|&&h| h != top) {
        // SAFETY: SetWindowPos on a stale handle just fails.
        unsafe {
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS;
            let _ = SetWindowPos(hwnd, Some(top), 0, 0, 0, 0, flags);
        }
    }
}

const FRAME: WINDOW_STYLE = WINDOW_STYLE(WS_CAPTION.0 | WS_THICKFRAME.0);

fn set_frame(hwnd: HWND, on: bool) {
    // SAFETY: style calls on a stale handle just fail.
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
        let new = if on { (style & !WS_POPUP) | WS_OVERLAPPEDWINDOW } else { style & !FRAME };
        if new != style {
            SetWindowLongPtrW(hwnd, GWL_STYLE, new.0 as isize);
            let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, flags);
        }
    }
    square_corners(hwnd, !on);
}

pub fn place(hwnd: HWND, c: Cell) {
    // SAFETY: SetWindowPos on a stale handle just fails.
    unsafe {
        let _ = SetWindowPos(hwnd, None, c.x, c.y, c.w, c.h, SWP_NOACTIVATE | SWP_NOZORDER);
    }
}

/// The work area (screen minus taskbar) of the monitor `hwnd` is mostly on.
pub fn work_area(hwnd: HWND) -> Option<Cell> {
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: info is sized for the call. A stale handle gets the nearest monitor.
    let ok = unsafe { GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info) }.as_bool();
    let r = info.rcWork;
    ok.then_some(Cell { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
}

pub fn rect(hwnd: HWND) -> Cell {
    let mut r = RECT::default();
    // SAFETY: r is valid for writes. A stale handle just fails.
    let _ = unsafe { GetWindowRect(hwnd, &mut r) };
    Cell { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
}

/// Restores, unframes and places each window in its cell without taking
/// focus. Roblox enforces a minimum size, so cells are then spaced by the
/// largest real window size. Returns the cells the windows ended up in.
pub fn tile(hwnds: &[HWND], x: i32, y: i32, width: i32) -> Vec<Cell> {
    let mut cells = grid(hwnds.len(), x, y, width);
    for (&hwnd, &c) in hwnds.iter().zip(&cells) {
        // SAFETY: calls on a stale handle just fail.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        set_frame(hwnd, false);
        place(hwnd, c);
    }
    let Some(&first) = cells.first() else { return cells };
    let (w, h) = hwnds.iter().map(|&h| rect(h)).fold((first.w, first.h), |(w, h), r| (w.max(r.w), h.max(r.h)));
    if (w, h) != (first.w, first.h) {
        let cols = columns(hwnds.len());
        for (i, (&hwnd, c)) in hwnds.iter().zip(&mut cells).enumerate() {
            *c = Cell { x: x + (i % cols) as i32 * w, y: y + (i / cols) as i32 * h, w, h };
            place(hwnd, *c);
        }
    }
    cells
}

pub fn grid_width(cells: &[Cell]) -> i32 {
    let cols = columns(cells.len()) as i32;
    cells.first().map_or(0, |c| c.w * cols)
}

pub fn drifted(hwnds: &[HWND], cells: &[Cell]) -> bool {
    // SAFETY: IsIconic on a stale handle returns false.
    hwnds.iter().zip(cells).any(|(&h, &c)| rect(h) != c || unsafe { IsIconic(h).as_bool() })
}

pub fn release(hwnds: &[HWND]) {
    for &hwnd in hwnds {
        clear_owner(hwnd);
        set_grouped(hwnd, false);
        set_frame(hwnd, true);
        show(hwnd);
    }
}

/// Shows windows that were minimized or hidden.
/// Async so a busy client can't stall the UI.
fn show(hwnd: HWND) {
    // SAFETY: calls on a stale handle just fail.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNOACTIVATE);
        } else if !IsWindowVisible(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_SHOWNA);
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
        assert_eq!(grid_width(&cells), 800);
    }
}
