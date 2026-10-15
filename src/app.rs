//! Shared state and the eframe application shell.

use std::collections::{HashMap, HashSet};
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

#[derive(Clone)]
pub struct PlaceInfo {
    /// "Place <id>" if the lookup failed.
    pub name: String,
    pub icon: Option<String>,
}

pub const DEFAULT_SIZE: [f32; 2] = [1260.0, 700.0];
pub const MIN_SIZE: [f32; 2] = [720.0, 480.0];

/// State shared between the UI thread and background tasks. Every lock is
/// held only briefly and never across an `.await`.
pub struct Shared {
    pub accounts: Mutex<Vec<Account>>,
    pub protection: Mutex<Protection>,
    pub sessions: Mutex<Vec<Session>>,
    pub config: Mutex<Config>,
    pub multi: Mutex<MultiInstance>,
    /// Game name and icon by place ID; None while loading.
    pub places: Mutex<HashMap<u64, Option<PlaceInfo>>>,
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

    /// Game name and icon of a place, looked up in the background on first use.
    pub fn place_info(self: &Arc<Self>, place_id: u64) -> Option<PlaceInfo> {
        let mut places = self.places.lock().unwrap();
        if let Some(info) = places.get(&place_id) {
            return info.clone();
        }
        places.insert(place_id, None);
        let s = self.clone();
        self.rt.spawn(async move {
            use crate::roblox_api::games;
            let (name, icon) = tokio::join!(games::place_name(place_id), games::place_icon(place_id));
            let name = name.unwrap_or_else(|e| {
                log(format!("looking up the name of place {place_id} failed: {e:#}"));
                format!("Place {place_id}")
            });
            let icon = icon.ok().flatten();
            s.places.lock().unwrap().insert(place_id, Some(PlaceInfo { name, icon }));
            s.ctx.request_repaint();
        });
        None
    }

    pub fn place_name(self: &Arc<Self>, place_id: u64) -> Option<String> {
        self.place_info(place_id).map(|p| p.name)
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

/// Fetches avatars for accounts that don't have one (e.g. the thumbnail was
/// still rendering when the account was added).
pub async fn fill_missing_avatars(shared: Arc<Shared>) {
    let missing: Vec<(Uuid, u64)> = shared
        .accounts
        .lock()
        .unwrap()
        .iter()
        .filter(|a| a.avatar_url.as_deref().is_none_or(str::is_empty))
        .map(|a| (a.id, a.user_id))
        .collect();
    let mut found = 0;
    for (id, user_id) in missing {
        if let Ok(Some(url)) = auth::avatar_headshot(user_id).await
            && let Some(a) = shared.accounts.lock().unwrap().iter_mut().find(|a| a.id == id)
        {
            a.avatar_url = Some(url);
            found += 1;
        }
    }
    if found > 0 {
        if let Err(e) = shared.save_accounts() {
            log(format!("saving accounts failed: {e:#}"));
        }
        shared.ctx.request_repaint();
    }
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

/// Logs in through a WebView2 window (or an external browser without WebView2)
/// and adds the account.
pub async fn add_account_via_browser(shared: &Shared, credentials: Option<(String, String)>) -> Result<String> {
    let cookie = if crate::webview::available() {
        crate::webview::login(credentials.clone()).await?
    } else {
        crate::browser_login::login(credentials.clone()).await?
    };
    add_account_from_cookie(shared, &cookie, credentials).await
}

// ----------------------------------------------------------------- the app --

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Accounts,
    Servers,
    Universe,
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
    universe_ui: ui::universe::State,
    sessions_ui: ui::sessions::State,
    settings_ui: ui::settings::State,
    /// Title bar colours last applied.
    title_bar: Option<(egui::Color32, egui::Color32)>,
    /// The window size was checked after startup.
    size_checked: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Handle) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
        ui::HttpLoader::install(&cc.egui_ctx);
        let config = Config::load();
        crate::webview::remove_stale_profiles();
        ui::theme::apply(&cc.egui_ctx, config.theme);
        cc.egui_ctx.set_zoom_factor(config.ui_scale.clamp(0.5, 3.0));
        let shared = Arc::new(Shared {
            accounts: Mutex::new(Vec::new()),
            protection: Mutex::new(Protection::Dpapi),
            sessions: Mutex::new(Vec::new()),
            config: Mutex::new(config.clone()),
            multi: Mutex::new(MultiInstance::default()),
            places: Mutex::new(HashMap::new()),
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
            universe_ui: ui::universe::State::new(&config),
            sessions_ui: ui::sessions::State::default(),
            settings_ui: Default::default(),
            title_bar: None,
            size_checked: false,
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

impl App {
    /// Resizes and centres the window if it opened smaller than the minimum
    /// size, which Windows otherwise only enforces once the user resizes it.
    fn fix_window_size(&mut self, ctx: &egui::Context) {
        let (inner, monitor) = ctx.input(|i| (i.viewport().inner_rect, i.viewport().monitor_size));
        let Some(inner) = inner else { return };
        self.size_checked = true;
        let min = egui::Vec2::from(MIN_SIZE);
        if inner.width() >= min.x && inner.height() >= min.y {
            return;
        }
        let size = egui::Vec2::from(DEFAULT_SIZE);
        log(format!("window opened at {:.0}x{:.0}; resizing to the default size", inner.width(), inner.height()));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        if let Some(monitor) = monitor {
            let pos = ((monitor - size) / 2.0).max(egui::Vec2::ZERO);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos.to_pos2()));
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let colors = ui::theme::title_bar_colors(ui.ctx());
        if self.title_bar != Some(colors) {
            self.title_bar = Some(colors);
            crate::title_bar::set_colors(frame, colors.0, colors.1);
        }
        if !self.size_checked {
            self.fix_window_size(ui.ctx());
        }
        if !matches!(self.lock, Lock::Unlocked) {
            self.unlock_screen(ui);
            return;
        }

        egui::Panel::top("tabs").frame(ui::theme::tab_bar_frame(ui.ctx())).show(ui, |ui| {
            ui::theme::style_tab_bar(ui);
            ui.horizontal(|ui| {
                for (tab, name) in [
                    (Tab::Accounts, "Accounts"),
                    (Tab::Servers, "Server browser"),
                    (Tab::Universe, "Universe"),
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
                    Tab::Universe => {
                        if let Some(place_id) = self.universe_ui.show(ui, &self.shared, &self.accounts_ui.selected) {
                            self.servers_ui.load(&self.shared, place_id);
                            self.tab = Tab::Servers;
                        }
                    }
                    Tab::Sessions => self.sessions_ui.show(ui, &self.shared),
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
