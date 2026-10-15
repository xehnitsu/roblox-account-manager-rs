// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod accounts;
mod app;
mod applog;
mod browser_login;
mod config;
mod launcher;
mod roblox_api;
mod title_bar;
mod ui;

fn main() -> eframe::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("tokio runtime");
    let handle = rt.handle().clone();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Roblox Account Manager")
            .with_inner_size(app::DEFAULT_SIZE)
            .with_min_inner_size(app::MIN_SIZE)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).expect("valid icon")),
        // Always open at the default size in the middle of the screen. (eframe's
        // window persistence is disabled: its restored size opened at 64x64.)
        centered: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "Roblox Account Manager",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, handle)))),
    );
    // Dropping the runtime stops background tasks; MultiInstance handles are
    // released when the process exits.
    drop(rt);
    result
}
