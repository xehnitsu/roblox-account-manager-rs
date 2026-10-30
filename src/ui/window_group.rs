//! Group handle: a thin frameless bar above the tiled client windows. Clients become tool
//! windows, so alt-tab and the taskbar list only the bar, and they hide, show and raise with it.
//! Syncing runs from the main window's frame because egui doesn't run a
//! minimized viewport's UI. Dragging and the alt-tab picture are handled by a
//! hook on the bar's window, see `launcher::group_bar`.
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;

use crate::app::Shared;
use crate::applog::{debug, log};
use crate::launcher::group_bar;
use crate::launcher::group_watch::{self, Watch};
use crate::launcher::group_worker::{self, Task};
use crate::launcher::window_group::{self as wg, Cell, columns};

const TITLE: &str = "Client group";
const BAR_HEIGHT: f32 = 22.0;
const RESCAN: Duration = Duration::from_secs(1);
const TICK: Duration = Duration::from_millis(100);

fn raw(hwnds: &[HWND]) -> Vec<isize> {
    hwnds.iter().map(|h| h.0 as isize).collect()
}

fn viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("client_group")
}

/// What the windows were last arranged for, so they move only on change.
#[derive(PartialEq)]
struct Applied {
    bar: Cell,
    hwnds: Vec<isize>,
}

#[derive(Default)]
struct Clicks {
    ungroup: bool,
    minimize: bool,
    tile: bool,
    fill: bool,
    resize: bool,
}

pub struct Group {
    ids: Vec<u64>,
    /// (pid, window) per client in tile order.
    members: Vec<(u32, HWND)>,
    bar: Option<HWND>,
    scanned: Option<Instant>,
    applied: Option<Applied>,
    /// Where `tile` last put each window.
    cells: Vec<Cell>,
    force: bool,
    tiling: bool,
    bar_was_min: bool,
    /// Bar size to restore after the one-pixel repaint nudge.
    nudged: Option<egui::Vec2>,
    last_fg: HWND,
    /// Grid columns when created. Sets the bar's starting size.
    start_cols: f32,
    /// Bar width per column, in points. Dragging the bar's right edge changes it.
    column_width: f32,
    /// Where the bar was before a double-click filled the screen with it, and
    /// where the fill put it.
    before_fill: Option<(Cell, Cell)>,
    /// The user dragged the bar's edge, so save its width after the next tile.
    resized: bool,
    watch: Watch,
}

impl Group {
    pub fn new(ids: Vec<u64>, column_width: f32) -> Self {
        let start_cols = columns(ids.len()) as f32;
        log(format!("Client group: grouping {} sessions", ids.len()));
        Self {
            ids,
            members: Vec::new(),
            bar: None,
            scanned: None,
            applied: None,
            cells: Vec::new(),
            force: false,
            tiling: false,
            bar_was_min: false,
            nudged: None,
            last_fg: HWND::default(),
            start_cols,
            column_width,
            before_fill: None,
            resized: false,
            watch: Watch::start(),
        }
    }

    fn hwnds(&self) -> Vec<HWND> {
        self.members.iter().map(|&(_, h)| h).collect()
    }

    fn rescan(&mut self, shared: &Shared) {
        if self.scanned.is_some_and(|t| t.elapsed() < RESCAN) {
            return;
        }
        self.scanned = Some(Instant::now());

        let bar = wg::own_window(TITLE);
        if let Some(b) = bar
            && self.bar != bar
        {
            debug(format!("group: bar window {:#x}", b.0 as isize));
            wg::square_corners(b, true);
            group_bar::install(b);
        }
        self.bar = bar;

        // Keep known windows instead of searching again, so a minimized client isn't mistaken for another window.
        let members: Vec<(u32, HWND)> = self
            .ids
            .iter()
            .filter_map(|&id| shared.session(id)?.process)
            .filter_map(|(pid, _)| {
                let known = self.members.iter().find(|&&(p, h)| p == pid && wg::alive(h, pid));
                Some((pid, known.map(|&(_, h)| h).or_else(|| wg::main_window(pid))?))
            })
            .collect();
        let dropped: Vec<HWND> = self.members.iter().filter(|m| !members.contains(m)).map(|&(_, h)| h).collect();
        if !dropped.is_empty() {
            group_worker::post(Task::Release(raw(&dropped), None));
        }
        if members.len() != self.members.len() {
            log(format!("Client group: {} windows", members.len()));
            // A new client may sit behind other windows.
            if let Some(b) = self.bar {
                let list: Vec<HWND> = members.iter().map(|&(_, h)| h).collect();
                wg::raise_under(b, &list);
            }
        }

        // Roblox moved or resized a window itself.
        let same = members == self.members;
        self.members = members;
        let bar_min = self.bar.is_some_and(wg::is_minimized);
        if same && !bar_min && !self.tiling && !group_bar::moving() && wg::drifted(&self.hwnds(), &self.cells) {
            debug("group: a window left its cell, re-tiling");
            self.force = true;
        }
        // Capture the grid while it's in front, for alt-tab.
        if let Some(b) = self.bar
            && !bar_min
        {
            let fg = wg::foreground();
            if fg == b || self.members.iter().any(|&(_, h)| h == fg) {
                group_bar::snapshot();
            }
        }
        let mut targets: Vec<(String, HWND)> = self.bar.map(|b| ("bar".to_owned(), b)).into_iter().collect();
        targets.extend(self.members.iter().map(|&(pid, h)| (format!("client {pid}"), h)));
        self.watch.set_targets(targets);
        group_worker::post(Task::Group(raw(&self.hwnds())));
    }

