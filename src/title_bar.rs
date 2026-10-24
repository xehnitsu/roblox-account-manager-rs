//! Colors the native window title bar (Windows 11; ignored on Windows 10).

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWINDOWATTRIBUTE, DwmSetWindowAttribute,
};

pub fn set_colors(window: &impl HasWindowHandle, caption: egui::Color32, text: egui::Color32) {
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(h) = handle.as_raw() else { return };
    let hwnd = HWND(h.hwnd.get() as *mut _);
    for (attr, color) in [(DWMWA_CAPTION_COLOR, caption), (DWMWA_BORDER_COLOR, caption), (DWMWA_TEXT_COLOR, text)] {
        set(hwnd, attr, colorref(color));
    }
}

fn colorref(c: egui::Color32) -> u32 {
    u32::from(c.r()) | u32::from(c.g()) << 8 | u32::from(c.b()) << 16
}

fn set(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: u32) {
    // SAFETY: `hwnd` is this app's live window; the value pointer and size
    // describe a u32 that outlives the call. Unsupported attributes just fail.
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, attr, (&raw const value).cast(), size_of::<u32>() as u32);
    }
}
