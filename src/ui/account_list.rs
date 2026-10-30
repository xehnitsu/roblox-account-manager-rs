//! Accounts tab: account list on the left, launch panel on the right.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context as _;
use uuid::Uuid;

use super::add_account::Mode as AddMode;
use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::accounts::Account;
use crate::app::Shared;
use crate::applog::log;
use crate::config::{Config, RecentGame};
use crate::launcher::{TargetInput, game_input, input_place_id, resolve_target, watchdog};

pub struct State {
    pub selected: HashSet<Uuid>,
    pub request_add: Option<AddMode>,
    filter: String,
    place: String,
    job_id: String,
    friend: String,
    launching: Option<Pending<anyhow::Result<usize>>>,
    opening_browser: Option<Pending<anyhow::Result<()>>>,
    refreshing: Option<Pending<(usize, usize)>>,
    error: Option<String>,
    confirm_remove: bool,
    /// Last plain-clicked account, the anchor for Shift+click ranges.
    anchor: Option<Uuid>,
    dragging: Option<HashSet<Uuid>>,
    new_group: String,
    avatars_checked: bool,
    /// Tickets were prefetched for this hover over Launch.
    prefetched: bool,
    /// Place field text and when it last changed, so the lookup waits for typing to pause.
    typed: (Option<u64>, std::time::Instant),
}

const ROW_HEIGHT: f32 = 36.0;

struct Row {
    id: Uuid,
    label: String,
    expired: bool,
    avatar: Option<String>,
    group: String,
    notes: String,
}

pub fn reorder(items: &mut Vec<Account>, moving: &HashSet<Uuid>, before: Option<Uuid>) {
    let (moved, mut rest): (Vec<Account>, Vec<Account>) = items.drain(..).partition(|a| moving.contains(&a.id));
    let at = before.and_then(|b| rest.iter().position(|a| a.id == b)).unwrap_or(rest.len());
    rest.splice(at..at, moved);
    *items = rest;
}

/// Groups in priority order: saved order first, then others alphabetically.
/// Ungrouped accounts always come last and aren't listed.
pub fn group_order(saved: &[String], accounts: &[Account]) -> Vec<String> {
    let existing: std::collections::BTreeSet<&str> =
        accounts.iter().map(|a| a.group.as_str()).filter(|g| !g.is_empty()).collect();
    let mut order: Vec<String> = saved.iter().filter(|g| existing.contains(g.as_str())).cloned().collect();
    let mut rest: Vec<&str> = existing.into_iter().filter(|g| !order.iter().any(|o| o == g)).collect();
    rest.sort_by_key(|g| g.to_lowercase());
    order.extend(rest.into_iter().map(str::to_owned));
    order
}

