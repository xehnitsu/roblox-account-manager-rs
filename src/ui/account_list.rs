//! Accounts tab: account list on the left, launch panel on the right.

use std::collections::HashSet;
use std::sync::Arc;

use egui_extras::{Column, TableBuilder};
use uuid::Uuid;

use super::add_account::Mode as AddMode;
use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::applog::log;
use crate::config::Config;
use crate::launcher::{TargetInput, resolve_target, watchdog};

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    Home,
    Place,
    PrivateServer,
    Job,
}

pub struct State {
    pub selected: HashSet<Uuid>,
    /// Set when the user picks an entry from the "Add account" menu.
    pub request_add: Option<AddMode>,
    filter: String,
    kind: TargetKind,
    place: String,
    private_link: String,
    job_id: String,
    launching: Option<Pending<anyhow::Result<usize>>>,
    refreshing: Option<Pending<(usize, usize)>>,
    error: Option<String>,
    confirm_remove: bool,
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            selected: HashSet::new(),
            request_add: None,
            filter: String::new(),
            kind: TargetKind::Place,
            place: cfg.last_place_id.clone(),
            private_link: String::new(),
            job_id: String::new(),
            launching: None,
            refreshing: None,
            error: None,
            confirm_remove: false,
        }
    }

    fn target_input(&self) -> TargetInput {
        match self.kind {
            TargetKind::Home => TargetInput::Home,
            TargetKind::Place => TargetInput::Place(self.place.clone()),
            TargetKind::PrivateServer => TargetInput::PrivateServer(self.private_link.clone()),
            TargetKind::Job => TargetInput::Job { place_id: self.place.clone(), job_id: self.job_id.clone() },
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        if let Some(r) = poll(&mut self.launching) {
            self.error = r.err().map(|e| format!("{e:#}"));
        }
        if let Some((ok, total)) = poll(&mut self.refreshing) {
            log(format!("Refreshed {ok}/{total} account(s)"));
        }

        egui::Panel::right("launch_panel").resizable(true).default_size(300.0).size_range(240.0..=520.0).show(
            ui,
            |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    self.launch_panel(ui, shared);
                    ui.separator();
                    self.details_panel(ui, shared);
                });
            },
        );

        egui::CentralPanel::default().show(ui, |ui| {
            self.toolbar(ui, shared);
            ui.add_space(4.0);
            self.table(ui, shared);
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.horizontal_wrapped(|ui| {
            ui.menu_button("➕ Add account", |ui| {
                if ui.button("Add by cookie…").clicked() {
                    self.request_add = Some(AddMode::Cookie);
                }
                if ui.button("Log in with username & password…").clicked() {
                    self.request_add = Some(AddMode::Login);
                }
                if ui.button("Bulk import…").clicked() {
                    self.request_add = Some(AddMode::Bulk);
                }
            });
            let n = self.selected.len();
            if self.confirm_remove {
                ui.strong(format!("Remove {n} account(s)?"));
                if ui.button("Yes, remove").clicked() {
                    shared.accounts.lock().unwrap().retain(|a| !self.selected.contains(&a.id));
                    self.selected.clear();
                    if let Err(e) = shared.save_accounts() {
                        log(format!("saving accounts failed: {e:#}"));
                    }
                    self.confirm_remove = false;
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_remove = false;
                }
            } else if ui.add_enabled(n > 0, egui::Button::new("Remove…")).clicked() {
                self.confirm_remove = true;
            }
            if ui.add_enabled(n > 0 && self.refreshing.is_none(), egui::Button::new("Refresh info")).clicked() {
                self.refresh(shared);
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter…").desired_width(160.0));
            if ui.button("All").clicked() {
                self.selected = shared.accounts.lock().unwrap().iter().map(|a| a.id).collect();
            }
            if ui.button("None").clicked() {
                self.selected.clear();
            }
        });
    }

    fn launch_panel(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.heading("Launch");
        ui.radio_value(&mut self.kind, TargetKind::Place, "Place ID");
        ui.radio_value(&mut self.kind, TargetKind::Job, "Specific server (Job ID)");
        ui.radio_value(&mut self.kind, TargetKind::PrivateServer, "Private server link");
        ui.radio_value(&mut self.kind, TargetKind::Home, "Roblox home");
        ui.add_space(4.0);
        let full = f32::INFINITY;
        match self.kind {
            TargetKind::Home => {}
            TargetKind::Place => {
                ui.label("Place ID or game URL");
                ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(full));
            }
            TargetKind::PrivateServer => {
                ui.label("Private server link");
                ui.add(
                    egui::TextEdit::singleline(&mut self.private_link)
                        .desired_width(full)
                        .hint_text("https://www.roblox.com/share?code=…"),
                );
            }
            TargetKind::Job => {
                ui.label("Place ID");
                ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(full));
                ui.label("Job ID");
                ui.add(egui::TextEdit::singleline(&mut self.job_id).desired_width(full));
            }
        }
        ui.add_space(6.0);
        let n = self.selected.len();
        let label = match n {
            0 => "Select accounts to launch".to_owned(),
            1 => "Launch".to_owned(),
            n => format!("Launch {n} accounts"),
        };
        let busy = self.launching.is_some();
        ui.horizontal(|ui| {
            let btn = egui::Button::new(label).min_size(egui::vec2(ui.available_width() - 24.0, 28.0));
            if ui.add_enabled(n > 0 && !busy, btn).clicked() {
                self.launch(shared);
            }
            if busy {
                ui.spinner();
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
    }

    fn details_panel(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let single = if self.selected.len() == 1 { self.selected.iter().next().copied() } else { None };
        let Some(id) = single else {
            ui.weak("Select one account to edit its group and notes.");
            return;
        };
        let mut dirty = false;
        {
            let mut accounts = shared.accounts.lock().unwrap();
            let Some(acc) = accounts.iter_mut().find(|a| a.id == id) else { return };
            ui.heading(acc.label());
            ui.horizontal(|ui| {
                ui.weak(format!("User ID {}", acc.user_id));
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(acc.user_id.to_string());
                }
            });
            ui.label("Group");
            dirty |= ui.add(egui::TextEdit::singleline(&mut acc.group).desired_width(f32::INFINITY)).lost_focus();
            ui.label("Notes");
            dirty |= ui
                .add(egui::TextEdit::multiline(&mut acc.notes).desired_rows(3).desired_width(f32::INFINITY))
                .lost_focus();
        }
        if dirty && let Err(e) = shared.save_accounts() {
            log(format!("saving accounts failed: {e:#}"));
        }
    }

    fn launch(&mut self, shared: &Arc<Shared>) {
        self.error = None;
        shared.config.lock().unwrap().last_place_id = self.place.clone();
        shared.save_config();
        // Keep the list order so launches happen top to bottom.
        let ids: Vec<Uuid> =
            shared.accounts.lock().unwrap().iter().filter(|a| self.selected.contains(&a.id)).map(|a| a.id).collect();
        let input = self.target_input();
        let s = shared.clone();
        self.launching = Some(Pending::spawn(shared, async move {
            let first = s.account(ids[0]).ok_or_else(|| anyhow::anyhow!("account vanished"))?;
            let target = resolve_target(&first.cookie, &input).await?;
            for id in &ids {
                watchdog::start_session(&s, *id, target.clone());
            }
            Ok(ids.len())
        }));
    }

    fn refresh(&mut self, shared: &Arc<Shared>) {
        let cookies: Vec<String> = shared
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| self.selected.contains(&a.id))
            .map(|a| a.cookie.clone())
            .collect();
        let s = shared.clone();
        self.refreshing = Some(Pending::spawn(shared, async move {
            let total = cookies.len();
            let mut ok = 0;
            for c in cookies {
                match crate::app::add_account_from_cookie(&s, &c, None).await {
                    Ok(_) => ok += 1,
                    Err(e) => log(format!("refresh failed: {e:#} (cookie may be expired)")),
                }
            }
            (ok, total)
        }));
    }

    fn table(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        // Copy out the few fields the table shows so no lock is held while drawing.
        let filter = self.filter.to_lowercase();
        let rows: Vec<(Uuid, String, Option<String>, String, String)> = shared
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| {
                filter.is_empty()
                    || format!("{} {} {} {}", a.username, a.display_name, a.group, a.notes)
                        .to_lowercase()
                        .contains(&filter)
            })
            .map(|a| (a.id, a.label(), a.avatar_url.clone(), a.group.clone(), a.notes.clone()))
            .collect();

        if rows.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if self.filter.is_empty() {
                    ui.label("No accounts yet.");
                    if ui.button("➕ Add account").clicked() {
                        self.request_add = Some(AddMode::Cookie);
                    }
                } else {
                    ui.label("No accounts match the filter.");
                }
            });
            return;
        }

        let ctrl = ui.input(|i| i.modifiers.command);
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(22.0))
            .column(Column::exact(36.0))
            .column(Column::initial(200.0).at_least(100.0).clip(true))
            .column(Column::initial(110.0).at_least(50.0).clip(true))
            .column(Column::remainder().at_least(80.0).clip(true))
            .header(22.0, |mut h| {
                h.col(|_| {});
                h.col(|_| {});
                h.col(|ui| {
                    ui.strong("Account");
                });
                h.col(|ui| {
                    ui.strong("Group");
                });
                h.col(|ui| {
                    ui.strong("Notes");
                });
            })
            .body(|body| {
                body.rows(36.0, rows.len(), |mut row| {
                    let (id, label, avatar, group, notes) = &rows[row.index()];
                    let mut sel = self.selected.contains(id);
                    row.set_selected(sel);
                    let mut toggled = false;
                    row.col(|ui| {
                        toggled = ui.checkbox(&mut sel, "").changed();
                    });
                    row.col(|ui| {
                        if let Some(url) = avatar {
                            ui.add(egui::Image::new(url.as_str()).fit_to_exact_size(egui::vec2(30.0, 30.0)));
                        }
                    });
                    row.col(|ui| {
                        ui.label(label);
                    });
                    row.col(|ui| {
                        ui.label(group);
                    });
                    row.col(|ui| {
                        ui.label(notes.lines().next().unwrap_or(""));
                    });
                    if toggled {
                        if sel {
                            self.selected.insert(*id);
                        } else {
                            self.selected.remove(id);
                        }
                    } else if row.response().clicked() {
                        // Click selects one account; Ctrl+click adds/removes, like a file list.
                        if ctrl {
                            if !self.selected.remove(id) {
                                self.selected.insert(*id);
                            }
                        } else {
                            self.selected.clear();
                            self.selected.insert(*id);
                        }
                    }
                });
            });
    }
}
