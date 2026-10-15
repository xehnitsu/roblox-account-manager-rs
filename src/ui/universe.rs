//! Lists every place in a game (universe), like the original account
//! manager's Universe tab. Games with several places (e.g. separate worlds)
//! only show their start place on the website.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::config::Config;
use crate::launcher::{LaunchTarget, watchdog};
use crate::roblox_api::games;

/// `(game name, places as (id, name))`.
type LoadResult = (Option<String>, Vec<(u64, String)>);

const ROW_HEIGHT: f32 = 26.0;

pub struct State {
    place: String,
    game_name: Option<String>,
    places: Vec<(u64, String)>,
    selected: Option<u64>,
    task: Option<Pending<anyhow::Result<LoadResult>>>,
    error: Option<String>,
}

impl State {
    pub fn new(cfg: &Config) -> Self {
        Self {
            place: cfg.last_place_id.clone(),
            game_name: None,
            places: Vec::new(),
            selected: None,
            task: None,
            error: None,
        }
    }

    /// Returns a place whose servers the user wants to see.
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>, accounts: &HashSet<Uuid>) -> Option<u64> {
        if let Some(r) = poll(&mut self.task) {
            match r {
                Ok((name, places)) => {
                    self.game_name = name;
                    self.places = places;
                    self.selected = None;
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            }
        }

        ui.horizontal(|ui| {
            ui.label("Place ID:");
            let edit = ui.add(egui::TextEdit::singleline(&mut self.place).desired_width(160.0));
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let busy = self.task.is_some();
            if (ui.add_enabled(!busy, egui::Button::new("Load places")).clicked() || enter && !busy)
                && let Some(id) = self.parse_place()
            {
                self.error = None;
                self.task = Some(Pending::spawn(shared, async move {
                    let places = games::universe_places(id).await?;
                    Ok((games::place_name(id).await.ok(), places))
                }));
            }
            if busy {
                ui.spinner();
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        ui.separator();
        if self.places.is_empty() {
            ui.weak("Enter any place of a game to list all of its places.");
            return None;
        }

        ui.horizontal(|ui| {
            if let Some(name) = &self.game_name {
                ui.strong(name);
            }
            ui.label(format!("{} place(s)", self.places.len()));
        });
        let mut servers = None;
        let mut launch = None;
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.selected.is_some(), egui::Button::new("Show servers"))
                .on_hover_text("Or double-click a place")
                .clicked()
            {
                servers = self.selected;
            }
            if ui
                .add_enabled(self.selected.is_some() && !accounts.is_empty(), egui::Button::new("Launch"))
                .on_hover_text("Launch the selected accounts into this place")
                .clicked()
            {
                launch = self.selected;
            }
        });
        if accounts.is_empty() {
            ui.weak("Select one or more accounts on the Accounts tab to launch.");
        }

        let cols = [Col::Fill(1.0), Col::Fixed(160.0)];
        rows::header(ui, &cols, &["Name", "Place ID"]);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, (id, name)) in self.places.iter().enumerate() {
                let selected = self.selected == Some(*id);
                let (inner, resp) = rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click(), true);
                if ui.is_rect_visible(resp.rect) {
                    let spans = rows::spans(inner, &cols);
                    rows::cells(ui, inner, &spans, &[name, &id.to_string()], selected);
                }
                if resp.clicked() || resp.secondary_clicked() {
                    self.selected = Some(*id);
                }
                if resp.double_clicked() {
                    servers = Some(*id);
                }
                resp.context_menu(|ui| {
                    if ui.button("Show servers").clicked() {
                        servers = Some(*id);
                        ui.close();
                    }
                    if ui.add_enabled(!accounts.is_empty(), egui::Button::new("Launch")).clicked() {
                        launch = Some(*id);
                        ui.close();
                    }
                    if ui.button("Copy place ID").clicked() {
                        ui.ctx().copy_text(id.to_string());
                        ui.close();
                    }
                });
            }
        });

        if let Some(place_id) = launch {
            let ids: Vec<Uuid> =
                shared.accounts.lock().unwrap().iter().filter(|a| accounts.contains(&a.id)).map(|a| a.id).collect();
            for id in ids {
                watchdog::start_session(shared, id, LaunchTarget::Place { place_id });
            }
        }
        servers
    }

    fn parse_place(&mut self) -> Option<u64> {
        let id = crate::launcher::input_place_id(&self.place);
        if id.is_none() {
            self.error = Some("enter a place ID or game link".into());
        }
        id
    }
}
