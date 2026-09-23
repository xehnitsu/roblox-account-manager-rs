//! Tiny in-memory log shown in the UI's bottom panel (also echoed to stderr).

use std::collections::VecDeque;
use std::sync::Mutex;

const MAX_LINES: usize = 500;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn log(msg: impl Into<String>) {
    let msg = msg.into();
    let line = format!("[{}] {msg}", chrono::Local::now().format("%H:%M:%S"));
    eprintln!("{line}");
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    if lines.len() >= MAX_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
}

pub fn snapshot() -> Vec<String> {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
