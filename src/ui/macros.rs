//! Macros tab: record and play back TinyTask-style macros.

use std::collections::HashSet;
use std::sync::Arc;

use uuid::Uuid;

use super::error_label;
use super::rows::{self, Col};
use crate::app::Shared;
use crate::applog::log;
use crate::launcher::roblox_processes;
use crate::macros::player::{self, Mode, Options, Playback};
use crate::macros::{self, Macro, recorder};

const ROW_HEIGHT: f32 = 26.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    Record,
    Play,
}

pub struct State {
    macros: Vec<Macro>,
    selected: Option<usize>,
    mode: Mode,
    all_windows: bool,
    loops: u32,
    speed: f32,
    playback: Option<Playback>,
    error: Option<String>,
    /// Hotkey waiting for its new key.
    binding: Option<Hotkey>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            macros: macros::load(),
            selected: None,
            mode: Mode::EachWindow,
            all_windows: true,
            loops: 1,
            speed: 1.0,
            playback: None,
            error: None,
            binding: None,
        }
    }
}

impl State {
    /// Handles the record and play hotkeys. Call every frame.
    pub fn poll(&mut self, ctx: &egui::Context, shared: &Arc<Shared>, accounts: &HashSet<Uuid>) {
        let keys = recorder::take_hotkeys();
        if keys & recorder::RECORD_PRESSED != 0 {
            self.toggle_recording();
        }
        if keys & recorder::PLAY_PRESSED != 0 {
            self.toggle_playback(ctx, shared, accounts);
        }
        if self.playback.as_ref().is_some_and(Playback::is_done) {
            self.playback = None;
        }
    }

    fn toggle_recording(&mut self) {
        self.error = None;
        if !recorder::is_recording() {
            if let Some(p) = self.playback.take() {
                p.stop();
            }
            recorder::begin();
            return;
        }
        let events = recorder::finish();
        if events.is_empty() {
            self.error = Some("nothing was recorded, switch to the game window after starting".into());
            return;
        }
        let name = (1..).map(|n| format!("Macro {n}")).find(|n| !self.macros.iter().any(|m| &m.name == n)).unwrap();
        self.macros.push(Macro { name, events });
        self.selected = Some(self.macros.len() - 1);
        self.save();
    }

    fn toggle_playback(&mut self, ctx: &egui::Context, shared: &Arc<Shared>, accounts: &HashSet<Uuid>) {
        self.error = None;
        if let Some(p) = self.playback.take() {
            p.stop();
            return;
        }
        if recorder::is_recording() {
            return;
        }
        let Some(m) = self.selected.and_then(|i| self.macros.get(i)).cloned() else {
            self.error = Some("select a macro to play".into());
            return;
        };
        let pids: HashSet<u32> = if self.all_windows {
            roblox_processes().into_iter().map(|(pid, _)| pid).collect()
        } else {
            let sessions = shared.sessions.lock().unwrap();
            sessions
                .iter()
                .filter(|s| accounts.contains(&s.account_id))
                .filter_map(|s| s.process.map(|p| p.0))
                .collect()
        };
        if self.mode == Mode::EachWindow && pids.is_empty() {
            self.error = Some(if self.all_windows {
                "no Roblox windows are open".into()
            } else {
                "none of the selected accounts has a running client".into()
            });
            return;
        }
        log(format!("Playing macro {}", m.name));
        let opts = Options { mode: self.mode, loops: self.loops, speed: self.speed };
        self.playback = Some(player::play(m, opts, pids, ctx.clone()));
    }

    fn save(&self) {
        if let Err(e) = macros::save(&self.macros) {
            log(format!("saving macros failed: {e:#}"));
        }
    }

