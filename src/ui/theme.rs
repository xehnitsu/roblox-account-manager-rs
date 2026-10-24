//! App themes: the default grey look and classic themes based on the 2011
//! Roblox website stylesheet (colors from its CSS, see `Palette`).

use std::sync::Arc;

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Stroke, Style, Theme, ThemePreference, Visuals,
};

use crate::config::AppTheme;

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

struct Palette {
    dark: bool,
    page: Color32,
    window: Color32,
    alt_row: Color32,
    input: Color32,
    border: Color32,
    text: Color32,
    strong: Color32,
    link: Color32,
    button: Color32,
    button_border: Color32,
    button_text: Color32,
    button_hover: Color32,
    button_hover_text: Color32,
    button_active: Color32,
    selection: Color32,
    selection_text: Color32,
    row_hover: Color32,
    nav: Color32,
    nav_selected: Color32,
    nav_hover: Color32,
    nav_hover_text: Color32,
    drag_line: Color32,
}

/// From the 2011 stylesheet: page `#F8FCFF` (body), `.Button` white
/// with a `#333` border turning `#6E99C9` with white text on hover, `.Grid`
/// border `#CCC` with `#EEE` alternate rows and `#DDD` hover, `#00F` links, and
/// the `#6E99C9` navigation bar. Selection is `#B6CCE4`, the selected tab
/// `#435D77`.
const CLASSIC: Palette = Palette {
    dark: false,
    page: hex(0xF8FCFF),
    window: hex(0xFFFFFF),
    alt_row: hex(0xEEEEEE),
    input: hex(0xFFFFFF),
    border: hex(0xCCCCCC),
    text: hex(0x333333),
    strong: hex(0x000000),
    link: hex(0x0000FF),
    button: hex(0xFFFFFF),
    button_border: hex(0x333333),
    button_text: hex(0x333333),
    button_hover: hex(0x6E99C9),
    button_hover_text: hex(0xFFFFFF),
    button_active: hex(0x5A85B5),
    selection: hex(0xB6CCE4),
    selection_text: hex(0x000066),
    row_hover: hex(0xDDDDDD),
    nav: hex(0x6E99C9),
    nav_selected: hex(0x435D77),
    nav_hover: hex(0xB6CCE4),
    nav_hover_text: hex(0x435D77),
    drag_line: hex(0x464646),
};

/// The site had no dark mode. This uses its dark elements:
/// `.StandardBoxLightHeader` `#3B526B`, `.DGB_Button` `#435D77` with a `#777`
/// border turning `#B6CCE4` / `#435D77` on hover, and the `#6E99C9` accent.
const CLASSIC_DARK: Palette = Palette {
    dark: true,
    page: hex(0x1B2530),
    window: hex(0x212D3A),
    alt_row: hex(0x222E3B),
    input: hex(0x141B23),
    border: hex(0x3B526B),
    text: hex(0xD8E2EE),
    strong: hex(0xFFFFFF),
    link: hex(0x8FB6E6),
    button: hex(0x435D77),
    button_border: hex(0x777777),
    button_text: hex(0xFFFFFF),
    button_hover: hex(0xB6CCE4),
    button_hover_text: hex(0x435D77),
    button_active: hex(0x6E99C9),
    selection: hex(0x3B526B),
    selection_text: hex(0xFFFFFF),
    row_hover: hex(0x2A3A4C),
    nav: hex(0x3B526B),
    nav_selected: hex(0x6E99C9),
    nav_hover: hex(0xB6CCE4),
    nav_hover_text: hex(0x435D77),
    drag_line: hex(0x8A9BB0),
};

fn palette(theme: AppTheme) -> Option<&'static Palette> {
    match theme {
        AppTheme::Default => None,
        AppTheme::Classic => Some(&CLASSIC),
        AppTheme::ClassicDark => Some(&CLASSIC_DARK),
    }
}

fn theme_id() -> egui::Id {
    egui::Id::new("app_theme")
}

fn current(ctx: &egui::Context) -> AppTheme {
    ctx.data(|d| d.get_temp(theme_id())).unwrap_or_default()
}

pub fn apply(ctx: &egui::Context, theme: AppTheme) {
    ctx.data_mut(|d| d.insert_temp(theme_id(), theme));
    let mut fonts = FontDefinitions::default();
    match palette(theme) {
        None => {
            ctx.set_theme(ThemePreference::System);
            ctx.set_style_of(Theme::Dark, grey_selection(Visuals::dark(), 74, 240));
            ctx.set_style_of(Theme::Light, grey_selection(Visuals::light(), 200, 30));
        }
        Some(p) => {
            let (egui_theme, preference) =
                if p.dark { (Theme::Dark, ThemePreference::Dark) } else { (Theme::Light, ThemePreference::Light) };
            ctx.set_theme(preference);
            ctx.set_style_of(egui_theme, Style { visuals: classic_visuals(p), ..Default::default() });
            // The site used Verdana (Arial fallback). egui's fonts stay last for symbols.
            let dir = std::env::var_os("WINDIR").map(|w| std::path::PathBuf::from(w).join("Fonts"));
            for (name, file) in [("verdana", "verdana.ttf"), ("arial", "arial.ttf")].into_iter().rev() {
                if let Some(bytes) = dir.as_ref().and_then(|d| std::fs::read(d.join(file)).ok()) {
                    fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                    if let Some(family) = fonts.families.get_mut(&FontFamily::Proportional) {
                        family.insert(0, name.into());
                    }
                }
            }
        }
    }
    ctx.set_fonts(fonts);
    // Match the Windows title bar.
    let title_bar = match palette(theme) {
        None => egui::SystemTheme::SystemDefault,
        Some(p) if p.dark => egui::SystemTheme::Dark,
        Some(_) => egui::SystemTheme::Light,
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(title_bar));
}

