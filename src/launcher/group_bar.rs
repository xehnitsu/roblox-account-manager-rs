//! Hooks the group bar's own window. The client windows follow the bar from
//! inside Windows' move loop, where egui doesn't get to run. They hide while
//! the bar is minimized and come to the front when it's activated. Alt-tab
//! gets a picture of the grid instead of the thin bar.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_FORCE_ICONIC_REPRESENTATION, DWMWA_HAS_ICONIC_BITMAP, DWMWINDOWATTRIBUTE, DwmSetIconicThumbnail,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject,
    GdiFlush, GetDC, HALFTONE, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SRCCOPY, SelectObject, SetStretchBltMode, StretchBlt,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    IsIconic, IsWindowVisible, SC_MINIMIZE, SIZE_MINIMIZED, SWP_NOMOVE, SWP_NOSIZE, WINDOWPOS, WM_ACTIVATE,
    WM_CAPTURECHANGED, WM_CONTEXTMENU, WM_DWMSENDICONICTHUMBNAIL, WM_ENTERMENULOOP, WM_ENTERSIZEMOVE, WM_EXITMENULOOP,
    WM_EXITSIZEMOVE, WM_NCDESTROY, WM_NCRBUTTONDOWN, WM_NCRBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SIZE,
    WM_SYSCOMMAND, WM_WINDOWPOSCHANGED,
};
use windows::core::BOOL;

use super::group_worker::{self, Moves};
use super::window_group::{self as wg, Cell, grid, rect};
use crate::applog::debug;

const SUBCLASS_ID: usize = 0x6772_6f75; // "grou"
/// Width the cached grid picture is scaled down to.
const SNAPSHOT_WIDTH: i32 = 960;

/// Window handles are stored as isize so the state can live in a static.
struct State {
    members: Vec<isize>,
    /// Each window's cell relative to the bar's bottom-left corner.
    offsets: Vec<Cell>,
    /// Bar width the offsets were made for.
    width: i32,
    snapshot: Option<Snapshot>,
    bar: isize,
    /// Largest preview alt-tab asked for last time.
    preview_size: (i32, i32),
}

/// Top-down BGRA pixels.
struct Snapshot {
    w: i32,
    h: i32,
    pixels: Vec<u8>,
}

impl Snapshot {
    /// Average of all color channels, 0 to 255.
    fn brightness(&self) -> u64 {
        let px = self.pixels.as_chunks::<4>().0;
        let sum: u64 = px.iter().map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2])).sum();
        sum / (px.len() as u64 * 3).max(1)
    }

    /// Writes the picture to `group-preview.bmp` in the data folder, so the
    /// preview can be checked by eye.
    fn save(&self) {
        let size = self.pixels.len() as u32;
        let mut bmp = Vec::with_capacity(54 + size as usize);
        bmp.extend_from_slice(b"BM");
        for v in [54 + size, 0, 54, 40] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&self.w.to_le_bytes());
        bmp.extend_from_slice(&(-self.h).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&32u16.to_le_bytes());
        for v in [0u32, size, 2835, 2835, 0, 0] {
            bmp.extend_from_slice(&v.to_le_bytes());
        }
        bmp.extend_from_slice(&self.pixels);
        let _ = std::fs::write(crate::config::data_dir().join("group-preview.bmp"), bmp);
    }
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// True while the user drags the bar.
static MOVING: AtomicBool = AtomicBool::new(false);
static MINIMIZED: AtomicBool = AtomicBool::new(false);

/// True while Windows' move loop runs for the bar. The UI leaves the
/// windows alone then, so it doesn't fight the drag.
pub fn moving() -> bool {
    MOVING.load(Ordering::Relaxed)
}

fn members() -> Vec<HWND> {
    STATE
        .try_lock()
        .ok()
        .and_then(|s| s.as_ref().map(|s| s.members.iter().map(|&h| hwnd(h)).collect()))
        .unwrap_or_default()
}

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

fn set_bool(bar: HWND, attr: DWMWINDOWATTRIBUTE) {
    let on = BOOL(1);
    // SAFETY: the pointer and size describe `on`, which outlives the call.
    unsafe {
        let _ = DwmSetWindowAttribute(bar, attr, (&raw const on).cast(), size_of::<BOOL>() as u32);
    }
}

pub fn install(bar: HWND) {
    *STATE.lock().unwrap() = Some(State {
        members: Vec::new(),
        offsets: Vec::new(),
        width: 0,
        snapshot: None,
        bar: bar.0 as isize,
        preview_size: (200, 113),
    });
    set_bool(bar, DWMWA_FORCE_ICONIC_REPRESENTATION);
    set_bool(bar, DWMWA_HAS_ICONIC_BITMAP);
    // SAFETY: called on the UI thread that owns `bar`; the hook removes
    // itself when the window is destroyed.
    unsafe {
        let _ = SetWindowSubclass(bar, Some(bar_proc), SUBCLASS_ID, 0);
    }
}

