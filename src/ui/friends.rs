//! Friends tab: the online friends of the selected accounts. Joining one
//! launches the selected accounts following that friend, the way the
//! website's Join button does.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use uuid::Uuid;

use super::rows::{self, Col};
use super::{Pending, error_label, poll};
use crate::app::Shared;
use crate::launcher::{LaunchTarget, watchdog};
use crate::roblox_api::friends::{self, Friend};
use crate::roblox_api::presence::PresenceType;

const ROW_HEIGHT: f32 = 26.0;
/// Pause between accounts: the friends API rate-limits hard.
const ACCOUNT_PAUSE: Duration = Duration::from_millis(800);

struct Row {
    friend: Friend,
    /// Labels of the accounts this is a friend of.
    of: Vec<String>,
}

/// Friends found, and accounts whose list couldn't be loaded.
type LoadResult = (Vec<Row>, Vec<String>);

#[derive(Default)]
pub struct State {
    rows: Vec<Row>,
    selected: Option<u64>,
    task: Option<Pending<LoadResult>>,
    error: Option<String>,
}

impl State {
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>, accounts: &HashSet<Uuid>) {
        if let Some((rows, failed)) = poll(&mut self.task) {
            self.rows = rows;
            self.selected = None;
            if !failed.is_empty() {
                self.error = Some(format!("couldn't load friends of {}", failed.join(", ")));
            }
        }

        ui.horizontal(|ui| {
            let busy = self.task.is_some();
            let label = format!("Load online friends of {} selected account(s)", accounts.len());
            if ui.add_enabled(!busy && !accounts.is_empty(), egui::Button::new(label)).clicked() {
                self.load(shared, accounts);
            }
            if busy {
                ui.spinner();
            }
        });
        if accounts.is_empty() {
            ui.weak("Select accounts on the Accounts tab first. Joining a friend launches the selected accounts.");
        }
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        ui.separator();
        if self.rows.is_empty() {
            if self.task.is_none() {
                ui.weak("No online friends loaded.");
            }
            return;
        }

        let chosen = self.selected.and_then(|id| self.rows.iter().find(|r| r.friend.user_id == id));
        let mut join = None;
        ui.horizontal(|ui| {
            let ok = chosen.is_some_and(joinable) && !accounts.is_empty();
            if ui.add_enabled(ok, egui::Button::new("Join friend")).on_hover_text("Or double-click a friend").clicked()
            {
                join = chosen.map(|r| r.friend.clone());
            }
            ui.weak(format!("{} online", self.rows.len()));
        });

        let cols = [Col::Fill(2.0), Col::Fill(3.0), Col::Fill(2.0)];
        rows::header(ui, &cols, &["Friend", "Where", "Friend of"]);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, row) in self.rows.iter().enumerate() {
                let f = &row.friend;
                let selected = self.selected == Some(f.user_id);
                let (inner, resp) = rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click(), true);
                if ui.is_rect_visible(resp.rect) {
                    let spans = rows::spans(inner, &cols);
                    let name = if f.display_name.is_empty() || f.display_name == f.name {
                        f.name.clone()
                    } else {
                        format!("{} (@{})", f.display_name, f.name)
                    };
                    let place =
                        f.presence.place_id.map(|p| shared.place_name(p).unwrap_or_else(|| format!("Place {p}")));
                    let place = place.unwrap_or_else(|| "a game (hidden)".into());
                    let place = format!("In {place}");
                    let where_ = match f.presence.kind {
                        PresenceType::InGame => place.as_str(),
                        PresenceType::InStudio => "In Studio",
                        _ => "On the website",
                    };
                    rows::cells(ui, inner, &spans, &[&name, where_, &row.of.join(", ")], selected);
                }
                if resp.clicked() || resp.secondary_clicked() {
                    self.selected = Some(f.user_id);
                }
                if resp.double_clicked() && joinable(row) && !accounts.is_empty() {
                    join = Some(f.clone());
                }
                resp.context_menu(|ui| {
                    if ui.add_enabled(joinable(row) && !accounts.is_empty(), egui::Button::new("Join")).clicked() {
                        join = Some(f.clone());
                        ui.close();
                    }
                    if ui.button("Copy user ID").clicked() {
                        ui.ctx().copy_text(f.user_id.to_string());
                        ui.close();
                    }
                });
            }
        });

        if let Some(f) = join
            && let Some(place_id) = f.presence.place_id
        {
            let ids: Vec<Uuid> =
                shared.accounts.lock().unwrap().iter().filter(|a| accounts.contains(&a.id)).map(|a| a.id).collect();
            for id in ids {
                watchdog::start_session(shared, id, LaunchTarget::Follow { place_id, user_id: f.user_id });
            }
        }
    }

    fn load(&mut self, shared: &Shared, accounts: &HashSet<Uuid>) {
        self.error = None;
        let list: Vec<(String, String, u64)> = shared
            .accounts
            .lock()
            .unwrap()
            .iter()
            .filter(|a| accounts.contains(&a.id))
            .map(|a| (a.label(), a.cookie.clone(), a.user_id))
            .collect();
        self.task = Some(Pending::spawn(shared, async move {
            let mut rows: Vec<Row> = Vec::new();
            let mut failed = Vec::new();
            for (i, (label, cookie, user_id)) in list.into_iter().enumerate() {
                if i > 0 {
                    tokio::time::sleep(ACCOUNT_PAUSE).await;
                }
                match friends::online(&cookie, user_id).await {
                    Ok(found) => {
                        for f in found {
                            match rows.iter_mut().find(|r| r.friend.user_id == f.user_id) {
                                Some(r) => r.of.push(label.clone()),
                                None => rows.push(Row { friend: f, of: vec![label.clone()] }),
                            }
                        }
                    }
                    Err(e) => {
                        crate::applog::log(format!("{label}: loading friends failed: {e:#}"));
                        failed.push(label);
                    }
                }
            }
            // Friends in a game first.
            rows.sort_by_key(|r| (r.friend.presence.kind != PresenceType::InGame, r.friend.name.to_lowercase()));
            (rows, failed)
        }));
    }
}

/// Joinable: in a game whose place is visible.
fn joinable(row: &Row) -> bool {
    row.friend.presence.kind == PresenceType::InGame && row.friend.presence.place_id.is_some()
}