pub fn sorted<'a>(accounts: &'a [Account], saved_order: &[String]) -> Vec<&'a Account> {
    let order = group_order(saved_order, accounts);
    let rank = |g: &str| order.iter().position(|o| o == g).unwrap_or(usize::MAX);
    let mut list: Vec<&Account> = accounts.iter().collect();
    list.sort_by_key(|a| rank(&a.group));
    list
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            selected: HashSet::new(),
            request_add: None,
            filter: String::new(),
            place: cfg.last_place_id.clone(),
            job_id: String::new(),
            friend: String::new(),
            launching: None,
            opening_browser: None,
            refreshing: None,
            error: None,
            confirm_remove: false,
            anchor: None,
            dragging: None,
            new_group: String::new(),
            avatars_checked: false,
            prefetched: false,
            typed: (None, std::time::Instant::now()),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        if let Some(r) = poll(&mut self.launching) {
            self.error = r.err().map(|e| format!("{e:#}"));
        }
        if let Some(Err(e)) = poll(&mut self.opening_browser) {
            self.error = Some(format!("{e:#}"));
        }
        if let Some((ok, total)) = poll(&mut self.refreshing) {
            log(format!("Refreshed {ok}/{total} account(s)"));
            shared.rt.spawn(crate::app::check_cookies(shared.clone(), Some(self.selected.clone())));
        }
        if !self.avatars_checked {
            self.avatars_checked = true;
            shared.rt.spawn(crate::app::fill_missing_avatars(shared.clone()));
            shared.rt.spawn(crate::app::check_cookies(shared.clone(), None));
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
            if ui.button("➕ Add account").clicked() {
                self.request_add = Some(AddMode::Login);
            }
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
        ui.add_space(4.0);
        let full = f32::INFINITY;
        self.game_card(ui, shared);
        ui.label("Place ID, game link or private server link");
        ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(full).hint_text("920587237"));
        ui.label("Job ID");
        ui.add(egui::TextEdit::singleline(&mut self.job_id).desired_width(full).hint_text("Any server"));
        ui.label("Friend");
        ui.add(egui::TextEdit::singleline(&mut self.friend).desired_width(full).hint_text("Username or user ID"));
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
            // Prefetch login tickets on hover so the click doesn't wait.
            if resp.hovered() && !self.prefetched {
                self.prefetched = true;
                self.prefetch_tickets(shared);
            } else if !resp.hovered() {
                self.prefetched = false;
            }
            if resp.clicked() {
                self.launch(shared, false);
            }
            if busy {
                ui.spinner();
            }
        });
        let opening = self.opening_browser.is_some();
        ui.horizontal(|ui| {
            // One rect as wide as Launch, so the edges line up.
            let size = egui::vec2(ui.available_width() - 24.0, ui.spacing().interact_size.y);
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            let gap = ui.spacing().item_spacing.x / 2.0;
            let (mut left, mut right) = rect.split_left_right_at_fraction(0.5);
            left.max.x -= gap;
            right.min.x += gap;
            if ui
                .put(left, |ui: &mut egui::Ui| ui.add_enabled(n > 0 && !opening, egui::Button::new("Browser")))
                .on_hover_text(
                    "Opens roblox.com in a window logged in as the account. Logging out is blocked there so the saved cookie keeps working.",
                )
                .clicked()
            {
                self.open_in_browser(shared);
            }
            if ui
                .put(right, |ui: &mut egui::Ui| ui.add_enabled(n > 0 && !busy, egui::Button::new("Home")))
                .on_hover_text("Starts Roblox on its home screen instead of a game")
                .clicked()
            {
                self.launch(shared, true);
            }
            if opening {
                ui.spinner();
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        self.game_history(ui, shared);
    }

    fn game_history(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let recent = shared.config.lock().unwrap().recent_games.clone();
        if recent.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.strong("Recent games");
            if ui.small_button("Clear").clicked() {
                shared.config.lock().unwrap().recent_games.clear();
                shared.save_config();
            }
        });
        let current = input_place_id(&self.place);
        let mut remove = None;
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, game) in recent.iter().enumerate() {
                let place_id = game.place_id;
                let selected = match &game.link {
                    Some(link) => self.place.trim() == link,
                    None => current == Some(place_id),
                };
                let (inner, resp) = rows::row(ui, i, 36.0, selected, egui::Sense::click(), true);
                let info = shared.place_info(place_id);
                if ui.is_rect_visible(resp.rect) {
                    let icon = egui::Rect::from_min_size(inner.left_top(), egui::vec2(30.0, 30.0));
                    match info.as_ref().and_then(|i| i.icon.as_deref()) {
                        Some(url) => egui::Image::new(url).corner_radius(4.0).paint_at(ui, icon),
                        None => {
                            ui.painter().rect_filled(icon, 4.0, ui.visuals().widgets.inactive.bg_fill);
                        }
                    }
                    let mut name = info.map_or_else(|| format!("Place {place_id}"), |i| i.name);
                    if game.link.is_some() {
                        name.push_str(" (private server)");
                    }
                    let color = rows::text_color(ui, selected);
                    rows::cell(
                        ui,
                        inner,
                        (icon.right() + 8.0, inner.right() + 8.0),
                        egui::RichText::new(name).color(color),
                    );
                }
                let resp = resp.on_hover_text(match &game.link {
                    Some(link) => format!("Private server in place {place_id}\n{link}"),
                    None => format!("Place {place_id}"),
                });
                if resp.clicked() {
                    self.place = game.link.clone().unwrap_or_else(|| place_id.to_string());
                    self.job_id.clear();
                    self.friend.clear();
                }
                resp.context_menu(|ui| {
                    if ui.button("Copy place ID").clicked() {
                        ui.ctx().copy_text(place_id.to_string());
                        ui.close();
                    }
                    if let Some(link) = &game.link
                        && ui.button("Copy link").clicked()
                    {
                        ui.ctx().copy_text(link.clone());
                        ui.close();
                    }
                    if ui.button("Remove from history").clicked() {
                        remove = Some(game.clone());
                        ui.close();
                    }
                });
            }
        });
        if let Some(game) = remove {
            shared.config.lock().unwrap().recent_games.retain(|g| *g != game);
            shared.save_config();
        }
    }

    fn game_card(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let place_id = input_place_id(&self.place);
        if self.typed.0 != place_id {
            self.typed = (place_id, std::time::Instant::now());
        }
        let Some(place_id) = place_id else { return };
        let wait = std::time::Duration::from_millis(400).saturating_sub(self.typed.1.elapsed());
        let info = if wait.is_zero() { shared.place_info(place_id) } else { None };
        if !wait.is_zero() {
            ui.ctx().request_repaint_after(wait);
        }
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(64.0, 64.0), egui::Sense::hover());
            match info.as_ref().and_then(|i| i.icon.as_deref()) {
                Some(url) => egui::Image::new(url).corner_radius(6.0).paint_at(ui, rect),
                None => {
                    ui.painter().rect_filled(rect, 6.0, ui.visuals().widgets.inactive.bg_fill);
                }
            }
            ui.vertical(|ui| {
                ui.add_space(4.0);
                match &info {
                    Some(info) => ui.add(egui::Label::new(egui::RichText::new(&info.name).strong()).wrap()),
                    None => ui.weak("Looking up game…"),
                };
                ui.weak(format!("Place {place_id}"));
            });
        });
        ui.add_space(4.0);
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

    fn launch(&mut self, shared: &Arc<Shared>, home: bool) {
        self.error = None;
        shared.config.lock().unwrap().last_place_id = self.place.clone();
        shared.save_config();
        let ids = self.selected_in_order(shared);
        let input = if home { Ok(TargetInput::Home) } else { game_input(&self.place, &self.job_id, &self.friend) };
        let input = match input {
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
            if let Some(place_id) = target.place_id() {
                let link = match &input {
                    TargetInput::PrivateServer(link) => Some(link.trim().to_owned()),
                    _ => None,
                };
                s.config.lock().unwrap().remember_game(RecentGame { place_id, link });
                s.save_config();
            }
            for id in &ids {
                watchdog::start_session(&s, *id, target.clone());
            }
            Ok(ids.len())
        }));
    }

    fn open_in_browser(&mut self, shared: &Arc<Shared>) {
        self.error = None;
        let accounts: Vec<(String, String)> = self
            .selected_in_order(shared)
            .into_iter()
            .filter_map(|id| shared.account(id))
            .map(|a| (a.username, a.cookie))
            .collect();
        self.opening_browser = Some(Pending::spawn(shared, async move {
            for (username, cookie) in accounts {
                let opened = if crate::webview::available() {
                    crate::webview::open_session(&username, &cookie)
                } else {
                    crate::browser_login::open_logged_in(&cookie).await
                };
                opened.with_context(|| format!("opening a browser for {username}"))?;
                log(format!("Opened a browser logged in as {username}"));
            }
            Ok(())
        }));
    }

    fn selected_in_order(&self, shared: &Shared) -> Vec<Uuid> {
        let saved_order = shared.config.lock().unwrap().group_order.clone();
        let accounts = shared.accounts.lock().unwrap();
        sorted(&accounts, &saved_order).into_iter().filter(|a| self.selected.contains(&a.id)).map(|a| a.id).collect()
    }

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

    fn row_menu(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let n = self.selected.len();
        let single = if n == 1 { self.selected.iter().next().and_then(|id| shared.account(*id)) } else { None };
        let label = if n > 1 { format!("Launch {n} accounts") } else { "Launch".to_owned() };
        if ui.add_enabled(self.launching.is_none(), egui::Button::new(label)).clicked() {
            self.launch(shared, false);
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
        }
        ui.separator();
        // Clicking the "New group" field must keep the menu open. The entries close it.
        let keep_open =
            egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        egui::containers::menu::SubMenuButton::new("Group").config(keep_open.clone()).ui(ui, |ui| {
            let saved_order = shared.config.lock().unwrap().group_order.clone();
            let groups = group_order(&saved_order, &shared.accounts.lock().unwrap());
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
            if groups.len() > 1 {
                ui.separator();
                egui::containers::menu::SubMenuButton::new("Order")
                    .config(keep_open.clone())
                    .ui(ui, |ui| group_order_menu(ui, shared));
            }
        });
        if ui.add_enabled(self.refreshing.is_none(), egui::Button::new("Refresh info")).clicked() {
            self.refresh(shared);
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
        let saved_order = shared.config.lock().unwrap().group_order.clone();
        let expired = shared.expired.lock().unwrap().clone();
        let rows: Vec<Row> = sorted(&shared.accounts.lock().unwrap(), &saved_order)
            .into_iter()
            .filter(|a| {
                filter.is_empty()
                    || format!("{} {} {} {}", a.username, a.display_name, a.group, a.notes)
                        .to_lowercase()
                        .contains(&filter)
            })
            .map(|a| Row {
                id: a.id,
                label: a.label(),
                expired: expired.contains(&a.id),
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
                        self.request_add = Some(AddMode::Login);
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

        // Account is 24 characters wide unless a name needs more, group fits the
        // longest group, notes take the rest.
        let font = egui::TextStyle::Body.resolve(ui.style());
        let (account_width, group_width) = ui.fonts_mut(|f| {
            let mut width = |text: &str| f.layout_no_wrap(text.to_owned(), font.clone(), egui::Color32::WHITE).size().x;
            let min_account = width(&"x".repeat(24));
            let account = rows.iter().map(|r| width(&r.label)).fold(min_account, f32::max);
            let min_group = width("Group");
            let group = rows.iter().map(|r| width(&r.group)).fold(min_group, f32::max);
            (account + 16.0, group + 16.0)
        });
        let account_width = account_width.min(ui.available_width() * 0.5);
        let group_width = group_width.min(ui.available_width() * 0.25);
        let cols = [Col::Fixed(38.0), Col::Fixed(account_width), Col::Fixed(group_width), Col::Fill(1.0)];
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
                        // No thumbnail (e.g. moderated): show the first letter.
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
                    if row.expired {
                        let label = egui::RichText::new(format!("⚠ {}", row.label));
                        rows::cell(ui, inner, spans[1], label.color(ui.visuals().error_fg_color));
                        rows::cells(ui, inner, &spans[2..], &[&row.group, &row.notes], selected);
                    } else {
                        rows::cells(ui, inner, &spans[1..], &[&row.label, &row.group, &row.notes], selected);
                    }
                }

                let resp = if row.expired {
                    resp.on_hover_text("Cookie expired. Add the account again to log back in.")
                } else {
                    resp
                };
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
                    self.launch(shared, false);
                }
                resp.context_menu(|ui| self.row_menu(ui, shared));
            }
            self.drag_rows(ui, shared, &rows, &rects);
        });
    }

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
            ui.ctx().request_repaint();
        }
    }
}