    /// A restored OpenGL window can stay blank until resized, so nudge the bar by a pixel and back.
    fn sync_restore(&mut self, ctx: &egui::Context) {
        if let Some(size) = self.nudged.take() {
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::InnerSize(size));
        }
        let min = self.bar.is_some_and(wg::is_minimized);
        if min != self.bar_was_min {
            debug(if min { "group: bar minimized" } else { "group: bar restored, nudging it to repaint" });
        }
        if self.bar_was_min && !min {
            self.force = true;
            if let Some(r) = ctx.input_for(viewport_id(), |i| i.viewport().inner_rect) {
                let size = r.size();
                ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::InnerSize(size + egui::vec2(1.0, 0.0)));
                self.nudged = Some(size);
            }
            ctx.request_repaint_of(viewport_id());
        }
        self.bar_was_min = min;
    }

    /// Focusing any group window raises the rest.
    fn sync_focus(&mut self) {
        let fg = wg::foreground();
        let mut all = self.hwnds();
        all.extend(self.bar);
        if !self.bar_was_min && fg != self.last_fg && all.contains(&fg) {
            debug(format!("group: {:#x} focused, raising the group", fg.0 as isize));
            wg::raise_under(fg, &all);
        }
        self.last_fg = fg;
    }

    fn sync_layout(&mut self, ctx: &egui::Context, shared: &Shared) {
        if let Some((list, b, cells)) = group_worker::take_tiled() {
            self.tiling = false;
            debug(format!("group: tiled under bar {b:?}: {cells:?}"));
            // Roblox wouldn't shrink that far, so match the bar to the grid.
            let real = wg::grid_width(&cells);
            let ppp = ctx.input_for(viewport_id(), |i| i.viewport().native_pixels_per_point).unwrap_or(1.0);
            if real > b.w {
                let size = egui::vec2(real as f32 / ppp, BAR_HEIGHT);
                ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::InnerSize(size));
            }
            // Remember a size the user picked for the next group, but not one
            // set by a double-click or by Roblox's minimum window size.
            let native = ctx.input_for(viewport_id(), |i| i.viewport().native_pixels_per_point);
            let cols = columns(cells.len()).max(1) as f32;
            let resize_done = self.resized && !group_bar::moving();
            if resize_done {
                self.resized = false;
            }
            if resize_done
                && let Some(ppp) = native
                && !cells.is_empty()
                && real <= b.w
            {
                let width = b.w as f32 / ppp / cols;
                self.column_width = width;
                shared.config.lock().unwrap().group_column_width = width;
                shared.save_config();
            }
            if list == raw(&self.hwnds()) {
                self.cells = cells;
            }
        }
        let Some(bar) = self.bar else { return };
        if wg::is_minimized(bar) || self.nudged.is_some() || group_bar::moving() {
            return;
        }
        let hwnds = self.hwnds();
        let now = Applied { bar: wg::rect(bar), hwnds: hwnds.iter().map(|h| h.0 as isize).collect() };
        if !self.force && self.applied.as_ref() == Some(&now) {
            return;
        }
        self.force = false;
        self.tiling = true;
        group_worker::post(Task::Tile { hwnds: raw(&hwnds), bar: now.bar });
        self.applied = Some(now);
    }

    /// Shows the bar and syncs the group. Returns false once ungrouped.
    pub fn show(&mut self, ctx: &egui::Context, shared: &Arc<Shared>) -> bool {
        group_watch::beat();
        self.rescan(shared);
        self.sync_restore(ctx);
        self.sync_focus();
        self.sync_layout(ctx, shared);
        ctx.request_repaint_after(TICK);

        let clicks = self.show_bar(ctx);
        if clicks.tile {
            self.scanned = None;
            self.force = true;
        }
        if clicks.minimize {
            debug("group: minimize clicked");
            group_bar::snapshot();
            ctx.send_viewport_cmd_to(viewport_id(), egui::ViewportCommand::Minimized(true));
        }
        self.resized |= clicks.resize;
        if clicks.fill {
            self.toggle_fill();
        }
        !clicks.ungroup
    }

    /// Double-clicking the bar moves it to the top of its screen at full width,
    /// or as wide as fits the grid's height. Double-clicking again, without
    /// moving it in between, puts it back.
    fn toggle_fill(&mut self) {
        let Some(bar) = self.bar else { return };
        let now = wg::rect(bar);
        // Moved since the fill: fill again instead of going back.
        let filled = self.before_fill.take().filter(|&(_, at)| (at.x, at.y) == (now.x, now.y));
        let to = match filled {
            Some((before, _)) => before,
            None => {
                let Some(area) = wg::work_area(bar) else { return };
                let n = self.members.len().max(1);
                let cols = columns(n) as f32;
                let rows = n.div_ceil(columns(n)) as f32;
                // Cells are 4:3, so the grid is rows * 0.75 * width / cols tall.
                let fits = (area.h - now.h) as f32 * cols / (rows * 0.75);
                let w = (area.w as f32).min(fits) as i32;
                let at = Cell { x: area.x + (area.w - w) / 2, y: area.y, w, h: now.h };
                self.before_fill = Some((now, at));
                at
            }
        };
        debug(format!("group: double-click, bar to {to:?}"));
        wg::place(bar, Cell { h: now.h, ..to });
        self.force = true;
    }

    fn show_bar(&self, ctx: &egui::Context) -> Clicks {
        let n = self.members.len();
        // Never open wider than the screen, whatever was saved.
        let screen = ctx.input(|i| i.viewport().monitor_size).map_or(f32::INFINITY, |s| s.x - 80.0);
        let width = (self.start_cols * self.column_width).min(screen).max(200.0);
        let builder = egui::ViewportBuilder::default()
            .with_title(TITLE)
            .with_decorations(false)
            .with_position([40.0, 40.0])
            .with_inner_size([width, BAR_HEIGHT])
            .with_min_inner_size([200.0, BAR_HEIGHT])
            .with_max_inner_size([100_000.0, BAR_HEIGHT])
            .with_resizable(true);

        ctx.show_viewport_immediate(viewport_id(), builder, |ui, _| {
            let mut clicks = Clicks { ungroup: ui.input(|i| i.viewport().close_requested()), ..Default::default() };
            let frame = egui::Frame::NONE.fill(ui.visuals().panel_fill).inner_margin(egui::Margin::symmetric(6, 0));
            egui::CentralPanel::default().frame(frame).show(ui, |ui| {
                // Added first so buttons on top still get clicks.
                // Click and drag, so a drag starts only once the mouse moves
                // and a double-click still gets through.
                let bar = ui.interact(ui.max_rect(), ui.id().with("drag"), egui::Sense::click_and_drag());
                // Any other button would start a move that only a left click ends.
                if bar.drag_started_by(egui::PointerButton::Primary) {
                    ui.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                clicks.fill = bar.double_clicked();
                // The right edge resizes the bar, and the grid follows its width.
                let full = ui.max_rect().expand2(egui::vec2(6.0, 0.0));
                let edge = egui::Rect::from_x_y_ranges(full.right() - 6.0..=full.right(), full.y_range());
                let grip = ui.interact(edge, ui.id().with("resize"), egui::Sense::drag());
                let grip = grip.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                clicks.resize = grip.drag_started_by(egui::PointerButton::Primary);
                if clicks.resize {
                    ui.send_viewport_cmd(egui::ViewportCommand::BeginResize(egui::viewport::ResizeDirection::East));
                }
                ui.horizontal_centered(|ui| {
                    match n {
                        0 => ui.weak("No client windows"),
                        1 => ui.weak("1 client"),
                        _ => ui.weak(format!("{n} clients")),
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        clicks.ungroup |= ui
                            .small_button("×")
                            .on_hover_text("Ungroup: give the windows their title bars back where they are")
                            .clicked();
                        clicks.minimize = ui.small_button("–").on_hover_text("Minimize all").clicked();
                        clicks.tile =
                            ui.small_button("Tile").on_hover_text("Put every window back in its cell").clicked();
                    });
                });
            });
            clicks
        })
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        log("Client group: ungrouped");
        if let Some(b) = self.bar {
            group_bar::uninstall(b);
        }
        group_worker::release_and_wait(raw(&self.hwnds()), Duration::from_secs(2));
    }
}
