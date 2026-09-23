//! "Add account" popup, opened from the Accounts toolbar.

use std::sync::Arc;
use std::time::Duration;

use super::{Pending, error_label, poll};
use crate::app::{LoginResult, Shared, add_account_from_cookie, add_account_from_credentials};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Cookie,
    Login,
    Bulk,
}

pub struct State {
    open: bool,
    mode: Mode,
    cookie: String,
    username: String,
    password: String,
    bulk: String,
    cookie_task: Option<Pending<anyhow::Result<String>>>,
    login_task: Option<Pending<anyhow::Result<LoginResult>>>,
    bulk_task: Option<Pending<BulkReport>>,
    message: Option<Result<String, String>>,
    /// Set when Roblox demanded a captcha / 2SV for a credential login.
    browser_fallback: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            mode: Mode::Cookie,
            cookie: String::new(),
            username: String::new(),
            password: String::new(),
            bulk: String::new(),
            cookie_task: None,
            login_task: None,
            bulk_task: None,
            message: None,
            browser_fallback: None,
        }
    }
}

pub struct BulkReport {
    added: usize,
    failed: Vec<String>,
    needs_browser: Vec<String>,
}

/// One line of bulk input.
#[derive(Debug, PartialEq, Eq)]
pub enum BulkLine {
    Cookie(String),
    Credentials(String, String),
}

pub fn parse_bulk_line(line: &str) -> Option<BulkLine> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let cookie = line.trim_start_matches(".ROBLOSECURITY=");
    // Real cookies start with the warning banner and are hundreds of chars long.
    if cookie.starts_with("_|WARNING") || cookie.len() > 200 {
        return Some(BulkLine::Cookie(cookie.to_owned()));
    }
    let (u, p) = line.split_once(':')?;
    Some(BulkLine::Credentials(u.trim().to_owned(), p.to_owned()))
}

impl State {
    pub fn open(&mut self, mode: Mode) {
        self.open = true;
        self.mode = mode;
        self.message = None;
    }

    fn busy(&self) -> bool {
        self.cookie_task.is_some() || self.login_task.is_some() || self.bulk_task.is_some()
    }