fn grey_selection(mut visuals: Visuals, bg: u8, fg: u8) -> Style {
    visuals.selection.bg_fill = Color32::from_gray(bg);
    visuals.selection.stroke.color = Color32::from_gray(fg);
    Style { visuals, ..Default::default() }
}

fn classic_visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark { Visuals::dark() } else { Visuals::light() };
    // The 2011 site had square corners.
    let square = CornerRadius::ZERO;
    v.panel_fill = p.page;
    v.window_fill = p.window;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.faint_bg_color = p.alt_row;
    v.extreme_bg_color = p.input;
    v.text_edit_bg_color = Some(p.input);
    v.code_bg_color = p.alt_row;
    v.hyperlink_color = p.link;
    v.selection.bg_fill = p.selection;
    v.selection.stroke = Stroke::new(1.0, p.selection_text);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.window;
    w.noninteractive.weak_bg_fill = p.window;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.bg_fill = p.button;
    w.inactive.weak_bg_fill = p.button;
    w.inactive.bg_stroke = Stroke::new(1.0, p.button_border);
    w.inactive.fg_stroke = Stroke::new(1.0, p.button_text);
    w.hovered.bg_fill = p.button_hover;
    w.hovered.weak_bg_fill = p.button_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.strong);
    w.hovered.fg_stroke = Stroke::new(1.5, p.button_hover_text);
    w.active.bg_fill = p.button_active;
    w.active.weak_bg_fill = p.button_active;
    w.active.bg_stroke = Stroke::new(1.0, p.strong);
    w.active.fg_stroke = Stroke::new(1.5, p.button_hover_text);
    w.open.bg_fill = p.button_hover;
    w.open.weak_bg_fill = p.button_hover;
    w.open.bg_stroke = Stroke::new(1.0, p.strong);
    w.open.fg_stroke = Stroke::new(1.0, p.button_hover_text);
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = square;
        s.expansion = 0.0;
    }
    // Plain labels keep the page text color. `strong` text is black or white.
    v.override_text_color = None;
    v.weak_text_color = Some(if p.dark { hex(0x8FA1B5) } else { hex(0x777777) });
    v
}

pub fn tab_bar_frame(ctx: &egui::Context) -> egui::Frame {
    let frame = egui::Frame::side_top_panel(&ctx.global_style());
    match palette(current(ctx)) {
        None => frame,
        Some(p) => frame.fill(p.nav).stroke(Stroke::NONE),
    }
}

pub fn style_tab_bar(ui: &mut egui::Ui) {
    let Some(p) = palette(current(ui.ctx())) else { return };
    let v = ui.visuals_mut();
    v.override_text_color = None;
    v.selection.bg_fill = p.nav_selected;
    v.selection.stroke.color = Color32::WHITE;
    v.weak_text_color = Some(hex(0xE1EAF3));
    let w = &mut v.widgets;
    w.noninteractive.fg_stroke.color = Color32::WHITE;
    w.inactive.weak_bg_fill = Color32::TRANSPARENT;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke.color = Color32::WHITE;
    for s in [&mut w.hovered, &mut w.active] {
        s.weak_bg_fill = p.nav_hover;
        s.bg_fill = p.nav_hover;
        s.bg_stroke = Stroke::NONE;
        s.fg_stroke.color = p.nav_hover_text;
    }
}

pub fn title_bar_colors(ctx: &egui::Context) -> (Color32, Color32) {
    match palette(current(ctx)) {
        Some(p) => (p.nav, Color32::WHITE),
        None => {
            let v = &ctx.global_style().visuals;
            (v.panel_fill, v.strong_text_color())
        }
    }
}

pub fn play_button(ctx: &egui::Context, text: String) -> egui::Button<'static> {
    match palette(current(ctx)) {
        None => egui::Button::new(text),
        Some(_) => egui::Button::new(egui::RichText::new(text).color(Color32::WHITE).strong())
            .fill(hex(0x2E9E3E))
            .stroke(Stroke::new(1.0, hex(0x1B6B27))),
    }
}

pub fn row_hover(ui: &egui::Ui) -> Color32 {
    palette(current(ui.ctx())).map_or(ui.visuals().widgets.hovered.weak_bg_fill, |p| p.row_hover)
}

pub fn drag_line(ui: &egui::Ui) -> Color32 {
    palette(current(ui.ctx())).map_or(Color32::from_gray(70), |p| p.drag_line)
}

/// Success green, readable on light and dark backgrounds.
pub fn success_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_rgb(80, 170, 90) } else { Color32::from_rgb(0x1e, 0x8a, 0x3c) }
}