pub fn uninstall(bar: HWND) {
    // SAFETY: removing a hook that isn't there just fails.
    unsafe {
        let _ = RemoveWindowSubclass(bar, Some(bar_proc), SUBCLASS_ID);
    }
    *STATE.lock().unwrap() = None;
}

/// Records where the windows were tiled, relative to the bar.
pub fn update(members: &[HWND], cells: &[Cell], bar: Cell) {
    if let Some(s) = STATE.lock().unwrap().as_mut() {
        s.members = members.iter().map(|h| h.0 as isize).collect();
        let (x, y) = (bar.x, bar.y + bar.h);
        s.offsets = cells.iter().map(|c| Cell { x: c.x - x, y: c.y - y, ..*c }).collect();
        s.width = bar.w;
    }
}

/// Saves a picture of the grid while it's on screen and hands it to DWM
/// right away, so alt-tab never has to wait for one or show black.
pub fn snapshot() {
    let Ok(mut st) = STATE.try_lock() else { return };
    if let Some(s) = st.as_mut() {
        let members: Vec<HWND> = s.members.iter().map(|&h| hwnd(h)).collect();
        if let Some(snap) = capture(&members) {
            let (w, h) = s.preview_size;
            let _ = push(hwnd(s.bar), &snap, w, h);
            s.snapshot = Some(snap);
        }
    }
}

unsafe extern "system" fn bar_proc(
    bar: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    if let Some(name) = notable(msg) {
        debug(format!("group: bar got {name} (wparam {:#x})", wparam.0));
    }
    match msg {
        WM_WINDOWPOSCHANGED => {
            // SAFETY: for this message lparam points at a WINDOWPOS.
            let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            let moved = !pos.flags.contains(SWP_NOMOVE) || !pos.flags.contains(SWP_NOSIZE);
            // SAFETY: `bar` is the live window this hook is on.
            if moved && !unsafe { IsIconic(bar) }.as_bool() {
                follow(pos.x, pos.y + pos.cy, pos.cx);
            }
        }
        WM_SYSCOMMAND if (wparam.0 as u32 & 0xFFF0) == SC_MINIMIZE => snapshot(),
        WM_ENTERSIZEMOVE => MOVING.store(true, Ordering::Relaxed),
        WM_EXITSIZEMOVE => MOVING.store(false, Ordering::Relaxed),
        WM_SIZE => {
            let min = wparam.0 as u32 == SIZE_MINIMIZED;
            if min != MINIMIZED.swap(min, Ordering::Relaxed) {
                debug(if min { "group: hiding clients with the bar" } else { "group: showing clients with the bar" });
                if min {
                    wg::hide(&members());
                } else {
                    wg::show_all(&members());
                }
            }
        }
        WM_ACTIVATE if (wparam.0 & 0xFFFF) != 0 => wg::raise_under(bar, &members()),
        WM_DWMSENDICONICTHUMBNAIL => {
            let max_w = (lparam.0 as u32 >> 16) as i32;
            let max_h = (lparam.0 as u32 & 0xFFFF) as i32;
            send_thumbnail(bar, max_w, max_h);
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: removing our own hook while the window goes away.
            unsafe {
                let _ = RemoveWindowSubclass(bar, Some(bar_proc), SUBCLASS_ID);
            }
        }
        _ => {}
    }
    // SAFETY: passes the message on to the next handler unchanged.
    unsafe { DefSubclassProc(bar, msg, wparam, lparam) }
}

/// Bar messages worth logging when tracking down a freeze.
fn notable(msg: u32) -> Option<&'static str> {
    Some(match msg {
        WM_SYSCOMMAND => "WM_SYSCOMMAND",
        WM_SIZE => "WM_SIZE",
        WM_CONTEXTMENU => "WM_CONTEXTMENU",
        WM_RBUTTONDOWN => "WM_RBUTTONDOWN",
        WM_RBUTTONUP => "WM_RBUTTONUP",
        WM_NCRBUTTONDOWN => "WM_NCRBUTTONDOWN",
        WM_NCRBUTTONUP => "WM_NCRBUTTONUP",
        WM_CAPTURECHANGED => "WM_CAPTURECHANGED",
        WM_ENTERSIZEMOVE => "WM_ENTERSIZEMOVE",
        WM_EXITSIZEMOVE => "WM_EXITSIZEMOVE",
        WM_ENTERMENULOOP => "WM_ENTERMENULOOP",
        WM_EXITMENULOOP => "WM_EXITMENULOOP",
        WM_ACTIVATE => "WM_ACTIVATE",
        _ => return None,
    })
}

/// Moves the windows along with the bar, whose bottom-left is now (`x`, `y`).
fn follow(x: i32, y: i32, width: i32) {
    let moves: Moves = {
        let Ok(st) = STATE.try_lock() else { return };
        let Some(s) = st.as_ref() else { return };
        let same_size = width == s.width;
        let cells: Vec<Cell> = if same_size {
            s.offsets.iter().map(|o| Cell { x: x + o.x, y: y + o.y, ..*o }).collect()
        } else {
            // The bar changed width: a rough grid now, the real tile follows
            // from the UI.
            grid(s.members.len(), x, y, width)
        };
        s.members.iter().zip(cells).map(|(&h, c)| (h, c, !same_size)).collect()
    };
    group_worker::post_moves(moves);
}

