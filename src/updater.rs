//! Updates the app from its GitHub releases. The new exe is downloaded next
//! to the running one, checked, and swapped in: Windows lets a running exe be
//! renamed, so the old one moves aside to `<exe>.old` and the new one takes
//! its name. The update takes effect on the next start, and that start
//! deletes the old exe.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context as _, Result, bail, ensure};
use reqwest::Client;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use crate::applog::log;

const LATEST_URL: &str = "https://api.github.com/repos/xehnitsu/roblox-account-manager-rs/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Command-line flag for a restarted app: wait for this PID to exit first, so
/// the old instance has let go of the Roblox mutex.
pub const WAIT_FLAG: &str = "--wait-for";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading(String),
    Ready(String),
    Failed(String),
}

fn status_cell() -> &'static Mutex<Status> {
    static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
    STATUS.get_or_init(|| Mutex::new(Status::Idle))
}

pub fn status() -> Status {
    status_cell().lock().unwrap().clone()
}

fn set_status(s: Status, ctx: &egui::Context) {
    *status_cell().lock().unwrap() = s;
    ctx.request_repaint();
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// `sha256:<hex>`, filled in by GitHub.
    digest: Option<String>,
}

fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("roblox-account-manager-rs/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(120))
            .build()
            .expect("HTTP client")
    })
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok());
    let v = (parts.next()??, parts.next()??, parts.next()??);
    parts.next().is_none().then_some(v)
}

fn is_newer(tag: &str) -> bool {
    matches!((parse_version(tag), parse_version(CURRENT)), (Some(new), Some(cur)) if new > cur)
}

fn old_path(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

pub fn remove_old() {
    let Ok(exe) = std::env::current_exe() else { return };
    let old = old_path(&exe);
    if old.exists() {
        match std::fs::remove_file(&old) {
            Ok(()) => log(format!("Updated to v{CURRENT}")),
            Err(e) => log(format!("could not delete {}: {e}", old.display())),
        }
    }
}

/// Checks for a newer release and installs it. Debug builds only check, so
/// a dev build is never replaced.
pub async fn run(ctx: egui::Context) {
    if matches!(status(), Status::Checking | Status::Downloading(_) | Status::Ready(_)) {
        return;
    }
    set_status(Status::Checking, &ctx);
    let result = check_and_install(&ctx).await;
    let s = match result {
        Ok(Some(tag)) => Status::Ready(tag),
        Ok(None) => Status::UpToDate,
        Err(e) => {
            log(format!("Update failed: {e:#}"));
            Status::Failed(format!("{e:#}"))
        }
    };
    set_status(s, &ctx);
}

async fn check_and_install(ctx: &egui::Context) -> Result<Option<String>> {
    let release: Release = client()
        .get(LATEST_URL)
        .send()
        .await?
        .error_for_status()
        .context("asking GitHub for the latest release")?
        .json()
        .await?;
    if !is_newer(&release.tag_name) {
        return Ok(None);
    }
    let tag = release.tag_name;
    let asset = release
        .assets
        .into_iter()
        .find(|a| a.name.starts_with("RobloxAccountManager-") && a.name.to_lowercase().ends_with(".exe"))
        .with_context(|| format!("release {tag} has no exe"))?;
    if cfg!(debug_assertions) {
        log(format!("Update {tag} is available (debug build, not installing)"));
        return Ok(None);
    }
    log(format!("Downloading update {tag}"));
    set_status(Status::Downloading(tag.clone()), ctx);

    let bytes = client().get(&asset.browser_download_url).send().await?.error_for_status()?.bytes().await?;
    ensure!(bytes.len() as u64 == asset.size, "download is {} bytes, expected {}", bytes.len(), asset.size);
    ensure!(bytes.starts_with(b"MZ"), "download is not an exe");
    let want = asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).context("release has no checksum")?;
    let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    ensure!(got.eq_ignore_ascii_case(want), "download checksum doesn't match the release");

    let exe = std::env::current_exe().context("finding the app's exe")?;
    tokio::task::spawn_blocking(move || swap_in(&exe, &bytes)).await??;
    log(format!("Update {tag} installed, restart to use it"));
    Ok(Some(tag))
}

/// Writes the new exe next to the running one and swaps them, putting the
/// old one back if the swap fails.
fn swap_in(exe: &Path, bytes: &[u8]) -> Result<()> {
    let new = exe.with_extension("exe.new");
    let old = old_path(exe);
    std::fs::write(&new, bytes).with_context(|| format!("writing {}", new.display()))?;
    let _ = std::fs::remove_file(&old);
    if let Err(e) = std::fs::rename(exe, &old) {
        let _ = std::fs::remove_file(&new);
        bail!("moving the running exe aside: {e}");
    }
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&new);
        bail!("putting the new exe in place: {e}");
    }
    Ok(())
}

pub fn restart() -> Result<()> {
    let exe = std::env::current_exe()?;
    std::process::Command::new(exe).args([WAIT_FLAG, &std::process::id().to_string()]).spawn()?;
    Ok(())
}

/// Blocks until `pid` has exited, for at most 30 seconds.
pub fn wait_for(pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
    // SAFETY: the handle is closed below; a gone PID just fails to open.
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            WaitForSingleObject(h, 30_000);
            let _ = CloseHandle(h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.1.3"), Some((0, 1, 3)));
        assert_eq!(parse_version("1.20.0"), Some((1, 20, 0)));
        assert_eq!(parse_version("v1.2"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert!(parse_version("v0.1.10") > parse_version("v0.1.9"));
        assert!(!is_newer(&format!("v{CURRENT}")));
        assert!(is_newer("v999.0.0"));
    }

    #[test]
    fn swap_replaces_and_keeps_old() {
        let dir = std::env::temp_dir().join(format!("ramrs-update-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        std::fs::write(&exe, b"old").unwrap();
        swap_in(&exe, b"new").unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"old");
        assert!(!exe.with_extension("exe.new").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
