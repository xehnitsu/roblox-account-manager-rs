//! Accounts tab: account list on the left, launch panel on the right.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use super::add_account::Mode as AddMode;
use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::accounts::Account;
use crate::app::Shared;
use crate::applog::log;
use crate::config::Config;
use crate::launcher::{TargetInput, game_input, resolve_target, watchdog};

#[derive(Clone, Copy, PartialEq, Eq)]
enum TargetKind {
    /// Place, specific server or private server, from the two input fields.
    Game,
    Home,
}

pub struct State {
    pub selected: HashSet<Uuid>,
    /// Set when the user picks an entry from the "Add account" menu.
    pub request_add: Option<AddMode>,
    filter: String,
    kind: TargetKind,
    place: String,
    job_id: String,
    launching: Option<Pending<anyhow::Result<usize>>>,
    refreshing: Option<Pending<(usize, usize)>>,
    error: Option<String>,
    confirm_remove: bool,
    /// Last plainly clicked account; Shift+click selects the range from here.
    anchor: Option<Uuid>,
    /// Accounts being dragged to a new position.
    dragging: Option<HashSet<Uuid>>,
    new_group: String,
    avatars_checked: bool,
    /// Tickets were prefetched for the current hover over Launch.
    prefetched: bool,
}

const ROW_HEIGHT: f32 = 36.0;

/// One visible row, copied out so no lock is held while drawing.
struct Row {
    id: Uuid,
    label: String,
    avatar: Option<String>,
    group: String,
    notes: String,
}

