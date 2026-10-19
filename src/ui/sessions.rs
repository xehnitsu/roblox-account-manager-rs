use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use super::rows::{self, Col};
use crate::app::Shared;
use crate::launcher::LaunchTarget;
use crate::launcher::watchdog::{self, Session, Status};

const ROW_HEIGHT: f32 = 26.0;

fn status_text(s: &Session) -> String {
    match &s.status {
        Status::Launching => "launching…".to_owned(),
        Status::Running => match (&s.last_drop, s.in_game, s.left_game) {
            (_, true, _) => "in game".to_owned(),
            (_, _, true) => "running (left the game)".to_owned(),
            (Some(why), _, _) => format!("running ({why})"),
            _ => "running".to_owned(),
        },
        Status::NoProcess => "launched (process not detected, not watched)".to_owned(),
        Status::WaitingRetry => {
            let secs = s.next_retry.map(|t| t.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0);
            match &s.last_drop {
                Some(why) => format!("{why}, rejoining in {secs} s"),
                None => format!("rejoining in {secs} s"),
            }
        }
        Status::GaveUp => "exited (no more retries)".to_owned(),
        Status::Stopped => "stopped".to_owned(),
        Status::Closed => "closed (not rejoined)".to_owned(),
        Status::Failed(e) => format!("failed: {e}"),
    }
}

fn target_text(s: &Session, shared: &Arc<Shared>) -> String {
    let Some(place_id) = s.target.place_id() else { return "Roblox home".to_owned() };
    let game = shared.place_name(place_id).unwrap_or_else(|| format!("Place {place_id}"));
    let short = |job: &str| job[..job.len().min(8)].to_owned();
    match &s.target {
        LaunchTarget::PrivateServer { .. } => format!("{game} (private server)"),
        LaunchTarget::Job { job_id, .. } => format!("{game} (server {})", short(job_id)),
        LaunchTarget::Follow { .. } => format!("{game} (following a player)"),
        _ => match &s.job_id {
            Some(job) => format!("{game} (server {})", short(job)),
            None => game,
        },
    }
}

/// Still running or about to be, so it can be stopped.
fn is_active(s: &Session) -> bool {
    matches!(s.status, Status::Launching | Status::Running | Status::WaitingRetry | Status::NoProcess)
}

#[derive(Default)]
pub struct State {
    selected: HashSet<u64>,
    anchor: Option<u64>,
}

impl State {
    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let sessions = shared.sessions.lock().unwrap().clone();
        self.selected.retain(|id| sessions.iter().any(|s| s.id == *id));
        let chosen: Vec<&Session> = sessions.iter().filter(|s| self.selected.contains(&s.id)).collect();
        let can_stop = chosen.iter().any(|s| is_active(s));
        let can_relaunch = chosen.iter().any(|s| !is_active(s));

        let mut stop = false;
        let mut relaunch = false;
        ui.horizontal(|ui| {
            ui.heading("Launched clients");
            stop |= ui
                .add_enabled(can_stop, egui::Button::new("Stop"))
                .on_hover_text("Close the selected clients and don't rejoin")
                .clicked();
            relaunch |= ui.add_enabled(can_relaunch, egui::Button::new("Relaunch")).clicked();
            if ui.button("Clear finished").clicked() {
                shared.sessions.lock().unwrap().retain(is_active);
            }
        });
        if sessions.is_empty() {
            ui.label("Nothing launched yet.");
            return;
        }

        let (ctrl, shift) = ui.input(|i| (i.modifiers.command, i.modifiers.shift));
        let order: Vec<u64> = sessions.iter().map(|s| s.id).collect();
        if rows::select_all_pressed(ui) {
            self.selected.extend(&order);
        }
        let cols = [Col::Fill(3.0), Col::Fill(4.0), Col::Fill(5.0), Col::Fixed(70.0), Col::Fixed(60.0)];
        rows::header(ui, &cols, &["Account", "Target", "Status", "PID", "Retries"]);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, s) in sessions.iter().enumerate() {
                let selected = self.selected.contains(&s.id);
                let (inner, resp) = rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click(), true);
                let status = status_text(s);
                if ui.is_rect_visible(resp.rect) {
                    let pid = s.process.map(|p| p.0.to_string()).unwrap_or_else(|| "–".into());
                    let texts = [s.label.as_str(), &target_text(s, shared), &status, &pid, &s.retries.to_string()];
                    rows::cells(ui, inner, &rows::spans(inner, &cols), &texts, selected);
                }
                let resp = resp.on_hover_text(format!(
                    "{}
{status}",
                    s.target.describe()
                ));
                if resp.clicked() {
                    rows::click(&mut self.selected, &mut self.anchor, s.id, &order, ctrl, shift);
                }
                if resp.secondary_clicked() {
                    rows::select_for_menu(&mut self.selected, &mut self.anchor, s.id);
                }
                resp.context_menu(|ui| {
                    let chosen: Vec<&Session> = sessions.iter().filter(|s| self.selected.contains(&s.id)).collect();
                    if ui.add_enabled(chosen.iter().any(|s| is_active(s)), egui::Button::new("Stop")).clicked() {
                        stop = true;
                        ui.close();
                    }
                    if ui.add_enabled(chosen.iter().any(|s| !is_active(s)), egui::Button::new("Relaunch")).clicked() {
                        relaunch = true;
                        ui.close();
                    }
                    if let [one] = chosen[..] {
                        ui.separator();
                        if let Some((pid, _)) = one.process
                            && ui.button("Copy PID").clicked()
                        {
                            ui.ctx().copy_text(pid.to_string());
                            ui.close();
                        }
                        if let Some(job) = &one.job_id
                            && ui.button("Copy job ID").clicked()
                        {
                            ui.ctx().copy_text(job.clone());
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui
                        .add_enabled(chosen.iter().any(|s| !is_active(s)), egui::Button::new("Remove from list"))
                        .clicked()
                    {
                        let ids = &self.selected;
                        shared.sessions.lock().unwrap().retain(|s| is_active(s) || !ids.contains(&s.id));
                        ui.close();
                    }
                });
            }
        });

        for s in sessions.iter().filter(|s| self.selected.contains(&s.id)) {
            if stop && is_active(s) {
                watchdog::stop_session(shared, s.id);
            }
            if relaunch && !is_active(s) {
                watchdog::rejoin_now(shared, s.id);
            }
        }
    }
}
