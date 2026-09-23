use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::config::Config;
use crate::launcher::{LaunchTarget, watchdog};
use crate::roblox_api::games::{self, Server, ServerPage};

/// `(place_id, page, place name)`.
type FetchResult = (u64, ServerPage, Option<String>);

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
        let servers = &self.servers;
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::exact(70.0))
            .column(egui_extras::Column::exact(70.0))
            .column(egui_extras::Column::exact(50.0))
            .column(egui_extras::Column::remainder().at_least(120.0).clip(true))
            .column(egui_extras::Column::exact(60.0))
            .header(22.0, |mut h| {
                for t in ["Players", "Ping", "FPS", "Job ID", ""] {
                    h.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(26.0, servers.len(), |mut row| {
                    let s = &servers[row.index()];
                    row.col(|ui| {
                        ui.label(format!("{}/{}", s.playing, s.max_players));
                    });
                    row.col(|ui| {
                        ui.label(s.ping.map(|p| format!("{p} ms")).unwrap_or_else(|| "–".into()));
                    });
                    row.col(|ui| {
                        ui.label(s.fps.map(|f| format!("{f:.0}")).unwrap_or_else(|| "–".into()));
                    });
                    row.col(|ui| {
                        if ui.link(egui::RichText::new(&s.id).monospace()).on_hover_text("Click to copy").clicked() {
                            ui.ctx().copy_text(s.id.clone());
                        }
                    });
                    row.col(|ui| {
                        let full = s.playing >= s.max_players && s.max_players > 0;
                        if ui.add_enabled(n_sel > 0 && !full, egui::Button::new("Join")).clicked() {
                            join_job = Some(s.id.clone());
                        }
                    });
                });
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
