//! Task Manager style lists (painted header, full-width rows) shared by the
//! accounts, sessions and servers tabs.

use std::collections::HashSet;
use std::hash::Hash;

#[derive(Clone, Copy)]
pub enum Col {
    Fixed(f32),
    /// Share of the width left after fixed columns.
    Fill(f32),
}

/// (left, right) span of each column inside `rect`.
pub fn spans(rect: egui::Rect, cols: &[Col]) -> Vec<(f32, f32)> {
    let fixed: f32 = cols.iter().map(|c| if let Col::Fixed(w) = c { *w } else { 0.0 }).sum();
    let weight: f32 = cols.iter().map(|c| if let Col::Fill(w) = c { *w } else { 0.0 }).sum();
    let spare = (rect.width() - fixed).max(0.0);
    let mut x = rect.left();
    cols.iter()
        .map(|c| {
            let w = match c {
                Col::Fixed(w) => *w,
                Col::Fill(f) if weight > 0.0 => spare * f / weight,
                Col::Fill(_) => 0.0,
            };
            let span = (x, x + w);
            x += w;
            span
        })
        .collect()
}

const PAD: egui::Vec2 = egui::vec2(6.0, 3.0);

/// Column titles with a separator below.
pub fn header(ui: &mut egui::Ui, cols: &[Col], titles: &[&str]) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::hover());
    let font = egui::TextStyle::Body.resolve(ui.style());
    let strong = ui.visuals().strong_text_color();
    for ((left, _), title) in spans(rect.shrink2(egui::vec2(PAD.x, 0.0)), cols).into_iter().zip(titles) {
        ui.painter().text(egui::pos2(left, rect.center().y), egui::Align2::LEFT_CENTER, *title, font.clone(), strong);
    }
    ui.separator();
}

/// Allocates a row and paints its background. Returns the padded-in rect and the response.
pub fn row(
    ui: &mut egui::Ui,
    index: usize,
    height: f32,
    selected: bool,
    sense: egui::Sense,
    hover: bool,
) -> (egui::Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), sense);
    let visuals = ui.visuals();
    let bg = if selected {
        visuals.selection.bg_fill
    } else if hover && resp.hovered() {
        super::theme::row_hover(ui)
    } else if index % 2 == 1 {
        visuals.faint_bg_color
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 2.0, bg);
    (rect.shrink2(PAD), resp)
}

/// Text color for a row's cells.
pub fn text_color(ui: &egui::Ui, selected: bool) -> egui::Color32 {
    if selected { ui.visuals().selection.stroke.color } else { ui.visuals().text_color() }
}

/// Non-selectable, truncated label in `left..right`.
pub fn cell(ui: &mut egui::Ui, inner: egui::Rect, (left, right): (f32, f32), text: egui::RichText) {
    let cell = egui::Rect::from_x_y_ranges(left..=(right - 8.0).max(left), inner.y_range());
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    // A child ui takes no space in the list, so the row keeps its height.
    let mut cell_ui = ui.new_child(egui::UiBuilder::new().max_rect(cell).layout(layout));
    cell_ui.add(egui::Label::new(text).selectable(false).truncate());
}

pub fn cells(ui: &mut egui::Ui, inner: egui::Rect, spans: &[(f32, f32)], texts: &[&str], selected: bool) {
    let color = text_color(ui, selected);
    for (span, text) in spans.iter().zip(texts) {
        cell(ui, inner, *span, egui::RichText::new(*text).color(color));
    }
}

/// Explorer-style selection: click selects one, Ctrl toggles, Shift selects a range from `anchor`.
pub fn click<T: Copy + Eq + Hash>(
    selected: &mut HashSet<T>,
    anchor: &mut Option<T>,
    id: T,
    order: &[T],
    ctrl: bool,
    shift: bool,
) {
    let pos = |id: T| order.iter().position(|r| *r == id);
    if shift && let Some((a, b)) = anchor.and_then(pos).zip(pos(id)) {
        if !ctrl {
            selected.clear();
        }
        selected.extend(order[a.min(b)..=a.max(b)].iter().copied());
        return;
    }
    if ctrl {
        if !selected.remove(&id) {
            selected.insert(id);
        }
    } else {
        selected.clear();
        selected.insert(id);
    }
    *anchor = Some(id);
}

/// Right-click on an unselected row selects only it.
pub fn select_for_menu<T: Copy + Eq + Hash>(selected: &mut HashSet<T>, anchor: &mut Option<T>, id: T) {
    if !selected.contains(&id) {
        selected.clear();
        selected.insert(id);
        *anchor = Some(id);
    }
}

/// Ctrl+A selects all rows unless a text field has focus.
pub fn select_all_pressed(ui: &egui::Ui) -> bool {
    !ui.ctx().egui_wants_keyboard_input() && ui.input(|i| i.modifiers.command && i.key_pressed(egui::Key::A))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_spans() {
        let r = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 10.0));
        let s = spans(r, &[Col::Fixed(100.0), Col::Fill(1.0), Col::Fill(3.0)]);
        assert_eq!(s, [(0.0, 100.0), (100.0, 150.0), (150.0, 300.0)]);
    }

    #[test]
    fn click_selection() {
        let order = [1, 2, 3, 4];
        let (mut sel, mut anchor) = (HashSet::new(), None);
        click(&mut sel, &mut anchor, 2, &order, false, false);
        click(&mut sel, &mut anchor, 4, &order, false, true);
        assert_eq!(sel, HashSet::from([2, 3, 4]));
        click(&mut sel, &mut anchor, 3, &order, true, false);
        assert_eq!(sel, HashSet::from([2, 4]));
    }
}
