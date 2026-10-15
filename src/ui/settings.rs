use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{Pending, error_label, poll};
use crate::accounts::storage::Protection;
use crate::accounts::transfer;
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
    /// Open file picker, and whether it's for export (else import).
    picking: Option<Pending<Option<PathBuf>>>,
    picking_export: bool,
    transfer_pw: String,
    transfer_pw2: String,
    /// Import only accounts, keeping this PC's settings.
    keep_settings: bool,
    transfer_msg: Option<Result<String, String>>,
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
                ui.label("Theme:");
                egui::ComboBox::from_id_salt("theme").selected_text(cfg.theme.name()).show_ui(ui, |ui| {
                    for t in crate::config::AppTheme::ALL {
                        if ui.selectable_value(&mut cfg.theme, t, t.name()).changed() {
                            super::theme::apply(ui.ctx(), t);
                        }
                    }
                });
            });
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
                ui.label("Pause between launches:");
                ui.add(egui::DragValue::new(&mut cfg.launch_delay_ms).range(0..=60_000).speed(50).suffix(" ms"))
                    .on_hover_text("Only applies when launching several accounts; a single launch starts right away.");
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
            ui.separator();
            self.transfer_section(ui, shared);
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

    fn transfer_section(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.heading("Export / import");
        ui.label("Moves accounts, groups and settings to another PC in one password-protected file.");
        if let Some(picked) = poll(&mut self.picking)
            && let Some(path) = picked
        {
            self.transfer_msg =
                Some(if self.picking_export { self.export(shared, &path) } else { self.import(shared, &path) });
        }
        egui::Grid::new("transfer").num_columns(2).show(ui, |ui| {
            ui.label("Password:");
            ui.add(egui::TextEdit::singleline(&mut self.transfer_pw).password(true));
            ui.end_row();
            ui.label("Repeat (export):");
            ui.add(egui::TextEdit::singleline(&mut self.transfer_pw2).password(true));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let idle = self.picking.is_none();
            if ui.add_enabled(idle, egui::Button::new("Export")).clicked() {
                self.transfer_msg = None;
                if self.transfer_pw.is_empty() {
                    self.transfer_msg = Some(Err("enter a password for the file".into()));
                } else if self.transfer_pw != self.transfer_pw2 {
                    self.transfer_msg = Some(Err("passwords don't match".into()));
                } else {
                    self.picking_export = true;
                    self.picking = Some(pick_file(shared, true));
                }
            }
            if ui.add_enabled(idle, egui::Button::new("Import")).clicked() {
                self.transfer_msg = None;
                if self.transfer_pw.is_empty() {
                    self.transfer_msg = Some(Err("enter the file's password".into()));
                } else {
                    self.picking_export = false;
                    self.picking = Some(pick_file(shared, false));
                }
            }
            ui.checkbox(&mut self.keep_settings, "Accounts only (keep current settings)");
        });
        ui.weak("The file holds every account's login cookie. Keep it and its password private.");
        match &self.transfer_msg {
            Some(Ok(m)) => {
                ui.label(m.as_str());
            }
            Some(Err(e)) => error_label(ui, e),
            None => {}
        }
    }

    fn export(&mut self, shared: &Shared, path: &Path) -> Result<String, String> {
        let bundle = transfer::Bundle {
            accounts: shared.accounts.lock().unwrap().clone(),
            settings: shared.config.lock().unwrap().clone(),
        };
        transfer::export(path, &self.transfer_pw, &bundle).map_err(|e| format!("{e:#}"))?;
        self.transfer_pw.clear();
        self.transfer_pw2.clear();
        Ok(format!("Exported {} account(s) to {}", bundle.accounts.len(), path.display()))
    }

    fn import(&mut self, shared: &Shared, path: &Path) -> Result<String, String> {
        let bundle = transfer::import(path, &self.transfer_pw).map_err(|e| format!("{e:#}"))?;
        let (added, updated) = transfer::merge(&mut shared.accounts.lock().unwrap(), bundle.accounts);
        shared.save_accounts().map_err(|e| format!("{e:#}"))?;
        if !self.keep_settings {
            let multi = bundle.settings.multi_instance.clone();
            let theme = bundle.settings.theme;
            *shared.config.lock().unwrap() = bundle.settings;
            shared.save_config();
            shared.multi.lock().unwrap().apply(&multi);
            super::theme::apply(&shared.ctx, theme);
        }
        self.transfer_pw.clear();
        self.transfer_pw2.clear();
        log(format!("Imported accounts from {}", path.display()));
        Ok(format!(
            "Added {added}, updated {updated} account(s){}",
            if self.keep_settings { "" } else { " and settings" }
        ))
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

/// Opens the Windows save (export) or open (import) dialog, starting in Downloads.
fn pick_file(shared: &Shared, export: bool) -> Pending<Option<PathBuf>> {
    Pending::spawn(shared, async move {
        tokio::task::spawn_blocking(move || {
            let dialog = rfd::FileDialog::new()
                .add_filter("Account export", &[transfer::EXTENSION])
                .set_directory(transfer::downloads());
            if export {
                dialog.set_title("Export accounts").set_file_name(transfer::default_file_name()).save_file()
            } else {
                dialog.set_title("Import accounts").pick_file()
            }
        })
        .await
        .ok()
        .flatten()
    })
}
