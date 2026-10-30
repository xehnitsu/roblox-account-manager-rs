//! Non-secret settings, stored as plain JSON next to the account file.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::macros::recorder::Hotkey;

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
    pub launch_delay_ms: u64,
    /// When launching several accounts, start the next one once the previous
    /// one has joined its game (read from its log), up to `JOIN_WAIT_MAX`.
    pub wait_for_join: bool,
    pub last_place_id: String,
    pub recent_places: Vec<u64>,
    /// Group priority: the account list shows these groups first, in this order.
    pub group_order: Vec<String>,
    pub ui_scale: f32,
    pub theme: AppTheme,
    pub auto_update: bool,
    /// Macro record and play hotkeys: virtual-key code plus modifier bits, see
    /// `macros::recorder::Hotkey`.
    pub macro_record_key: u16,
    pub macro_record_mods: u8,
    pub macro_play_key: u16,
    pub macro_play_mods: u8,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            multi_instance: MultiInstanceConfig::default(),
            auto_rejoin: AutoRejoinConfig::default(),
            anti_afk: AntiAfkConfig::default(),
            launch_delay_ms: 500,
            wait_for_join: true,
            last_place_id: String::new(),
            recent_places: Vec::new(),
            group_order: Vec::new(),
            ui_scale: 1.0,
            theme: AppTheme::default(),
            auto_update: true,
            macro_record_key: 0x77,
            macro_record_mods: 0,
            macro_play_key: 0x78,
            macro_play_mods: 0,
        }
    }
}

impl Config {
    /// (record, play)
    pub fn macro_hotkeys(&self) -> (Hotkey, Hotkey) {
        (
            Hotkey { mods: self.macro_record_mods, vk: self.macro_record_key },
            Hotkey { mods: self.macro_play_mods, vk: self.macro_play_key },
        )
    }

    pub fn set_macro_hotkeys(&mut self, record: Hotkey, play: Hotkey) {
        (self.macro_record_mods, self.macro_record_key) = (record.mods, record.vk);
        (self.macro_play_mods, self.macro_play_key) = (play.mods, play.vk);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MultiInstanceConfig {
    pub enabled: bool,
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
    /// First retry delay. Doubles on each retry up to `max_delay_secs`.
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
    /// Gray. Follows the Windows light/dark setting.
    #[default]
    Default,
    Classic,
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
    Background,
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

pub const MAX_RECENT_PLACES: usize = 12;

impl Config {
    pub fn remember_place(&mut self, place_id: u64) {
        self.recent_places.retain(|p| *p != place_id);
        self.recent_places.insert(0, place_id);
        self.recent_places.truncate(MAX_RECENT_PLACES);
    }

    pub fn load() -> Self {
        std::fs::read(settings_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(data_dir())?;
        std::fs::write(settings_path(), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_history_order() {
        let mut c = Config::default();
        for id in [1, 2, 3, 2] {
            c.remember_place(id);
        }
        assert_eq!(c.recent_places, [2, 3, 1]);
        for id in 10..40 {
            c.remember_place(id);
        }
        assert_eq!(c.recent_places.len(), MAX_RECENT_PLACES);
        assert_eq!(c.recent_places[0], 39);
    }
}
