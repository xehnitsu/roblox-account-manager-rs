use std::path::PathBuf;
use std::sync::Arc;

use super::{Pending, error_label, poll};
use crate::accounts::storage::Protection;
use crate::app::Shared;
use crate::applog::log;
use crate::config::{AfkKey, AfkMode};
use crate::launcher::multi_instance;

#[derive(Default)]
pub struct State {
    new_pw: String,
    new_pw2: String,
    pw_msg: Option<Result<String, String>>,
    download: Option<Pending<anyhow::Result<PathBuf>>>,
}

impl State {
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        if let Some(r) = poll(&mut self.download) {
            match r {
                Ok(p) => log(format!("handle64.exe saved to {}", p.display())),
                Err(e) => log(format!("handle64.exe download failed: {e:#}")),
            }
        }

        let mut cfg = shared.config.lock().unwrap().clone();
        let before = serde_json::to_string(&cfg).unwrap_or_default();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.heading("Appearance");
            ui.horizontal(|ui| {
                ui.label("UI scale:");
                let mut zoom = ui.ctx().zoom_factor();
                if ui.add(egui::Slider::new(&mut zoom, 0.75..=2.5).step_by(0.05).suffix("×")).changed() {
                    ui.ctx().set_zoom_factor(zoom);
                }
                if ui.button("Reset").clicked() {
                    ui.ctx().set_zoom_factor(1.0);
                }
                ui.weak("Ctrl + / Ctrl − / Ctrl 0 also work.");
            });

            ui.separator();
            ui.heading("Multi-instance");
            ui.checkbox(&mut cfg.multi_instance.enabled, "Allow several Roblox clients at once (hold singleton mutex)");
            ui.add_enabled_ui(cfg.multi_instance.enabled, |ui| {
                ui.checkbox(&mut cfg.multi_instance.error_773_fix, "Error 773 fix (lock RobloxCookies.dat)");
                ui.checkbox(
                    &mut cfg.multi_instance.handle64,
                    "Also close singleton handles with handle64.exe (needs administrator)",
                );
                ui.indent("h64", |ui| {
                    let admin = multi_instance::is_admin();
                    ui.label(if admin { "Running as administrator." } else { "Not running as administrator." });
                    match multi_instance::find_handle64() {
                        Some(p) => {
                            ui.label(format!("handle64.exe: {}", p.display()));
                        }
                        None => {
                            ui.label("handle64.exe not found. Put it in a utils\\ folder next to this app, or download it from Microsoft Sysinternals (by downloading you accept the Sysinternals license).");
                            let busy = self.download.is_some();
                            if ui.add_enabled(!busy, egui::Button::new("Download handle64.exe")).clicked() {
                                self.download = Some(Pending::spawn(shared, multi_instance::download_handle64()));
                            }
                            if busy {
                                ui.spinner();
                            }
                        }
                    }
                });
            });
            ui.label(format!("Status: {}", shared.multi.lock().unwrap().status()));
            ui.weak("The mutex can only be taken while no Roblox client is running. If status says “mutex not held”, close all Roblox windows and it is taken again automatically.");

            ui.separator();
            ui.heading("Launching");
            ui.horizontal(|ui| {
                ui.label("Extra pause between launches:");
                ui.add(egui::DragValue::new(&mut cfg.launch_delay_secs).range(0..=120).suffix(" s"));
            });

            ui.separator();
            ui.heading("Auto-rejoin");
            ui.checkbox(&mut cfg.auto_rejoin.enabled, "Relaunch a client that exits unexpectedly");
            ui.add_enabled_ui(cfg.auto_rejoin.enabled, |ui| {
                egui::Grid::new("rejoin").num_columns(2).show(ui, |ui| {
                    ui.label("Max retries:");
                    ui.add(egui::DragValue::new(&mut cfg.auto_rejoin.max_retries).range(1..=100));
                    ui.end_row();
                    ui.label("First retry after:");
                    ui.add(egui::DragValue::new(&mut cfg.auto_rejoin.base_delay_secs).range(1..=600).suffix(" s"));
                    ui.end_row();
                    ui.label("Longest wait between retries:");
                    ui.add(egui::DragValue::new(&mut cfg.auto_rejoin.max_delay_secs).range(1..=3600).suffix(" s"));
                    ui.end_row();
                });
                ui.checkbox(
                    &mut cfg.auto_rejoin.presence_check,
                    "Also restart clients whose account is no longer in a game (disconnect screen)",
                );
                ui.horizontal(|ui| {
                    ui.label("Presence check every:");
                    ui.add(
                        egui::DragValue::new(&mut cfg.auto_rejoin.presence_interval_secs).range(15..=3600).suffix(" s"),
                    );
                });
            });

