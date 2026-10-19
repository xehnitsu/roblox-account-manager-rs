//! TinyTask-style macros: record mouse and keyboard input, then play it back,
//! either on screen or as window messages to Roblox windows in the background.
//!
//! Mouse positions are stored as a fraction of the window's client area, so a
//! recording plays back in windows of any size.

pub mod player;
pub mod recorder;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    pub name: String,
    pub events: Vec<Event>,
}

impl Macro {
    pub fn duration_ms(&self) -> u32 {
        self.events.last().map_or(0, |e| e.t)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Event {
    /// Milliseconds since the first event.
    pub t: u32,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Kind {
    Key { vk: u16, scan: u16, extended: bool, up: bool },
    Move { x: f32, y: f32 },
    Button { button: Button, up: bool, x: f32, y: f32 },
    Wheel { delta: i16, x: f32, y: f32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Button {
    Left,
    Right,
    Middle,
}

fn path() -> PathBuf {
    crate::config::data_dir().join("macros.json")
}

pub fn load() -> Vec<Macro> {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn save(macros: &[Macro]) -> anyhow::Result<()> {
    std::fs::create_dir_all(crate::config::data_dir())?;
    std::fs::write(path(), serde_json::to_vec(macros)?)?;
    Ok(())
}
