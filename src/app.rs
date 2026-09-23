//! Shared state and the eframe application shell.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use uuid::Uuid;

use crate::accounts::storage::{self, Protection};
use crate::accounts::{Account, AuthMethod};
use crate::applog::log;
use crate::config::Config;
use crate::launcher::multi_instance::MultiInstance;
use crate::launcher::watchdog::{Session, Status};
use crate::roblox_api::auth;
use crate::ui;

/// State shared between the UI thread and background tasks. Every lock is
/// held only briefly and never across an `.await`.
pub struct Shared {
    pub accounts: Mutex<Vec<Account>>,
    pub protection: Mutex<Protection>,
    pub sessions: Mutex<Vec<Session>>,
    pub config: Mutex<Config>,
    pub multi: Mutex<MultiInstance>,
    pub rt: tokio::runtime::Handle,
    pub ctx: egui::Context,
}

impl Shared {
    pub fn account(&self, id: Uuid) -> Option<Account> {
        self.accounts.lock().unwrap().iter().find(|a| a.id == id).cloned()
    }

    pub fn session(&self, id: u64) -> Option<Session> {
        self.sessions.lock().unwrap().iter().find(|s| s.id == id).cloned()
    }

    pub fn update_session(&self, id: u64, f: impl FnOnce(&mut Session)) {
        if let Some(s) = self.sessions.lock().unwrap().iter_mut().find(|s| s.id == id) {
            f(s);
        }
    }

    /// PIDs of clients this app launched and is still tracking.
    pub fn claimed_pids(&self) -> HashSet<u32> {
        self.sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.status == Status::Running)
            .filter_map(|s| s.process.map(|p| p.0))
            .collect()
    }

    pub fn save_accounts(&self) -> Result<()> {
        let accounts = self.accounts.lock().unwrap().clone();
        let protection = self.protection.lock().unwrap().clone();
        storage::save(&storage::accounts_path(), &protection, &accounts)
    }

    pub fn save_config(&self) {
        if let Err(e) = self.config.lock().unwrap().save() {
            log(format!("saving settings failed: {e:#}"));
        }
    }
}

/// Validates `cookie`, then inserts the account or refreshes an existing one
/// with the same Roblox user ID. Returns the username.
pub async fn add_account_from_cookie(
    shared: &Shared,
    cookie: &str,
    credentials: Option<(String, String)>,
) -> Result<String> {
    let cookie = cookie.trim().trim_start_matches(".ROBLOSECURITY=").trim().to_owned();
    anyhow::ensure!(!cookie.is_empty(), "cookie is empty");
    let user = auth::authenticated_user(&cookie).await?;
    let avatar = auth::avatar_headshot(user.id).await.unwrap_or(None);
    let (method, login_username, login_password) = match credentials {
        Some((u, p)) => (AuthMethod::Credentials, Some(u), Some(p)),
        None => (AuthMethod::Cookie, None, None),
    };
    {
        let mut accounts = shared.accounts.lock().unwrap();
        if let Some(a) = accounts.iter_mut().find(|a| a.user_id == user.id) {
            a.cookie = cookie;
            a.username = user.name.clone();
            a.display_name = user.display_name.clone();
            a.avatar_url = avatar.or(a.avatar_url.take());
            if login_username.is_some() {
                a.auth_method = method;
                a.login_username = login_username;
                a.login_password = login_password;
            }
        } else {
            accounts.push(Account {
                id: Uuid::new_v4(),
                user_id: user.id,
                username: user.name.clone(),
                display_name: user.display_name,
                avatar_url: avatar,
                auth_method: method,
                cookie,
                login_username,
                login_password,
                group: String::new(),
                notes: String::new(),
            });
        }
    }
    shared.save_accounts()?;
    log(format!("Added/updated account {}", user.name));
    shared.ctx.request_repaint();
    Ok(user.name)
}

pub enum LoginResult {
    Added(String),
    NeedsBrowser(String),
}

pub async fn add_account_from_credentials(shared: &Shared, username: &str, password: &str) -> Result<LoginResult> {
    match auth::login(username, password).await? {
        auth::LoginOutcome::Success { cookie } => {
            let name =
                add_account_from_cookie(shared, &cookie, Some((username.to_owned(), password.to_owned()))).await?;
            Ok(LoginResult::Added(name))
        }
        auth::LoginOutcome::ChallengeRequired { kind } => Ok(LoginResult::NeedsBrowser(kind)),
    }
}

// ----------------------------------------------------------------- the app --

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Accounts,
    Servers,
    Sessions,
    Settings,
}

enum Lock {
    Unlocked,
    NeedsPassword { input: String, error: Option<String> },
    Broken(String),
}

