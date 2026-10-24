//! Reads each Roblox client's own log file (`%LOCALAPPDATA%\Roblox\logs`) to
//! learn, without asking Roblox's servers, when it joined a game, which
//! server, and why it left one: kicked, lost connection, server shut down,
//! idle, or the player left on purpose.
//!
//! A log belongs to the client whose threads wrote it: each line carries the
//! writing thread's ID, and Windows says which process owns that thread.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};

/// Read at most this much new log per pass.
const MAX_READ: u64 = 8 * 1024 * 1024;
/// Only logs written this recently can belong to a running client.
const RECENT: Duration = Duration::from_secs(6 * 60 * 60);

pub fn logs_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Roblox").join("logs"))
}

/// What a log line says happened.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Joined {
        place_id: u64,
        job_id: String,
    },
    Teleporting,
    /// The player left the game themselves (back to the Roblox home screen).
    Left,
    Disconnected(u32),
    /// The game's kick message, when it logs one.
    KickMessage(String),
}

/// Why a client dropped out of a game, from a disconnect code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drop {
    Kicked,
    ServerShutDown,
    LoggedInElsewhere,
    LostConnection,
    Idle,
    Other(u32),
}

impl Drop {
    /// None for the code a client logs whenever it leaves on its own (285).
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            285 => return None,
            267 => Drop::Kicked,
            274 | 275 => Drop::ServerShutDown,
            264 | 273 => Drop::LoggedInElsewhere,
            260..=262 | 266 | 277 | 279 => Drop::LostConnection,
            278 => Drop::Idle,
            c => Drop::Other(c),
        })
    }

    pub fn describe(self) -> String {
        match self {
            Drop::Kicked => "kicked".into(),
            Drop::ServerShutDown => "server shut down".into(),
            Drop::LoggedInElsewhere => "logged in somewhere else".into(),
            Drop::LostConnection => "lost connection".into(),
            Drop::Idle => "kicked for being idle".into(),
            Drop::Other(c) => format!("disconnected (code {c})"),
        }
    }
}

/// Parses one log line.
pub fn classify(line: &str) -> Option<Event> {
    if let Some(rest) = line.split("! Joining game '").nth(1) {
        let (job_id, rest) = rest.split_once('\'')?;
        let place_id = rest.trim_start().strip_prefix("place ")?.split_whitespace().next()?.parse().ok()?;
        return Some(Event::Joined { place_id, job_id: job_id.to_owned() });
    }
    if line.contains("doTeleport:")
        || line.contains("finishTeleportWithJoinScriptPayload")
        || line.contains("[FLog::SessionTransitionFSM] Teleported.")
    {
        return Some(Event::Teleporting);
    }
    if line.contains("leaveUGCGameInternal") || line.contains("returnToLuaApp") {
        return Some(Event::Left);
    }
    if let Some(msg) = line.split("kicked from this experience:").nth(1) {
        return Some(Event::KickMessage(msg.trim().to_owned()));
    }
    for marker in [
        "Disconnection Notification. Reason: ",
        "Sending disconnect with reason: ",
        "Disconnected from server for reason: Player: ",
        "setting replicator disconnect reason to ",
        "Error Code: ",
    ] {
        if let Some(rest) = line.split(marker).nth(1) {
            let code: u32 = rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()?;
            if (256..=299).contains(&code) {
                return Some(Event::Disconnected(code));
            }
        }
    }
    None
}

/// Thread ID (hex) of a log line: `<time>,<seconds>,<thread>,<level>...`.
fn line_thread(line: &str) -> Option<u32> {
    let mut parts = line.splitn(4, ',');
    parts.next()?;
    parts.next()?;
    u32::from_str_radix(parts.next()?, 16).ok()
}