fn group_order_menu(ui: &mut egui::Ui, shared: &Shared) {
    let saved_order = shared.config.lock().unwrap().group_order.clone();
    let mut order = group_order(&saved_order, &shared.accounts.lock().unwrap());
    let mut swap = None;
    // A grid sizes to its content. A right-to-left layout would stretch the menu to the window width.
    egui::Grid::new("group_order").num_columns(2).show(ui, |ui| {
        for (i, g) in order.iter().enumerate() {
            ui.label(g);
            ui.horizontal(|ui| {
                if ui.add_enabled(i > 0, egui::Button::new("\u{2B06}").small()).clicked() {
                    swap = Some((i - 1, i));
                }
                if ui.add_enabled(i + 1 < order.len(), egui::Button::new("\u{2B07}").small()).clicked() {
                    swap = Some((i, i + 1));
                }
            });
            ui.end_row();
        }
    });
    if let Some((a, b)) = swap {
        order.swap(a, b);
        shared.config.lock().unwrap().group_order = order;
        shared.save_config();
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
    fn sorts_by_group_priority() {
        let mut v: Vec<Account> = (1..=5).map(account).collect();
        for (i, g) in ["b", "", "a", "b", "c"].into_iter().enumerate() {
            v[i].group = g.into();
        }
        assert_eq!(group_order(&["c".into(), "gone".into()], &v), ["c", "a", "b"]);
        let ids: Vec<u64> = sorted(&v, &["b".into()]).iter().map(|a| a.user_id).collect();
        assert_eq!(ids, [1, 4, 3, 5, 2]);
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
