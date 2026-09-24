use std::sync::Arc;
use std::time::Instant;

use egui_extras::{Column, TableBuilder};

use crate::app::Shared;
use crate::launcher::LaunchTarget;
use crate::launcher::watchdog::{self, Session, Status};

fn status_text(s: &Session) -> String {
    match &s.status {
        Status::Launching => "launching…".to_owned(),
        Status::Running => "running".to_owned(),
        Status::NoProcess => "launched (process not detected, not watched)".to_owned(),
        Status::WaitingRetry => {
            let secs = s.next_retry.map(|t| t.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0);
            format!("rejoining in {secs} s")
        }
        Status::GaveUp => "exited (no more retries)".to_owned(),
        Status::Stopped => "stopped".to_owned(),
        Status::Closed => "closed (not rejoined)".to_owned(),
        Status::Failed(e) => format!("failed: {e}"),
    }
}

pub fn show(ui: &mut egui::Ui, shared: &Arc<Shared>) {
    let sessions = shared.sessions.lock().unwrap().clone();
    ui.horizontal(|ui| {
        ui.heading("Launched clients");
        if ui.button("Clear finished").clicked() {
            shared
                .sessions
                .lock()
                .unwrap()
                .retain(|s| matches!(s.status, Status::Launching | Status::Running | Status::WaitingRetry));
        }
    });
    if sessions.is_empty() {
        ui.label("Nothing launched yet.");
        return;
    }
    let mut stop = None;
    let mut rejoin = None;
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::initial(160.0).at_least(80.0).clip(true))
        .column(Column::initial(220.0).at_least(80.0).clip(true))
        .column(Column::remainder().at_least(120.0).clip(true))
        .column(Column::exact(60.0))
        .column(Column::exact(55.0))
        .column(Column::exact(80.0))
        .header(22.0, |mut h| {
            for t in ["Account", "Target", "Status", "PID", "Retries", ""] {
                h.col(|ui| {
                    ui.strong(t);
                });
            }
        })
        .body(|body| {
            body.rows(26.0, sessions.len(), |mut row| {
                let s = &sessions[row.index()];
                row.col(|ui| {
                    ui.label(&s.label);
                });
                row.col(|ui| {
                    let mut target = s.target.describe();
                    if let (Some(job), false) = (&s.job_id, matches!(s.target, LaunchTarget::Job { .. })) {
                        target += &format!(" (server {})", &job[..job.len().min(8)]);
                    }
                    ui.label(target);
                });
                row.col(|ui| {
                    let text = status_text(s);
                    ui.label(&text).on_hover_text(&text);
                });
                row.col(|ui| {
                    ui.label(s.process.map(|p| p.0.to_string()).unwrap_or_else(|| "–".into()));
                });
                row.col(|ui| {
                    ui.label(s.retries.to_string());
                });
                row.col(|ui| match s.status {
                    Status::Launching | Status::Running | Status::WaitingRetry | Status::NoProcess => {
                        if ui.button("Stop").on_hover_text("Close the client and don't rejoin").clicked() {
                            stop = Some(s.id);
                        }
                    }
                    _ => {
                        if ui.button("Relaunch").clicked() {
                            rejoin = Some(s.id);
                        }
                    }
                });
            });
        });
    if let Some(id) = stop {
        watchdog::stop_session(shared, id);
    }
    if let Some(id) = rejoin {
        watchdog::rejoin_now(shared, id);
    }
}
