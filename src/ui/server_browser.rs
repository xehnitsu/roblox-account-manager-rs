use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use uuid::Uuid;

use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::config::Config;
use crate::launcher::{LaunchTarget, watchdog};
use crate::roblox_api::find_player::{self, Progress};
use crate::roblox_api::games::{self, Server, ServerPage};

/// `(place_id, page, place name)`.
type FetchResult = (u64, ServerPage, Option<String>);
/// `(place_id, username, their server, place name)`.
type FindResult = (u64, String, Option<Server>, Option<String>);

const ROW_HEIGHT: f32 = 26.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Ping,
    Players,
    Free,
}

pub struct State {
    place: String,
    loaded_place: Option<u64>,
    place_name: Option<String>,
    servers: Vec<Server>,
    cursor: Option<String>,
    sort: Sort,
    manual_job: String,
    selected_server: Option<String>,
    task: Option<Pending<anyhow::Result<FetchResult>>>,
    find_name: String,
    finding: Option<(Pending<anyhow::Result<FindResult>>, Arc<Progress>)>,
    find_status: Option<String>,
    error: Option<String>,
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            place: cfg.last_place_id.clone(),
            loaded_place: None,
            place_name: None,
            servers: Vec::new(),
            cursor: None,
            sort: Sort::Ping,
            manual_job: String::new(),
            selected_server: None,
            task: None,
            find_name: String::new(),
            finding: None,
            find_status: None,
            error: None,
        }
    }

    /// Loads the servers of a place, e.g. one picked on the Universe tab.
    pub fn load(&mut self, shared: &Shared, place_id: u64) {
        self.place = place_id.to_string();
        self.servers.clear();
        self.loaded_place = None;
        self.place_name = None;
        self.find_status = None;
        self.fetch(shared, place_id, None);
    }

    fn fetch(&mut self, shared: &Shared, place_id: u64, cursor: Option<String>) {
        self.error = None;
        let want_name = cursor.is_none();
        self.task = Some(Pending::spawn(shared, async move {
            let page = games::public_servers(place_id, cursor.as_deref(), None).await?;
            let name = if want_name { games::place_name(place_id).await.ok() } else { None };
            Ok((place_id, page, name))
        }));
    }

    /// Searches with the cookie of a selected account, or any account.
    fn find(&mut self, shared: &Shared, place_id: u64, selected: &HashSet<Uuid>) {
        self.error = None;
        self.find_status = None;
        let cookie = {
            let accounts = shared.accounts.lock().unwrap();
            let account = accounts.iter().find(|a| selected.contains(&a.id)).or_else(|| accounts.first());
            account.map(|a| a.cookie.clone())
        };
        let Some(cookie) = cookie else {
            self.error = Some("add an account first, Roblox only lists players to logged-in users".into());
            return;
        };
        let name = self.find_name.trim().to_owned();
        let progress = Arc::new(Progress::default());
        let p = progress.clone();
        let task = Pending::spawn(shared, async move {
            let (user_id, username) = find_player::user_id(&name).await?;
            let server = find_player::find_player(place_id, user_id, &cookie, &p).await?;
            let place_name = games::place_name(place_id).await.ok();
            Ok((place_id, username, server, place_name))
        });
        self.finding = Some((task, progress));
    }

    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>, selected: &HashSet<Uuid>) {
        if let Some(r) = poll(&mut self.task) {
            match r {
                Ok((place_id, page, name)) => {
                    if self.loaded_place != Some(place_id) {
                        self.servers.clear();
                    }
                    self.loaded_place = Some(place_id);
                    if name.is_some() {
                        self.place_name = name;
                    }
                    self.servers.extend(page.servers);
                    self.cursor = page.next_cursor;
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }

        let found = self.finding.as_mut().and_then(|(task, _)| task.take());
        if let Some(r) = found {
            let cancelled = self.finding.take().is_some_and(|(_, p)| p.cancel.load(Ordering::Relaxed));
            match r {
                Ok((place_id, username, Some(server), name)) => {
                    self.find_status = Some(format!("{username} is in this server."));
                    self.loaded_place = Some(place_id);
                    self.place_name = name;
                    self.selected_server = Some(server.id.clone());
                    self.servers = vec![server];
                    self.cursor = None;
                }
                Ok((_, username, None, _)) if !cancelled => {
                    self.find_status = Some(format!("{username} is not in a public server of this place."));
                }
                Ok(_) => {}
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }

        ui.horizontal(|ui| {
            ui.label("Place ID:");
            ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(160.0));
            let busy = self.task.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Load servers")).clicked() {
                match self.place.trim().parse::<u64>() {
                    Ok(id) => self.load(shared, id),
                    Err(_) => self.error = Some("place ID must be a number".into()),
                }
            }
            if let (Some(id), Some(c)) = (self.loaded_place, self.cursor.clone())
                && ui.add_enabled(!busy, egui::Button::new("Load more")).clicked()
            {
                self.fetch(shared, id, Some(c));
            }
            if busy {
                ui.spinner();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Find player:");
            let edit =
                ui.add(egui::TextEdit::singleline(&mut self.find_name).hint_text("Username").desired_width(160.0));
            match &self.finding {
                Some((_, progress)) => {
                    if ui.button("Stop").clicked() {
                        progress.cancel.store(true, Ordering::Relaxed);
                    }
                    ui.spinner();
                    ui.label(format!("{} server(s) checked", progress.servers_checked.load(Ordering::Relaxed)));
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
                }
                None => {
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let ok = !self.find_name.trim().is_empty();
                    let clicked = ui
                        .add_enabled(ok, egui::Button::new("Find"))
                        .on_hover_text("Searches the place's public servers for this player")
                        .clicked();
                    if ok && (clicked || enter) {
                        match self.place.trim().parse::<u64>() {
                            Ok(id) => self.find(shared, id, selected),
                            Err(_) => self.error = Some("place ID must be a number".into()),
                        }
                    }
                }
            }
        });
        if let Some(status) = &self.find_status {
            ui.label(status);
        }

        let n_sel = selected.len();
        ui.horizontal(|ui| {
            ui.label("Join job ID directly:");
            ui.add(
                egui::TextEdit::singleline(&mut self.manual_job)
                    .desired_width((ui.available_width() - 60.0).clamp(120.0, 360.0)),
            );
            let ok = n_sel > 0 && !self.manual_job.trim().is_empty() && self.place.trim().parse::<u64>().is_ok();
            if ui.add_enabled(ok, egui::Button::new("Join")).clicked() {
                let place_id = self.place.trim().parse().unwrap();
                join(shared, selected, place_id, self.manual_job.trim().to_owned());
            }
        });
        if n_sel == 0 {
            ui.weak("Select one or more accounts on the Accounts tab to join a server.");
        }
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        ui.separator();

        let Some(place_id) = self.loaded_place else { return };
        ui.horizontal_wrapped(|ui| {
            ui.strong(self.place_name.clone().unwrap_or_else(|| format!("Place {place_id}")));
            ui.label(format!("— {} server(s) loaded", self.servers.len()));
            ui.label("Sort:");
            ui.radio_value(&mut self.sort, Sort::Ping, "Ping");
            ui.radio_value(&mut self.sort, Sort::Players, "Most players");
            ui.radio_value(&mut self.sort, Sort::Free, "Most free slots");
        });
        match self.sort {
            Sort::Ping => self.servers.sort_by_key(|s| s.ping.unwrap_or(u32::MAX)),
            Sort::Players => self.servers.sort_by_key(|s| std::cmp::Reverse(s.playing)),
            Sort::Free => self.servers.sort_by_key(|s| s.playing as i64 - s.max_players as i64),
        }
        ui.weak("Region is not part of Roblox's public server list API, so it isn't shown. Ping is measured by Roblox from the server's side.");

        let mut join_job = None;
        if self.selected_server.as_ref().is_some_and(|id| !self.servers.iter().any(|s| &s.id == id)) {
            self.selected_server = None;
        }
        let can_join = |s: &Server| n_sel > 0 && !(s.playing >= s.max_players && s.max_players > 0);
        let chosen = self.selected_server.as_ref().and_then(|id| self.servers.iter().find(|s| &s.id == id));
        if ui
            .add_enabled(chosen.is_some_and(can_join), egui::Button::new("Join selected server"))
            .on_hover_text("Or double-click a server")
            .clicked()
        {
            join_job = chosen.map(|s| s.id.clone());
        }

        let cols = [Col::Fixed(80.0), Col::Fixed(80.0), Col::Fixed(60.0), Col::Fill(1.0)];
        rows::header(ui, &cols, &["Players", "Ping", "FPS", "Job ID"]);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, s) in self.servers.iter().enumerate() {
                let selected = self.selected_server.as_ref() == Some(&s.id);
                let (inner, resp) = rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click(), true);
                if ui.is_rect_visible(resp.rect) {
                    let spans = rows::spans(inner, &cols);
                    let players = format!("{}/{}", s.playing, s.max_players);
                    let ping = s.ping.map(|p| format!("{p} ms")).unwrap_or_else(|| "–".into());
                    let fps = s.fps.map(|f| format!("{f:.0}")).unwrap_or_else(|| "–".into());
                    rows::cells(ui, inner, &spans[..3], &[&players, &ping, &fps], selected);
                    let color = rows::text_color(ui, selected);
                    rows::cell(ui, inner, spans[3], egui::RichText::new(&s.id).monospace().color(color));
                }
                if resp.clicked() || resp.secondary_clicked() {
                    self.selected_server = Some(s.id.clone());
                }
                if resp.double_clicked() && can_join(s) {
                    join_job = Some(s.id.clone());
                }
                resp.context_menu(|ui| {
                    if ui.add_enabled(can_join(s), egui::Button::new("Join")).clicked() {
                        join_job = Some(s.id.clone());
                        ui.close();
                    }
                    if ui.button("Copy job ID").clicked() {
                        ui.ctx().copy_text(s.id.clone());
                        ui.close();
                    }
                });
            }
        });
        if let Some(job) = join_job {
            join(shared, selected, place_id, job);
        }
    }
}

fn join(shared: &Arc<Shared>, selected: &HashSet<Uuid>, place_id: u64, job_id: String) {
    let ids: Vec<Uuid> =
        shared.accounts.lock().unwrap().iter().filter(|a| selected.contains(&a.id)).map(|a| a.id).collect();
    for id in ids {
        watchdog::start_session(shared, id, LaunchTarget::Job { place_id, job_id: job_id.clone() });
    }
}