    fn poll_tasks(&mut self) {
        if let Some(r) = poll(&mut self.cookie_task) {
            self.message = Some(r.map(|n| format!("Added {n}")).map_err(|e| format!("{e:#}")));
            if matches!(self.message, Some(Ok(_))) {
                self.cookie.clear();
                self.browser_fallback = None;
            }
        }
        if let Some(r) = poll(&mut self.login_task) {
            match r {
                Ok(LoginResult::Added(n)) => {
                    self.message = Some(Ok(format!("Logged in and added {n}")));
                    self.password.clear();
                }
                Ok(LoginResult::NeedsBrowser(kind)) => {
                    self.message = None;
                    self.browser_fallback = Some(kind);
                }
                Err(e) => self.message = Some(Err(format!("{e:#}"))),
            }
        }
        if let Some(r) = poll(&mut self.bulk_task) {
            let mut msg = format!("Bulk import: {} added", r.added);
            if !r.failed.is_empty() {
                msg += &format!(", {} failed ({})", r.failed.len(), r.failed.join("; "));
            }
            if !r.needs_browser.is_empty() {
                msg += &format!(". Needs browser login (captcha / 2-step): {}", r.needs_browser.join(", "));
            }
            self.message = Some(if r.failed.is_empty() { Ok(msg) } else { Err(msg) });
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, shared: &Arc<Shared>) {
        self.poll_tasks();
        if !self.open {
            return;
        }

        let modal = egui::Modal::new(egui::Id::new("add_account_modal")).show(ctx, |ui| {
            let width = (ctx.content_rect().width() * 0.8).clamp(320.0, 560.0);
            ui.set_width(width);
            ui.heading("Add account");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Cookie, "Cookie");
                ui.selectable_value(&mut self.mode, Mode::Login, "Username & password");
                ui.selectable_value(&mut self.mode, Mode::Bulk, "Bulk import");
            });
            ui.separator();

            match self.mode {
                Mode::Cookie => self.cookie_form(ui, shared),
                Mode::Login => self.login_form(ui, shared),
                Mode::Bulk => self.bulk_form(ui, shared),
            }

            if let Some(m) = &self.message {
                ui.add_space(6.0);
                match m {
                    Ok(s) => {
                        ui.colored_label(egui::Color32::from_rgb(80, 170, 90), s);
                    }
                    Err(s) => error_label(ui, s),
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if self.busy() {
                    ui.spinner();
                    ui.label("Working…");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        self.open = false;
                    }
                });
            });
        });
        // Esc or a click outside closes the popup; running tasks keep going.
        if modal.should_close() {
            self.open = false;
        }
    }

    fn cookie_form(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.label("Paste the value of the .ROBLOSECURITY cookie.");
        ui.add(
            egui::TextEdit::multiline(&mut self.cookie)
                .password(true)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("_|WARNING:-DO-NOT-SHARE-THIS…"),
        );
        let enabled = self.cookie_task.is_none() && !self.cookie.trim().is_empty();
        if ui.add_enabled(enabled, egui::Button::new("Add")).clicked() {
            let (s, c) = (shared.clone(), self.cookie.clone());
            self.message = None;
            self.cookie_task = Some(Pending::spawn(shared, async move { add_account_from_cookie(&s, &c, None).await }));
        }
    }

    fn login_form(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        egui::Grid::new("login").num_columns(2).show(ui, |ui| {
            ui.label("Username:");
            ui.add(egui::TextEdit::singleline(&mut self.username).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Password:");
            ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(f32::INFINITY));
            ui.end_row();
        });
        let ready = !self.username.trim().is_empty() && !self.password.is_empty();
        if ui.add_enabled(self.login_task.is_none() && ready, egui::Button::new("Log in")).clicked() {
            let (s, u, p) = (shared.clone(), self.username.trim().to_owned(), self.password.clone());
            self.message = None;
            self.browser_fallback = None;
            self.login_task =
                Some(Pending::spawn(shared, async move { add_account_from_credentials(&s, &u, &p).await }));
        }
        if let Some(kind) = self.browser_fallback.clone() {
            ui.add_space(6.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.strong(format!("Roblox asked for a {kind} challenge."));
                ui.label("This app does not solve captchas. Log in with your browser instead:");
                ui.label("1. Open a private/incognito window and log in to roblox.com.");
                ui.label(
                    "2. Press F12 → Application (Chrome/Edge) or Storage (Firefox) → Cookies → https://www.roblox.com.",
                );
                ui.label("3. Copy the value of .ROBLOSECURITY and paste it on the Cookie tab.");
                ui.label("4. Close the window without logging out (logging out invalidates the cookie).");
                ui.horizontal(|ui| {
                    if ui.button("Open roblox.com/login").clicked() {
                        let _ = open::that_detached("https://www.roblox.com/login");
                    }
                    if ui.button("Go to Cookie tab").clicked() {
                        self.mode = Mode::Cookie;
                    }
                });
            });
        }
    }

    fn bulk_form(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        ui.label("One per line: a cookie, or username:password.");
        egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut self.bulk)
                    .desired_rows(8)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
        });
        let enabled = self.bulk_task.is_none() && !self.bulk.trim().is_empty();
        if ui.add_enabled(enabled, egui::Button::new("Import")).clicked() {
            let lines: Vec<BulkLine> = self.bulk.lines().filter_map(parse_bulk_line).collect();
            let s = shared.clone();
            self.bulk.clear();
            self.message = None;
            self.bulk_task = Some(Pending::spawn(shared, bulk_import(s, lines)));
        }
    }
}

async fn bulk_import(shared: Arc<Shared>, lines: Vec<BulkLine>) -> BulkReport {
    let mut r = BulkReport { added: 0, failed: Vec::new(), needs_browser: Vec::new() };
    for (i, line) in lines.into_iter().enumerate() {
        if i > 0 {
            // Stay well clear of Roblox's rate limits.
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        match line {
            BulkLine::Cookie(c) => match add_account_from_cookie(&shared, &c, None).await {
                Ok(_) => r.added += 1,
                Err(e) => r.failed.push(format!("line {}: {e}", i + 1)),
            },
            BulkLine::Credentials(u, p) => match add_account_from_credentials(&shared, &u, &p).await {
                Ok(LoginResult::Added(_)) => r.added += 1,
                Ok(LoginResult::NeedsBrowser(_)) => r.needs_browser.push(u),
                Err(e) => r.failed.push(format!("{u}: {e}")),
            },
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_lines() {
        assert_eq!(parse_bulk_line("  "), None);
        assert_eq!(parse_bulk_line("bob:pa:ss"), Some(BulkLine::Credentials("bob".into(), "pa:ss".into())));
        let c = "_|WARNING:-DO-NOT-SHARE-THIS.--|_ABC";
        assert_eq!(parse_bulk_line(c), Some(BulkLine::Cookie(c.into())));
        assert_eq!(parse_bulk_line(&format!(".ROBLOSECURITY={c}")), Some(BulkLine::Cookie(c.into())));
        assert_eq!(parse_bulk_line("nocolon"), None);
    }
}
