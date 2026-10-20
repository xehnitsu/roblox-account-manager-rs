//! Tiny in-memory log shown in the UI's bottom panel (also echoed to stderr),
//! plus `app.log` in the data folder so a freeze or crash leaves a trace.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_LINES: usize = 500;
/// The file starts over once it grows past this.
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

static LINES: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    crate::config::data_dir().join("app.log")
}

fn write_file(msg: &str) {
    let mut file = FILE.lock().unwrap_or_else(|e| e.into_inner());
    if file.is_none() {
        let path = path();
        let _ = std::fs::create_dir_all(crate::config::data_dir());
        let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_FILE_BYTES);
        *file = OpenOptions::new().create(true).append(!big).write(true).truncate(big).open(path).ok();
    }
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{} {msg}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"));
    }
}

pub fn log(msg: impl Into<String>) {
    let msg = msg.into();
    write_file(&msg);
    let line = format!("[{}] {msg}", chrono::Local::now().format("%H:%M:%S"));
    eprintln!("{line}");
    let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
    if lines.len() >= MAX_LINES {
        lines.pop_front();
    }
    lines.push_back(line);
}

/// Detail for troubleshooting: goes to the file only.
pub fn debug(msg: impl AsRef<str>) {
    write_file(msg.as_ref());
}

pub fn snapshot() -> Vec<String> {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).iter().cloned().collect()
}

pub fn clear() {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
}