/// Moves the `moving` items, keeping their order, in front of `before`
/// (or to the end).
pub fn reorder(items: &mut Vec<Account>, moving: &HashSet<Uuid>, before: Option<Uuid>) {
    let (moved, mut rest): (Vec<Account>, Vec<Account>) = items.drain(..).partition(|a| moving.contains(&a.id));
    let at = before.and_then(|b| rest.iter().position(|a| a.id == b)).unwrap_or(rest.len());
    rest.splice(at..at, moved);
    *items = rest;
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            selected: HashSet::new(),
            request_add: None,
            filter: String::new(),
            kind: TargetKind::Game,
            place: cfg.last_place_id.clone(),
            job_id: String::new(),
            launching: None,
            refreshing: None,
            error: None,
            confirm_remove: false,
            anchor: None,
            dragging: None,
            new_group: String::new(),
            avatars_checked: false,
            prefetched: false,
        }
    }

    fn target_input(&self) -> anyhow::Result<TargetInput> {
        match self.kind {
            TargetKind::Home => Ok(TargetInput::Home),
            TargetKind::Game => game_input(&self.place, &self.job_id),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        if let Some(r) = poll(&mut self.launching) {
            self.error = r.err().map(|e| format!("{e:#}"));
        }
        if let Some((ok, total)) = poll(&mut self.refreshing) {
            log(format!("Refreshed {ok}/{total} account(s)"));
        }
        if !self.avatars_checked {
            self.avatars_checked = true;
            shared.rt.spawn(crate::app::fill_missing_avatars(shared.clone()));
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
        ui.radio_value(&mut self.kind, TargetKind::Game, "Game");
        ui.radio_value(&mut self.kind, TargetKind::Home, "Roblox home");
        ui.add_space(4.0);
        if self.kind == TargetKind::Game {
            let full = f32::INFINITY;
            ui.label("Place ID, game URL or private server link");
            ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(full).hint_text("920587237"));
            ui.label("Job ID (optional, joins that server)");
            ui.add(egui::TextEdit::singleline(&mut self.job_id).desired_width(full).hint_text("Any server"));
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
            let btn =
                super::theme::play_button(ui.ctx(), label).min_size(egui::vec2(ui.available_width() - 24.0, 28.0));
            let resp = ui.add_enabled(n > 0 && !busy, btn);
            // Fetch login tickets while the pointer is on the button, so the
            // click doesn't wait for them.
            if resp.hovered() && !self.prefetched {
                self.prefetched = true;
                self.prefetch_tickets(shared);
            } else if !resp.hovered() {
                self.prefetched = false;
            }
            if resp.clicked() {
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
        let input = match self.target_input() {
            Ok(i) => i,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
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

    /// Prefetches tickets for the first few selected accounts, in list order.
    fn prefetch_tickets(&self, shared: &Arc<Shared>) {
        let cookies: Vec<String> = shared
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| self.selected.contains(&a.id))
            .take(3)
            .map(|a| a.cookie.clone())
            .collect();
        for c in cookies {
            shared.rt.spawn(crate::roblox_api::auth::prefetch_ticket(c));
        }
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

    fn set_group(&mut self, shared: &Shared, group: &str) {
        for a in shared.accounts.lock().unwrap().iter_mut().filter(|a| self.selected.contains(&a.id)) {
            a.group = group.to_owned();
        }
        if let Err(e) = shared.save_accounts() {
            log(format!("saving accounts failed: {e:#}"));
        }
    }

    fn move_selected(&mut self, shared: &Shared, to_top: bool) {
        {
            let mut accounts = shared.accounts.lock().unwrap();
            let before =
                if to_top { accounts.iter().find(|a| !self.selected.contains(&a.id)).map(|a| a.id) } else { None };
            reorder(&mut accounts, &self.selected, before);
        }
        if let Err(e) = shared.save_accounts() {
            log(format!("saving accounts failed: {e:#}"));
        }
    }

    fn row_menu(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let n = self.selected.len();
        let single = if n == 1 { self.selected.iter().next().and_then(|id| shared.account(*id)) } else { None };
        let label = if n > 1 { format!("Launch {n} accounts") } else { "Launch".to_owned() };
        if ui.add_enabled(self.launching.is_none(), egui::Button::new(label)).clicked() {
            self.launch(shared);
            ui.close();
        }
        if let Some(acc) = &single {
            ui.separator();
            if ui.button("Copy username").clicked() {
                ui.ctx().copy_text(acc.username.clone());
                ui.close();
            }
            if ui.button("Copy user ID").clicked() {
                ui.ctx().copy_text(acc.user_id.to_string());
                ui.close();
            }
            if ui.button("Copy cookie").clicked() {
                ui.ctx().copy_text(acc.cookie.clone());
                ui.close();
            }
            if ui.button("Open profile").clicked() {
                let _ = open::that_detached(format!("https://www.roblox.com/users/{}/profile", acc.user_id));
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button("Group", |ui| {
            let mut groups: Vec<String> = shared
                .accounts
                .lock()
                .unwrap()
                .iter()
                .map(|a| a.group.clone())
                .filter(|g| !g.is_empty())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            groups.sort_by_key(|g| g.to_lowercase());
            for g in &groups {
                if ui.button(g).clicked() {
                    self.set_group(shared, g);
                    ui.close();
                }
            }
            if !groups.is_empty() {
                ui.separator();
            }
            ui.horizontal(|ui| {
                let edit =
                    ui.add(egui::TextEdit::singleline(&mut self.new_group).hint_text("New group").desired_width(120.0));
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let name = self.new_group.trim().to_owned();
                let set = ui.add_enabled(!name.is_empty(), egui::Button::new("Set")).clicked();
                if (set || enter) && !name.is_empty() {
                    self.set_group(shared, &name);
                    self.new_group.clear();
                    ui.close();
                }
            });
            if ui.button("Remove from group").clicked() {
                self.set_group(shared, "");
                ui.close();
            }
        });
        if ui.add_enabled(self.refreshing.is_none(), egui::Button::new("Refresh info")).clicked() {
            self.refresh(shared);
            ui.close();
        }
        ui.separator();
        if ui.button("Move to top").clicked() {
            self.move_selected(shared, true);
            ui.close();
        }
        if ui.button("Move to bottom").clicked() {
            self.move_selected(shared, false);
            ui.close();
        }
        ui.separator();
        if ui.button("Remove\u{2026}").clicked() {
            self.confirm_remove = true;
            ui.close();
        }
    }

    fn table(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let filter = self.filter.to_lowercase();
        let rows: Vec<Row> = shared
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
            .map(|a| Row {
                id: a.id,
                label: a.label(),
                avatar: a.avatar_url.clone().filter(|u| !u.is_empty()),
                group: a.group.clone(),
                notes: a.notes.lines().next().unwrap_or("").to_owned(),
            })
            .collect();

        if rows.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                if self.filter.is_empty() {
                    ui.label("No accounts yet.");
                    if ui.button("\u{2795} Add account").clicked() {
                        self.request_add = Some(AddMode::Cookie);
                    }
                } else {
                    ui.label("No accounts match the filter.");
                }
            });
            return;
        }

        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        if rows::select_all_pressed(ui) {
            self.selected.extend(rows.iter().map(|r| r.id));
        }
        let order: Vec<Uuid> = rows.iter().map(|r| r.id).collect();

        // Account (after the avatar), group and notes.
        let cols = [Col::Fixed(38.0), Col::Fill(5.0), Col::Fill(2.0), Col::Fill(3.0)];
        rows::header(ui, &cols, &["", "Account", "Group", "Notes"]);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let mut rects = Vec::with_capacity(rows.len());
            for (i, row) in rows.iter().enumerate() {
                let selected = self.selected.contains(&row.id);
                let (inner, resp) =
                    rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click_and_drag(), self.dragging.is_none());
                rects.push(resp.rect);

                if ui.is_rect_visible(resp.rect) {
                    let avatar = egui::Rect::from_min_size(inner.left_top(), egui::vec2(30.0, 30.0));
                    if let Some(url) = &row.avatar {
                        egui::Image::new(url.as_str()).corner_radius(4.0).paint_at(ui, avatar);
                    } else {
                        // No thumbnail (e.g. Roblox moderated it): show the first letter.
                        let initial = row.label.chars().next().unwrap_or('?').to_uppercase().to_string();
                        ui.painter().rect_filled(avatar, 4.0, ui.visuals().widgets.inactive.bg_fill);
                        ui.painter().text(
                            avatar.center(),
                            egui::Align2::CENTER_CENTER,
                            initial,
                            egui::FontId::proportional(15.0),
                            ui.visuals().weak_text_color(),
                        );
                    }
                    let spans = rows::spans(inner, &cols);
                    rows::cells(ui, inner, &spans[1..], &[&row.label, &row.group, &row.notes], selected);
                }

                if resp.clicked() {
                    rows::click(&mut self.selected, &mut self.anchor, row.id, &order, ctrl, shift);
                }
                // Right-click or drag on an unselected row acts on that row only.
                if resp.secondary_clicked() || resp.drag_started() {
                    rows::select_for_menu(&mut self.selected, &mut self.anchor, row.id);
                }
                if resp.drag_started() {
                    self.dragging = Some(self.selected.clone());
                }
                if resp.double_clicked() && self.launching.is_none() {
                    self.launch(shared);
                }
                resp.context_menu(|ui| self.row_menu(ui, shared));
            }
            self.drag_rows(ui, shared, &rows, &rects);
        });
    }

    /// Draws the drop marker while dragging and moves the rows on release.
    fn drag_rows(&mut self, ui: &mut egui::Ui, shared: &Shared, rows: &[Row], rects: &[egui::Rect]) {
        let Some(moving) = &self.dragging else { return };
        let released = ui.input(|i| i.pointer.any_released());
        if let Some(p) = ui.ctx().pointer_interact_pos() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            let idx = rects.iter().position(|r| p.y < r.center().y).unwrap_or(rects.len());
            let y = match rects.get(idx) {
                Some(r) => r.top(),
                None => rects.last().map_or(0.0, |r| r.bottom()),
            };
            if !released {
                let x = rects.first().map_or(ui.max_rect().x_range(), |r| r.x_range());
                ui.painter().hline(x, y, egui::Stroke::new(2.0, super::theme::drag_line(ui)));
            }

            // Scroll when dragging past the top or bottom edge.
            let clip = ui.clip_rect();
            if p.y < clip.top() + 20.0 {
                ui.scroll_with_delta(egui::vec2(0.0, 6.0));
            } else if p.y > clip.bottom() - 20.0 {
                ui.scroll_with_delta(egui::vec2(0.0, -6.0));
            }

            if released {
                let before = rows[idx..].iter().find(|r| !moving.contains(&r.id)).map(|r| r.id);
                reorder(&mut shared.accounts.lock().unwrap(), moving, before);
                if let Err(e) = shared.save_accounts() {
                    log(format!("saving accounts failed: {e:#}"));
                }
            }
        }
        if released || !ui.input(|i| i.pointer.any_down()) {
            self.dragging = None;
            // Redraw without the drop marker.
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(n: u128) -> Account {
        Account {
            id: Uuid::from_u128(n),
            user_id: n as u64,
            username: n.to_string(),
            display_name: String::new(),
            avatar_url: None,
            auth_method: crate::accounts::AuthMethod::Cookie,
            cookie: String::new(),
            login_username: None,
            login_password: None,
            group: String::new(),
            notes: String::new(),
        }
    }

    fn order(items: &[Account]) -> Vec<u64> {
        items.iter().map(|a| a.user_id).collect()
    }

    #[test]
    fn reorder_moves_selection_as_block() {
        let mut v: Vec<Account> = (1..=5).map(account).collect();
        let moving: HashSet<Uuid> = [2, 4].map(Uuid::from_u128).into();
        reorder(&mut v, &moving, Some(Uuid::from_u128(1)));
        assert_eq!(order(&v), [2, 4, 1, 3, 5]);
        reorder(&mut v, &moving, None);
        assert_eq!(order(&v), [1, 3, 5, 2, 4]);
        reorder(&mut v, &moving, Some(Uuid::from_u128(5)));
        assert_eq!(order(&v), [1, 3, 2, 4, 5]);
    }
}