pub struct App {
    pub shared: Arc<Shared>,
    tab: Tab,
    lock: Lock,
    accounts_ui: ui::account_list::State,
    add_ui: ui::add_account::State,
    servers_ui: ui::server_browser::State,
    settings_ui: ui::settings::State,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        let config = Config::load();
        cc.egui_ctx.set_zoom_factor(config.ui_scale.clamp(0.5, 3.0));
        let shared = Arc::new(Shared {
            accounts: Mutex::new(Vec::new()),
            protection: Mutex::new(Protection::Dpapi),
            sessions: Mutex::new(Vec::new()),
            config: Mutex::new(config.clone()),
            multi: Mutex::new(MultiInstance::default()),
            rt: rt.clone(),
            ctx: cc.egui_ctx.clone(),
        });

        let path = storage::accounts_path();
        let lock = match storage::stored_mode(&path) {
            Ok(Some(true)) => Lock::NeedsPassword { input: String::new(), error: None },
            Ok(_) => match storage::load(&path, &Protection::Dpapi) {
                Ok(a) => {
                    *shared.accounts.lock().unwrap() = a;
                    Lock::Unlocked
                }
                Err(e) => Lock::Broken(format!("{e:#}")),
            },
            Err(e) => Lock::Broken(format!("{e:#}")),
        };

        // Take the singleton mutex as early as possible, before any Roblox starts.
        shared.multi.lock().unwrap().apply(&config.multi_instance);
        rt.spawn(crate::launcher::watchdog::run(shared.clone()));

        Self {
            accounts_ui: ui::account_list::State::new(&config),
            add_ui: Default::default(),
            servers_ui: ui::server_browser::State::new(&config),
            settings_ui: Default::default(),
            shared,
            tab: Tab::Accounts,
            lock,
        }
    }

    fn unlock_screen(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(80.0);
            ui.vertical_centered(|ui| match &mut self.lock {
                Lock::NeedsPassword { input, error } => {
                    ui.heading("Account file is password protected");
                    ui.add_space(8.0);
                    let resp = ui.add(egui::TextEdit::singleline(input).password(true).hint_text("Password"));
                    let go = ui.button("Unlock").clicked()
                        || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                    if let Some(e) = error {
                        ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    }
                    if go {
                        let pw = Protection::Password(input.clone());
                        match storage::load(&storage::accounts_path(), &pw) {
                            Ok(a) => {
                                *self.shared.accounts.lock().unwrap() = a;
                                *self.shared.protection.lock().unwrap() = pw;
                                self.lock = Lock::Unlocked;
                            }
                            Err(e) => *error = Some(format!("{e:#}")),
                        }
                    }
                }
                Lock::Broken(msg) => {
                    ui.heading("Could not open the account file");
                    ui.label(msg.as_str());
                    ui.label(storage::accounts_path().display().to_string());
                    ui.label("Move or delete that file to start fresh. Nothing was changed.");
                }
                Lock::Unlocked => {}
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if !matches!(self.lock, Lock::Unlocked) {
            self.unlock_screen(ui);
            return;
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (tab, name) in [
                    (Tab::Accounts, "Accounts"),
                    (Tab::Servers, "Server browser"),
                    (Tab::Sessions, "Sessions"),
                    (Tab::Settings, "Settings"),
                ] {
                    ui.selectable_value(&mut self.tab, tab, name);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(self.shared.multi.lock().unwrap().status());
                });
            });
        });

        egui::Panel::bottom("log").resizable(true).default_size(120.0).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Log");
                if ui.small_button("Clear").clicked() {
                    crate::applog::clear();
                }
            });
            // Only the visible rows are laid out, so a long log costs nothing.
            let lines = crate::applog::snapshot();
            let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::both().stick_to_bottom(true).auto_shrink([false, false]).show_rows(
                ui,
                row_h,
                lines.len(),
                |ui, range| {
                    for line in &lines[range] {
                        ui.add(egui::Label::new(egui::RichText::new(line).monospace()).extend());
                    }
                },
            );
        });

        match self.tab {
            // The Accounts tab lays out its own side and central panels.
            Tab::Accounts => self.accounts_ui.show(ui, &self.shared),
            tab => {
                egui::CentralPanel::default().show(ui, |ui| match tab {
                    Tab::Servers => self.servers_ui.show(ui, &self.shared, &self.accounts_ui.selected),
                    Tab::Sessions => ui::sessions::show(ui, &self.shared),
                    _ => self.settings_ui.show(ui, &self.shared),
                });
            }
        }

        if let Some(mode) = self.accounts_ui.request_add.take() {
            self.add_ui.open(mode);
        }
        self.add_ui.show(ui.ctx(), &self.shared);

        // Ctrl +/- zooms too; remember whatever the user ends up with.
        let zoom = ui.ctx().zoom_factor();
        let changed = {
            let mut cfg = self.shared.config.lock().unwrap();
            let changed = (cfg.ui_scale - zoom).abs() > 0.001;
            cfg.ui_scale = zoom;
            changed
        };
        if changed {
            self.shared.save_config();
        }

        // Session status changes come from background tasks; keep the view fresh.
        ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
    }
}