    /// Takes the next main key press, with the modifiers held at the time, as
    /// the new hotkey. Modifiers alone and keys that can't be hotkeys are
    /// ignored, so Ctrl and Shift can be held first. Esc cancels.
    fn bind(&mut self, ui: &egui::Ui, shared: &Shared) {
        let Some(which) = self.binding else { return };
        let pressed = ui.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key: egui::Key::Escape, pressed: true, .. } => Some(None),
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => {
                    recorder::Hotkey::from_egui(*key, *modifiers).map(Some)
                }
                _ => None,
            })
        });
        let Some(hotkey) = pressed else { return };
        self.binding = None;
        let (mut record, mut play) = shared.config.lock().unwrap().macro_hotkeys();
        match hotkey {
            None => {}
            Some(h) if h == (if which == Hotkey::Record { play } else { record }) => {
                self.error = Some(format!("{} is already the other hotkey", h.name()));
            }
            Some(h) => {
                match which {
                    Hotkey::Record => record = h,
                    Hotkey::Play => play = h,
                }
                shared.config.lock().unwrap().set_macro_hotkeys(record, play);
                shared.save_config();
            }
        }
        recorder::set_keys(record, play);
    }

    /// Right-clicking a Record or Play button (even a disabled one) rebinds its hotkey.
    fn rebind_on_right_click(&mut self, ui: &egui::Ui, resp: &egui::Response, which: Hotkey) {
        let hint = "Right-click to change the hotkey. Ctrl, Shift and Alt combos work, like Ctrl+Shift+R.";
        let resp = resp.clone().on_hover_text(hint).on_disabled_hover_text(hint);
        if self.binding.is_none() && resp.contains_pointer() && ui.input(|i| i.pointer.secondary_clicked()) {
            self.error = None;
            self.binding = Some(which);
            // Free the keys so the press reaches this window.
            recorder::set_keys(recorder::Hotkey::NONE, recorder::Hotkey::NONE);
        }
    }

    fn button_text(&self, ui: &egui::Ui, which: Hotkey, text: &str, key: &str) -> String {
        if self.binding != Some(which) {
            return format!("{text} ({key})");
        }
        // Show the modifiers held so far, e.g. "Ctrl+Shift+…".
        let m = ui.input(|i| i.modifiers);
        let held: String = [(m.ctrl, "Ctrl+"), (m.shift, "Shift+"), (m.alt, "Alt+")]
            .into_iter()
            .filter_map(|(on, name)| on.then_some(name))
            .collect();
        format!("Press the new hotkey: {held}… (Esc cancels)")
    }

    pub fn show(&mut self, ui: &mut egui::Ui, shared: &Arc<Shared>, accounts: &HashSet<Uuid>) {
        self.bind(ui, shared);
        let (record, play) = shared.config.lock().unwrap().macro_hotkeys();
        let (record_key, play_key) = (record.name(), play.name());
        let recording = recorder::is_recording();
        ui.horizontal(|ui| {
            if recording {
                if ui.button(format!("\u{23F9} Stop recording ({record_key})")).clicked() {
                    self.toggle_recording();
                }
                ui.spinner();
                ui.label(format!("Recording, {} events. Switch to the game.", recorder::event_count()));
            } else {
                let text = self.button_text(ui, Hotkey::Record, "\u{23FA} Record", &record_key);
                let resp = ui.add_enabled(self.playback.is_none(), egui::Button::new(text));
                if resp.clicked() {
                    self.toggle_recording();
                }
                self.rebind_on_right_click(ui, &resp, Hotkey::Record);
            }
            ui.separator();
            match &self.playback {
                Some(p) => {
                    if ui.button(format!("\u{23F9} Stop ({play_key})")).clicked() {
                        self.toggle_playback(ui.ctx(), shared, accounts);
                    } else {
                        ui.spinner();
                        let of = if self.loops == 0 { String::new() } else { format!(" of {}", self.loops) };
                        ui.label(format!("Playing, run {}{of}", p.run()));
                    }
                }
                None => {
                    let can_play = !recording && self.selected.is_some();
                    let text = self.button_text(ui, Hotkey::Play, "\u{25B6} Play", &play_key);
                    let resp = ui.add_enabled(can_play, egui::Button::new(text));
                    if resp.clicked() {
                        self.toggle_playback(ui.ctx(), shared, accounts);
                    }
                    self.rebind_on_right_click(ui, &resp, Hotkey::Play);
                }
            }
        });
        if let Some(e) = &self.error {
            error_label(ui, e);
        }
        let taken = recorder::taken();
        for (flag, h) in [(recorder::RECORD_PRESSED, record), (recorder::PLAY_PRESSED, play)] {
            if self.binding.is_none() && taken & flag != 0 {
                error_label(
                    ui,
                    &format!("{} is in use by another app, right-click the button to pick another", h.name()),
                );
            }
        }

        ui.add_enabled_ui(self.playback.is_none(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Playback:");
                ui.radio_value(&mut self.mode, Mode::Screen, "On screen");
                ui.radio_value(&mut self.mode, Mode::EachWindow, "Each window");
            });
            if self.mode == Mode::EachWindow {
                ui.horizontal(|ui| {
                    ui.label("Send to:");
                    ui.radio_value(&mut self.all_windows, true, "All Roblox windows");
                    ui.radio_value(&mut self.all_windows, false, "Selected accounts");
                });
            }
            ui.horizontal(|ui| {
                ui.label("Repeat:");
                ui.add(egui::DragValue::new(&mut self.loops).range(0..=10_000).suffix(" times"));
                ui.weak("(0 = until stopped)");
                ui.label("Speed:");
                ui.add(egui::DragValue::new(&mut self.speed).range(0.25..=4.0).speed(0.05).suffix("x"));
            });
            ui.weak(match self.mode {
                Mode::Screen => {
                    "Replays real mouse and keyboard input on the window in front, like TinyTask. Don't use the PC while it plays."
                }
                Mode::EachWindow => {
                    "Plays the macro in each Roblox window in turn, briefly bringing each one to the front with real input. Roblox ignores input sent to a window that isn't in front, so windows can't run it at the same time. Don't use the PC while it plays."
                }
            });
        });
        ui.separator();

        if self.macros.is_empty() {
            ui.weak(format!(
                "No macros yet. Press Record (or {record_key}), switch to the game, perform the actions, then press {record_key} again."
            ));
            return;
        }
        if let Some(m) = self.selected.and_then(|i| self.macros.get_mut(i)) {
            let mut renamed = false;
            ui.horizontal(|ui| {
                ui.label("Name:");
                renamed = ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(220.0)).lost_focus();
            });
            if renamed {
                self.save();
            }
        }

        let mut delete = None;
        let mut play = false;
        let cols = [Col::Fill(1.0), Col::Fixed(90.0), Col::Fixed(90.0)];
        rows::header(ui, &cols, &["Name", "Length", "Events"]);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, m) in self.macros.iter().enumerate() {
                let selected = self.selected == Some(i);
                let (inner, resp) = rows::row(ui, i, ROW_HEIGHT, selected, egui::Sense::click(), true);
                if ui.is_rect_visible(resp.rect) {
                    let spans = rows::spans(inner, &cols);
                    let length = format!("{:.1} s", m.duration_ms() as f32 / 1000.0);
                    rows::cells(ui, inner, &spans, &[&m.name, &length, &m.events.len().to_string()], selected);
                }
                if resp.clicked() || resp.secondary_clicked() {
                    self.selected = Some(i);
                }
                if resp.double_clicked() && self.playback.is_none() {
                    play = true;
                }
                resp.context_menu(|ui| {
                    if ui.add_enabled(self.playback.is_none(), egui::Button::new("Play")).clicked() {
                        play = true;
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        delete = Some(i);
                        ui.close();
                    }
                });
            }
        });
        if play {
            self.toggle_playback(ui.ctx(), shared, accounts);
        }
        if let Some(i) = delete {
            self.macros.remove(i);
            self.selected = None;
            self.save();
        }
    }
}