            ui.separator();
            ui.heading("Anti-AFK");
            ui.checkbox(&mut cfg.anti_afk.enabled, "Periodically tap a key in each launched Roblox window");
            ui.add_enabled_ui(cfg.anti_afk.enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Every:");
                    ui.add(egui::DragValue::new(&mut cfg.anti_afk.interval_secs).range(30..=1140).suffix(" s"));
                    egui::ComboBox::from_label("Key")
                        .selected_text(cfg.anti_afk.key.label())
                        .show_ui(ui, |ui| {
                            for k in AfkKey::ALL {
                                ui.selectable_value(&mut cfg.anti_afk.key, k, k.label());
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("Method:");
                    for m in [AfkMode::Background, AfkMode::Focus] {
                        ui.radio_value(&mut cfg.anti_afk.mode, m, m.label());
                    }
                });
                ui.weak(match cfg.anti_afk.mode {
                    AfkMode::Background => "Key messages go straight to each Roblox window; your focus never changes. Experimental: if clients still get kicked after 20 idle minutes, switch to “Focus window briefly”.",
                    AfkMode::Focus => "Each Roblox window is focused for a moment, then focus returns to your window.",
                });
            });

            ui.separator();
            self.password_section(ui, shared);
        });

        if serde_json::to_string(&cfg).unwrap_or_default() != before {
            let multi = cfg.multi_instance.clone();
            *shared.config.lock().unwrap() = cfg;
            shared.save_config();
            shared.multi.lock().unwrap().apply(&multi);
        }
    }

    fn password_section(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.heading("Account file encryption");
        let is_pw = shared.protection.lock().unwrap().is_password();
        ui.label(if is_pw {
            "Encrypted with your password (AES-256-GCM). You enter it on each start."
        } else {
            "Encrypted with Windows DPAPI: only your Windows user on this PC can read it."
        });
        egui::Grid::new("pw").num_columns(2).show(ui, |ui| {
            ui.label(if is_pw { "New password:" } else { "Password:" });
            ui.add(egui::TextEdit::singleline(&mut self.new_pw).password(true));
            ui.end_row();
            ui.label("Repeat:");
            ui.add(egui::TextEdit::singleline(&mut self.new_pw2).password(true));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let label = if is_pw { "Change password" } else { "Set password" };
            if ui.button(label).clicked() {
                self.pw_msg = Some(if self.new_pw.is_empty() {
                    Err("password is empty".into())
                } else if self.new_pw != self.new_pw2 {
                    Err("passwords don't match".into())
                } else {
                    self.set_protection(shared, Protection::Password(self.new_pw.clone()), "Password set")
                });
            }
            if is_pw && ui.button("Remove password (use DPAPI)").clicked() {
                self.pw_msg = Some(self.set_protection(shared, Protection::Dpapi, "Switched to DPAPI"));
            }
        });
        match &self.pw_msg {
            Some(Ok(m)) => {
                ui.label(m.as_str());
            }
            Some(Err(e)) => error_label(ui, e),
            None => {}
        }
    }

    fn set_protection(&mut self, shared: &Shared, p: Protection, ok: &str) -> Result<String, String> {
        let old = std::mem::replace(&mut *shared.protection.lock().unwrap(), p);
        match shared.save_accounts() {
            Ok(()) => {
                self.new_pw.clear();
                self.new_pw2.clear();
                Ok(ok.to_owned())
            }
            Err(e) => {
                *shared.protection.lock().unwrap() = old;
                Err(format!("{e:#}"))
            }
        }
    }
}