/// Owning process of every thread on the system.
fn thread_owners() -> HashMap<u32, u32> {
    let mut owners = HashMap::new();
    // SAFETY: standard Toolhelp snapshot walk, handle closed below.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else { return owners };
        let mut entry = THREADENTRY32 { dwSize: std::mem::size_of::<THREADENTRY32>() as u32, ..Default::default() };
        if Thread32First(snap, &mut entry).is_ok() {
            loop {
                owners.insert(entry.th32ThreadID, entry.th32OwnerProcessID);
                if Thread32Next(snap, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    owners
}

/// The log of each of `pids` that can be found. Logs already claimed are skipped.
pub fn find_logs(pids: &[u32], claimed: &[PathBuf]) -> HashMap<u32, PathBuf> {
    let mut found = HashMap::new();
    let Some(dir) = logs_dir() else { return found };
    let Ok(entries) = std::fs::read_dir(&dir) else { return found };
    let now = SystemTime::now();
    let mut logs: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.contains("_Player_") && !name.contains("CrashHandler") && name.ends_with(".log")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(t, p)| now.duration_since(*t).unwrap_or_default() < RECENT && !claimed.contains(p))
        .collect();
    logs.sort_by_key(|a| std::cmp::Reverse(a.0));
    let owners = thread_owners();
    for (_, path) in logs {
        if found.len() == pids.len() {
            break;
        }
        let Some(pid) = log_owner(&path, &owners) else { continue };
        if pids.contains(&pid) && !found.contains_key(&pid) {
            found.insert(pid, path);
        }
    }
    found
}

/// The process that wrote the start of a log, from its lines' thread IDs.
fn log_owner(path: &Path, owners: &HashMap<u32, u32>) -> Option<u32> {
    let mut head = vec![0; 16 * 1024];
    let n = File::open(path).ok()?.read(&mut head).ok()?;
    let head = String::from_utf8_lossy(&head[..n]);
    head.lines().filter_map(line_thread).find_map(|t| owners.get(&t).copied())
}

/// Reads a log a bit at a time, returning only complete new lines.
#[derive(Debug, Clone)]
pub struct Reader {
    pub path: PathBuf,
    offset: u64,
}

impl Reader {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0 }
    }

    pub fn read_new(&mut self) -> Vec<String> {
        let Ok(mut file) = File::open(&self.path) else { return Vec::new() };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            self.offset = 0;
        }
        if len == self.offset || file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if file.take((len - self.offset).min(MAX_READ)).read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        // Keep a half-written last line for next time.
        let Some(end) = buf.iter().rposition(|&b| b == b'\n') else { return Vec::new() };
        self.offset += end as u64 + 1;
        String::from_utf8_lossy(&buf[..end]).lines().map(str::to_owned).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_real_lines() {
        let join = "2026-10-19T15:13:32.248Z,1.248207,7b18,6 [FLog::Output] ! Joining game 'aa677461-605c-45f0-8b4f-af5aca5a056b' place 7041939546 at 10.17.1.77";
        assert_eq!(
            classify(join),
            Some(Event::Joined { place_id: 7041939546, job_id: "aa677461-605c-45f0-8b4f-af5aca5a056b".into() })
        );
        let idle = "2026-10-19T15:37:03.993Z,1412.993530,7884,6,Info [DFLog::NetworkClient] Client:Disconnect setting replicator disconnect reason to 278";
        assert_eq!(classify(idle), Some(Event::Disconnected(278)));
        let own = "2026-10-19T15:37:04.023Z,1413.023438,3eec,6,Info [DFLog::RbxTransportDummyClient] Disconnected from server for reason: Player: 285 (DisconnectClientInitiated)";
        assert_eq!(classify(own), Some(Event::Disconnected(285)));
        let left = "2026-10-19T15:49:12.218Z,2141.218018,01c4,6 [FLog::SingleSurfaceApp] leaveUGCGameInternal";
        assert_eq!(classify(left), Some(Event::Left));
        assert_eq!(
            classify("2026-10-19T15:49:12.222Z,2141.222168,01c4,6,Info [DFLog::NetworkClient] Client:Disconnect"),
            None
        );
        assert_eq!(line_thread(join), Some(0x7b18));
    }

    #[test]
    fn disconnect_codes() {
        assert_eq!(Drop::from_code(285), None);
        assert_eq!(Drop::from_code(267), Some(Drop::Kicked));
        assert_eq!(Drop::from_code(277), Some(Drop::LostConnection));
        assert_eq!(Drop::from_code(278), Some(Drop::Idle));
        assert_eq!(Drop::from_code(290), Some(Drop::Other(290)));
    }
}
