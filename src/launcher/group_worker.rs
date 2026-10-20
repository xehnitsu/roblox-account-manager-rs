//! Background thread for every window-group call that waits on a Roblox
//! client. Styling, tiling and moving a client's window all block until the
//! client handles them, and a busy client can take seconds, so none of it
//! runs on the UI thread. Each client gets its own mover thread that keeps
//! only the newest position, so a slow client skips ahead instead of working
//! through a backlog, and never holds up the others.
use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{IsWindow, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetWindowPos};

use super::group_bar;
use super::window_group::{self as wg, Cell};

/// Window handles are kept as isize so they can cross threads.
pub enum Task {
    /// Make these tool windows.
    Group(Vec<isize>),
    /// Tile these windows under the bar at this rect.
    Tile { hwnds: Vec<isize>, bar: Cell },
    /// Hand these windows back, then signal the sender if there is one.
    Release(Vec<isize>, Option<Sender<()>>),
}

/// (window, cell, resize too) for each window.
pub type Moves = Vec<(isize, Cell, bool)>;

/// What a Tile task produced: the windows, the bar rect and their cells.
pub type Tiled = (Vec<isize>, Cell, Vec<Cell>);

#[derive(Default)]
struct Pending {
    tasks: VecDeque<Task>,
}

struct Worker {
    pending: Mutex<Pending>,
    ready: Condvar,
    tiled: Mutex<Option<Tiled>>,
}

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

fn hwnds(v: &[isize]) -> Vec<HWND> {
    v.iter().map(|&h| hwnd(h)).collect()
}

fn worker() -> &'static Worker {
    static WORKER: OnceLock<Worker> = OnceLock::new();
    WORKER.get_or_init(|| {
        let _ = std::thread::Builder::new().name("group-worker".into()).spawn(|| run(WORKER.wait()));
        Worker { pending: Mutex::new(Pending::default()), ready: Condvar::new(), tiled: Mutex::new(None) }
    })
}

fn run(w: &Worker) {
    loop {
        let task = {
            let mut p = w.pending.lock().unwrap();
            loop {
                match p.tasks.pop_front() {
                    Some(t) => break t,
                    None => p = w.ready.wait(p).unwrap(),
                }
            }
        };
        match task {
            Task::Group(list) => {
                for h in hwnds(&list) {
                    wg::set_grouped(h, true);
                }
            }
            Task::Tile { hwnds: list, bar } => {
                let cells = wg::tile(&hwnds(&list), bar.x, bar.y + bar.h, bar.w);
                group_bar::update(&hwnds(&list), &cells, bar);
                *w.tiled.lock().unwrap() = Some((list, bar, cells));
            }
            Task::Release(list, done) => {
                wg::release(&hwnds(&list));
                if let Some(done) = done {
                    let _ = done.send(());
                }
            }
        }
    }
}

/// The newest position for one client's window.
#[derive(Default)]
struct Slot {
    next: Mutex<Option<(Cell, bool)>>,
    ready: Condvar,
}

/// Mover thread for one window. Ends a while after the window is gone.
fn run_mover(h: isize, slot: &Slot) {
    loop {
        let (c, sized) = {
            let mut next = slot.next.lock().unwrap();
            loop {
                if let Some(v) = next.take() {
                    break v;
                }
                let (g, timeout) = slot.ready.wait_timeout(next, Duration::from_secs(30)).unwrap();
                next = g;
                // SAFETY: IsWindow accepts any handle value.
                if timeout.timed_out() && !unsafe { IsWindow(Some(hwnd(h))) }.as_bool() {
                    // post_moves locks the map, then a slot, so let go of
                    // the slot first.
                    drop(next);
                    movers().lock().unwrap().remove(&h);
                    return;
                }
            }
        };
        let mut flags = SWP_NOZORDER | SWP_NOACTIVATE;
        if !sized {
            flags |= SWP_NOSIZE;
        }
        // SAFETY: SetWindowPos on a stale handle just fails.
        unsafe {
            let _ = SetWindowPos(hwnd(h), None, c.x, c.y, c.w, c.h, flags);
        }
    }
}

fn movers() -> &'static Mutex<HashMap<isize, Arc<Slot>>> {
    static MOVERS: OnceLock<Mutex<HashMap<isize, Arc<Slot>>>> = OnceLock::new();
    MOVERS.get_or_init(Default::default)
}

pub fn post(task: Task) {
    let w = worker();
    w.pending.lock().unwrap().tasks.push_back(task);
    w.ready.notify_one();
}

/// Replaces any move that hasn't run yet, per window.
pub fn post_moves(moves: Moves) {
    let mut map = movers().lock().unwrap();
    for (h, c, sized) in moves {
        let slot = map
            .entry(h)
            .or_insert_with(|| {
                let slot = Arc::new(Slot::default());
                let s = slot.clone();
                let _ = std::thread::Builder::new().name("group-mover".into()).spawn(move || run_mover(h, &s));
                slot
            })
            .clone();
        *slot.next.lock().unwrap() = Some((c, sized));
        slot.ready.notify_one();
    }
}

/// The result of the last Tile task, once.
pub fn take_tiled() -> Option<Tiled> {
    worker().tiled.lock().unwrap().take()
}

/// Hands windows back and waits up to `timeout`, so it's done before the app
/// exits.
pub fn release_and_wait(list: Vec<isize>, timeout: Duration) {
    let (tx, rx) = channel();
    post(Task::Release(list, Some(tx)));
    let _ = rx.recv_timeout(timeout);
}