/// A 32-bit top-down DIB selected into a new memory DC.
struct Dib {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    w: i32,
    h: i32,
}

impl Dib {
    fn new(w: i32, h: i32) -> Option<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: `info` describes a valid 32-bit DIB; the DC and bitmap are
        // freed in Drop.
        unsafe {
            let bmp = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let dc = CreateCompatibleDC(None);
            let old = SelectObject(dc, bmp.into());
            SetStretchBltMode(dc, HALFTONE);
            Some(Self { dc, bmp, old, bits: bits.cast(), w, h })
        }
    }

    fn pixels(&mut self) -> &mut [u8] {
        // SAFETY: flushes pending GDI drawing; `bits` points at w * h * 4
        // bytes owned by the DIB, which lives as long as self.
        unsafe {
            let _ = GdiFlush();
            std::slice::from_raw_parts_mut(self.bits, (self.w * self.h * 4) as usize)
        }
    }

    /// GDI leaves alpha at 0, which DWM would show as transparent.
    fn make_opaque(&mut self) {
        for px in self.pixels().as_chunks_mut::<4>().0 {
            px[3] = 255;
        }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // SAFETY: undoes what new() set up, in reverse.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bmp.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// Copies the grid off the screen, scaled down. PrintWindow can't read
/// Roblox's DirectX output, so the grid has to be in front when this runs.
fn capture(members: &[HWND]) -> Option<Snapshot> {
    // SAFETY: IsWindowVisible accepts any handle value.
    if members.is_empty() || members.iter().any(|&h| !unsafe { IsWindowVisible(h) }.as_bool()) {
        return None;
    }
    let rects: Vec<Cell> = members.iter().map(|&h| rect(h)).collect();
    let left = rects.iter().map(|r| r.x).min()?;
    let top = rects.iter().map(|r| r.y).min()?;
    let right = rects.iter().map(|r| r.x + r.w).max()?;
    let bottom = rects.iter().map(|r| r.y + r.h).max()?;
    let (gw, gh) = (right - left, bottom - top);
    let scale = (SNAPSHOT_WIDTH as f32 / gw as f32).min(1.0);
    let mut out = Dib::new(((gw as f32 * scale) as i32).max(1), ((gh as f32 * scale) as i32).max(1))?;

    // SAFETY: the screen DC is released before the block ends.
    unsafe {
        let screen = GetDC(None);
        let _ = StretchBlt(out.dc, 0, 0, out.w, out.h, Some(screen), left, top, gw, gh, SRCCOPY);
        ReleaseDC(None, screen);
    }
    let pixels = out.pixels().to_vec();
    if pixels.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]) {
        debug("group: grid capture came out black, keeping the last one");
        return None;
    }
    let snap = Snapshot { w: out.w, h: out.h, pixels };
    let level = snap.brightness();
    if level < 16 {
        debug(format!("group: grid capture is very dark (brightness {level})"));
    }
    Some(snap)
}

fn send_thumbnail(bar: HWND, max_w: i32, max_h: i32) {
    let Ok(mut st) = STATE.try_lock() else { return };
    let Some(s) = st.as_mut() else { return };
    s.preview_size = (max_w, max_h);
    // Alt-tab covers the screen now, so only the saved picture will do.
    let Some(snap) = &s.snapshot else {
        debug(format!("group: alt-tab asked for a {max_w}x{max_h} preview, none saved yet"));
        return;
    };
    snap.save();
    let result = push(bar, snap, max_w, max_h);
    debug(format!("group: alt-tab asked for {max_w}x{max_h} (brightness {}), {result:?}", snap.brightness()));
}

/// Scales the picture to fit and gives it to DWM as the bar's preview.
fn push(bar: HWND, snap: &Snapshot, max_w: i32, max_h: i32) -> windows::core::Result<()> {
    let fail = windows::core::Error::from_thread;
    let mut src = Dib::new(snap.w, snap.h).ok_or_else(fail)?;
    src.pixels().copy_from_slice(&snap.pixels);
    let scale = (max_w as f32 / snap.w as f32).min(max_h as f32 / snap.h as f32).min(1.0);
    let (w, h) = (((snap.w as f32 * scale) as i32).max(1), ((snap.h as f32 * scale) as i32).max(1));
    let mut out = Dib::new(w, h).ok_or_else(fail)?;
    // SAFETY: both DCs are live DIB DCs; DWM copies the bitmap, so `out`
    // can be freed afterwards.
    unsafe {
        let _ = StretchBlt(out.dc, 0, 0, w, h, Some(src.dc), 0, 0, snap.w, snap.h, SRCCOPY);
        out.make_opaque();
        DwmSetIconicThumbnail(bar, out.bmp, 0)
    }
}
