//! "Add account" popup, opened from the Accounts toolbar.

use std::sync::Arc;
use std::time::Duration;

use super::{Pending, error_label, poll};
use crate::app::{LoginResult, Shared, add_account_from_cookie, add_account_from_credentials, add_account_via_browser};

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
    browser_task: Option<Pending<anyhow::Result<String>>>,
    message: Option<Result<String, String>>,
    /// Bulk credentials that Roblox answered with a captcha / 2-step challenge.
    needs_browser: Vec<(String, String)>,
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
            browser_task: None,
            message: None,
            needs_browser: Vec::new(),
        }
    }
}

pub struct BulkReport {
    added: usize,
    failed: Vec<String>,
    needs_browser: Vec<(String, String)>,
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
        self.cookie_task.is_some()
            || self.login_task.is_some()
            || self.bulk_task.is_some()
            || self.browser_task.is_some()
    }

    fn poll_tasks(&mut self, shared: &Arc<Shared>) {
        if let Some(r) = poll(&mut self.cookie_task) {
            self.message = Some(r.map(|n| format!("Added {n}")).map_err(|e| format!("{e:#}")));
            if matches!(self.message, Some(Ok(_))) {
                self.cookie.clear();
            }
        }
        if let Some(r) = poll(&mut self.login_task) {
            match r {
                Ok(LoginResult::Added(n)) => {
                    self.message = Some(Ok(format!("Logged in and added {n}")));
                    self.password.clear();
                }
                Ok(LoginResult::NeedsBrowser(kind)) => {
                    // Like the original apps: finish the login in a browser window
                    // with the form filled in; the user solves the challenge there.
                    self.message = Some(Ok(format!(
                        "Roblox asked for a {kind} challenge. Finish the login in the browser window."
                    )));
                    let creds = (self.username.trim().to_owned(), self.password.clone());
                    self.start_browser(shared, Some(creds));
                }
                Err(e) => self.message = Some(Err(format!("{e:#}"))),
            }
        }
        if let Some(r) = poll(&mut self.browser_task) {
            self.message = Some(r.map(|n| format!("Added {n}")).map_err(|e| format!("{e:#}")));
            if matches!(self.message, Some(Ok(_))) {
                self.password.clear();
            }
        }
        if let Some(r) = poll(&mut self.bulk_task) {
            let mut msg = format!("Bulk import: {} added", r.added);
            if !r.failed.is_empty() {
                msg += &format!(", {} failed ({})", r.failed.len(), r.failed.join("; "));
            }
            if !r.needs_browser.is_empty() {
                let names: Vec<&str> = r.needs_browser.iter().map(|(u, _)| u.as_str()).collect();
                msg += &format!(". Needs browser login (captcha / 2-step): {}", names.join(", "));
            }
            self.message = Some(if r.failed.is_empty() { Ok(msg) } else { Err(msg) });
            self.needs_browser = r.needs_browser;
        }
    }

    fn start_browser(&mut self, shared: &Arc<Shared>, creds: Option<(String, String)>) {
        let s = shared.clone();
        self.browser_task = Some(Pending::spawn(shared, async move { add_account_via_browser(&s, creds).await }));
    }

    fn browser_button(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>, creds: Option<(String, String)>) {
        let enabled = self.browser_task.is_none() && self.bulk_task.is_none();
        if ui
            .add_enabled(enabled, egui::Button::new("Log in with browser"))
            .on_hover_text("Opens the Roblox login page in your browser (Helium, Chrome, Edge or Brave). Log in there; the account is added when done.")
            .clicked()
        {
            self.message = None;
            self.start_browser(shared, creds);
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, shared: &Arc<Shared>) {
        self.poll_tasks(shared);
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
                        ui.colored_label(super::theme::success_color(ui), s);
                    }
                    Err(s) => error_label(ui, s),
                }
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                if self.busy() {
                    ui.spinner();
                    ui.label(if self.browser_task.is_some() { "Waiting for browser login…" } else { "Working…" });
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
        ui.weak(
            "Don't log out in the browser afterwards: that ends the session and the cookie stops working. \
             Close the browser or clear its cookies instead, or use \"Log in with username & password\".",
        );
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
        ui.add_space(6.0);
        ui.label("Or log in on roblox.com in a browser window and let the app read the cookie:");
        self.browser_button(ui, shared, None);
    }

    fn login_form(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>) {
        let mut enter = false;
        egui::Grid::new("login").num_columns(2).show(ui, |ui| {
            ui.label("Username:");
            let user = ui.add(egui::TextEdit::singleline(&mut self.username).desired_width(f32::INFINITY));
            ui.end_row();
            ui.label("Password:");
            let pass =
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true).desired_width(f32::INFINITY));
            ui.end_row();
            // Enter in either field logs in, like pressing "Log in".
            enter = (user.lost_focus() || pass.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        let ready = !self.username.trim().is_empty() && !self.password.is_empty();
        ui.horizontal(|ui| {
            let idle = self.login_task.is_none() && self.browser_task.is_none() && self.bulk_task.is_none();
            if ui
                .add_enabled(idle && ready, egui::Button::new("Log in"))
                .on_hover_text(
                    "Opens the Roblox login page in your browser with your details filled in. Press Log in there.",
                )
                .clicked()
                || (enter && idle && ready)
            {
                self.message = None;
                let creds = (self.username.trim().to_owned(), self.password.clone());
                self.start_browser(shared, Some(creds));
            }
            // Direct API login; Roblox usually answers it with a captcha, in
            // which case the browser opens anyway.
            if ui
                .add_enabled(idle && ready, egui::Button::new("Log in without browser"))
                .on_hover_text("Tries the login API directly. Falls back to the browser if Roblox asks for a captcha.")
                .clicked()
            {
                let (s, u, p) = (shared.clone(), self.username.trim().to_owned(), self.password.clone());
                self.message = None;
                self.login_task =
                    Some(Pending::spawn(shared, async move { add_account_from_credentials(&s, &u, &p).await }));
            }
        });
        ui.label("A browser window opens with your details filled in. Solve any captcha or 2-step check there.");
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
        if !self.needs_browser.is_empty() {
            let enabled = self.bulk_task.is_none() && self.browser_task.is_none();
            let label = format!("Log in {} account(s) with browser", self.needs_browser.len());
            if ui
                .add_enabled(enabled, egui::Button::new(label))
                .on_hover_text("Opens one browser window at a time with the login filled in.")
                .clicked()
            {
                let (s, pairs) = (shared.clone(), std::mem::take(&mut self.needs_browser));
                self.message = None;
                self.bulk_task = Some(Pending::spawn(shared, bulk_browser(s, pairs)));
            }
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
                Ok(LoginResult::NeedsBrowser(_)) => r.needs_browser.push((u, p)),
                Err(e) => r.failed.push(format!("{u}: {e}")),
            },
        }
    }
    r
}

async fn bulk_browser(shared: Arc<Shared>, pairs: Vec<(String, String)>) -> BulkReport {
    let mut r = BulkReport { added: 0, failed: Vec::new(), needs_browser: Vec::new() };
    for (u, p) in pairs {
        match add_account_via_browser(&shared, Some((u.clone(), p))).await {
            Ok(_) => r.added += 1,
            Err(e) => r.failed.push(format!("{u}: {e}")),
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
