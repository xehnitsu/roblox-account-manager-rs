//! Right-click menu (Cut, Copy, Paste, Select all) for every text field.
//! egui text fields have none. Instead of wiring each field, this finds the
//! field under the pointer on a right-click and replays the chosen action as
//! the matching input event on the next frame, with the field focused again.

use egui::text::CCursorRange;
use egui::text_edit::TextEditState;
use egui::{Context, Event, Id, Key, Modifiers, Pos2};

#[derive(Default)]
pub struct TextMenu {
    /// Field the menu is open for, where, and whether text is selected.
    open: Option<(Id, Pos2, bool)>,
    /// Selection to put back: a right-click moves the field's cursor.
    restore: Option<(Id, Option<CCursorRange>)>,
    /// Events to send to a field on the next frame.
    pending: Option<(Id, Vec<Event>)>,
}

impl TextMenu {
    /// Call once per frame before any widgets are drawn.
    pub fn show(&mut self, ctx: &Context) {
        if let Some((id, range)) = self.restore.take()
            && let Some(mut state) = TextEditState::load(ctx, id)
        {
            state.cursor.set_char_range(range);
            state.store(ctx, id);
        }
        if let Some((id, events)) = self.pending.take() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.input_mut(|i| i.events.extend(events));
        }

        let pointer = ctx.input(|i| i.pointer.interact_pos());
        if ctx.input(|i| i.pointer.secondary_pressed())
            && let Some(pos) = pointer
            && let Some(id) = text_field_at(ctx, pos)
        {
            let range = TextEditState::load(ctx, id).and_then(|s| s.cursor.char_range());
            self.restore = Some((id, range));
            let selected = range.is_some_and(|r| !r.is_empty());
            self.open = Some((id, pos, selected));
            ctx.memory_mut(|m| m.request_focus(id));
        }
        let Some((id, pos, selected)) = self.open else { return };

        let mut chosen = None;
        let area =
            egui::Area::new(Id::new("text_menu")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
                egui::Frame::menu(ui.style()).show(ui, |ui| {
                    egui::containers::menu::menu_style(ui.style_mut());
                    ui.set_width(150.0);
                    ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                        let items = [
                            ("Cut", "Ctrl+X", selected, Some(Event::Cut)),
                            ("Copy", "Ctrl+C", selected, Some(Event::Copy)),
                            ("Paste", "Ctrl+V", true, None),
                            ("Select all", "Ctrl+A", true, Some(select_all())),
                        ];
                        for (label, keys, enabled, event) in items {
                            if label == "Select all" {
                                ui.separator();
                            }
                            let button = egui::Button::new(label).shortcut_text(keys);
                            if ui.add_enabled(enabled, button).clicked() {
                                chosen = Some(event.unwrap_or_else(|| Event::Paste(clipboard_text())));
                            }
                        }
                    });
                });
            });
        let pressed_outside =
            ctx.input(|i| i.pointer.primary_pressed()) && pointer.is_some_and(|p| !area.response.rect.contains(p));
        if let Some(event) = chosen {
            self.pending = Some((id, vec![event]));
            self.open = None;
            ctx.request_repaint();
        } else if pressed_outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.open = None;
        }
    }
}

/// The text field under `pos` on the last frame, if any.
fn text_field_at(ctx: &Context, pos: Pos2) -> Option<Id> {
    let top = ctx.layer_id_at(pos)?;
    let ids: Vec<Id> = ctx.viewport(|v| {
        v.prev_pass.widgets.get_layer(top).filter(|w| w.interact_rect.contains(pos)).map(|w| w.id).collect()
    });
    ids.into_iter().rev().find(|id| TextEditState::load(ctx, *id).is_some())
}

fn select_all() -> Event {
    Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }
}

fn clipboard_text() -> String {
    arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default()
}
