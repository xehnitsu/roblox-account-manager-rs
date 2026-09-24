//! Non-secret settings, stored as plain JSON next to the account file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("RobloxAccountManagerRS")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub multi_instance: MultiInstanceConfig,
    pub auto_rejoin: AutoRejoinConfig,
    pub anti_afk: AntiAfkConfig,
    /// Pause between launches when launching several accounts at once.
    pub launch_delay_ms: u64,
    /// Last values typed into the launch panel.
    pub last_place_id: String,
    /// UI zoom factor (Ctrl + / Ctrl - also change it).
    pub ui_scale: f32,
    pub theme: AppTheme,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            multi_instance: MultiInstanceConfig::default(),
            auto_rejoin: AutoRejoinConfig::default(),
            anti_afk: AntiAfkConfig::default(),
            launch_delay_ms: 500,
            last_place_id: String::new(),
            ui_scale: 1.0,
            theme: AppTheme::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiInstanceConfig {
    /// Hold the Roblox singleton mutexes so extra clients can start.
    pub enabled: bool,
    /// Also run the handle64.exe monitor (needs admin + handle64.exe).
    pub handle64: bool,
    /// Lock RobloxCookies.dat while multi-instance is active (error 773 fix).
    pub error_773_fix: bool,
}

impl Default for MultiInstanceConfig {
    fn default() -> Self {
        Self { enabled: true, handle64: false, error_773_fix: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoRejoinConfig {
    pub enabled: bool,
    pub max_retries: u32,
    /// First retry delay; doubles on each further retry up to `max_delay_secs`.
    pub base_delay_secs: u64,
    pub max_delay_secs: u64,
    /// Also rejoin when presence says the account left the game while the
    /// client is still open (e.g. stuck on a disconnect dialog).
    pub presence_check: bool,
    pub presence_interval_secs: u64,
}

impl Default for AutoRejoinConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 5,
            base_delay_secs: 5,
            max_delay_secs: 120,
            presence_check: false,
            presence_interval_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AntiAfkConfig {
    pub enabled: bool,
    pub interval_secs: u64,
    /// Single key sent to each Roblox window. Space (jump) is harmless in most games.
    pub key: AfkKey,
    pub mode: AfkMode,
}

impl Default for AntiAfkConfig {
    fn default() -> Self {
        Self { enabled: false, interval_secs: 600, key: AfkKey::Space, mode: AfkMode::Background }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfkKey {
    Space,
    W,
    RightArrow,
}

impl AfkKey {
    pub const ALL: [AfkKey; 3] = [AfkKey::Space, AfkKey::W, AfkKey::RightArrow];

    pub fn label(self) -> &'static str {
        match self {
            AfkKey::Space => "Space (jump)",
            AfkKey::W => "W (step forward)",
            AfkKey::RightArrow => "Right arrow (turn camera)",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppTheme {
    /// Grey; follows the Windows light/dark setting.
    #[default]
    Default,
    /// The 2011 Roblox website.
    Classic,
    /// Dark variant of `Classic`.
    ClassicDark,
}

impl AppTheme {
    pub const ALL: [AppTheme; 3] = [AppTheme::Default, AppTheme::Classic, AppTheme::ClassicDark];

    pub fn name(self) -> &'static str {
        match self {
            AppTheme::Default => "Default",
            AppTheme::Classic => "Classic Roblox",
            AppTheme::ClassicDark => "Classic Roblox (dark)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AfkMode {
    /// Post key messages straight to the window; focus never changes.
    Background,
    /// Focus each window for a moment and send real input.
    Focus,
}

impl AfkMode {
    pub fn label(self) -> &'static str {
        match self {
            AfkMode::Background => "Background (no focus change)",
            AfkMode::Focus => "Focus window briefly",
        }
    }
}

impl Config {
    pub fn load() -> Self {
        std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(data_dir())?;
        std::fs::write(settings_path(), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}
