use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::config::Config;
use crate::launcher::{LaunchTarget, watchdog};
use crate::roblox_api::games::{self, Server, ServerPage};

/// `(place_id, page, place name)`.
type FetchResult = (u64, ServerPage, Option<String>);

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
            error: None,
        }
    }

    fn fetch(&mut self, shared: &Shared, place_id: u64, cursor: Option<String>) {
        self.error = None;
        let want_name = cursor.is_none();
        self.task = Some(Pending::spawn(shared, async move {
            let page = games::public_servers(place_id, cursor.as_deref()).await?;
            let name = if want_name { games::place_name(place_id).await.ok() } else { None };
            Ok((place_id, page, name))
        }));
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

        ui.horizontal(|ui| {
            ui.label("Place ID:");
            ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(160.0));
            let busy = self.task.is_some();
            if ui.add_enabled(!busy, egui::Button::new("Load servers")).clicked() {
                match self.place.trim().parse::<u64>() {
                    Ok(id) => {
                        self.servers.clear();
                        self.loaded_place = None;
                        self.place_name = None;
                        self.fetch(shared, id, None);
                    }
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
