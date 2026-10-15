//! Log in through a real browser window.
//!
//! Roblox often answers API logins with a captcha. Instead of solving it, this
//! opens Helium/Chrome/Edge/Brave with a throwaway profile on the Roblox login page,
//! optionally types in the username and password, and lets the user press
//! "Log in" and finish the login (captcha, 2-step verification) themselves. The `.ROBLOSECURITY` cookie
//! is then read over the Chrome DevTools Protocol and the browser is closed.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::applog::log;

const LOGIN_URL: &str = "https://www.roblox.com/login";
const HOME_URL: &str = "https://www.roblox.com/home";
/// Profiles of "open in browser" windows, kept until the window closes.
const SESSION_PREFIX: &str = "ramrs-browser-";
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Installed Chromium-based browser, preferring Helium, then Chrome, Edge and
/// Brave.
pub fn find_browser() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let roots = [env("LOCALAPPDATA"), env("ProgramFiles"), env("ProgramFiles(x86)")];
    let rels = [
        r"imput\Helium\Application\chrome.exe",
        r"Google\Chrome\Application\chrome.exe",
        r"Microsoft\Edge\Application\msedge.exe",
        r"BraveSoftware\Brave-Browser\Application\brave.exe",
    ];
    rels.iter().flat_map(|rel| roots.iter().flatten().map(move |root| root.join(rel))).find(|p| p.is_file())
}

/// Opens a browser window on the Roblox login page and waits until the user is
/// logged in. Returns the `.ROBLOSECURITY` cookie.
pub async fn login(credentials: Option<(String, String)>) -> Result<String> {
    let exe = find_browser().ok_or_else(|| anyhow!("no Helium, Chrome, Edge or Brave installation found"))?;
    remove_stale_profiles();
    let profile = std::env::temp_dir().join(format!("ramrs-login-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&profile)?;

    let mut browser = Browser { child: None, profile };
    browser.child = Some(spawn(&exe, &browser.profile, &["--window-size=900,860", "--new-window", LOGIN_URL])?);
    log(format!("[login] opened {} for browser login", exe.file_name().unwrap_or_default().display()));

    let result = run(&mut browser, credentials).await;
    browser.close().await;
    result
}

/// Starts the browser on a throwaway profile with DevTools on a random local port.
fn spawn(exe: &Path, profile: &Path, args: &[&str]) -> Result<Child> {
    Command::new(exe)
        .arg(format!("--user-data-dir={}", profile.display()))
        .args(["--remote-debugging-port=0", "--no-first-run", "--no-default-browser-check", "--disable-sync"])
        .args(args)
        .spawn()
        .with_context(|| format!("starting {}", exe.display()))
}

/// Opens a browser window already logged in as the account behind `cookie`,
/// on its own throwaway profile. The window stays open after this returns.
pub async fn open_logged_in(cookie: &str) -> Result<()> {
    let exe = find_browser().ok_or_else(|| anyhow!("no Helium, Chrome, Edge or Brave installation found"))?;
    remove_unused_profiles(SESSION_PREFIX);
    let profile = std::env::temp_dir().join(format!("{SESSION_PREFIX}{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&profile)?;
    let mut browser = Browser { child: None, profile };
    browser.child = Some(spawn(&exe, &browser.profile, &["--new-window", "about:blank"])?);

    let result = async {
        let ws_url = browser.devtools_url().await?;
        let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.context("connecting to browser")?;
        let mut cdp = Cdp { ws, next_id: 0 };
        let cookie = json!({
            "name": ".ROBLOSECURITY", "value": cookie, "domain": ".roblox.com", "path": "/",
            "secure": true, "httpOnly": true,
        });
        cdp.call("Storage.setCookies", json!({ "cookies": [cookie] }), None).await?;
        // Load the home page in the blank tab, or a new one if it's gone.
        let blank = pages(&mut cdp).await?.into_iter().next();
        match blank {
            Some((target, _)) => {
                let attached =
                    cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None).await?;
                let session = attached["sessionId"].as_str().ok_or_else(|| anyhow!("no session id"))?.to_owned();
                cdp.call("Page.navigate", json!({ "url": HOME_URL }), Some(&session)).await?;
                let _ = cdp.call("Target.detachFromTarget", json!({ "sessionId": session }), None).await;
            }
            None => {
                cdp.call("Target.createTarget", json!({ "url": HOME_URL }), None).await?;
            }
        }
        anyhow::Ok(())
    }
    .await;
    if result.is_err() {
        browser.close().await;
    }
    result
}

async fn run(browser: &mut Browser, credentials: Option<(String, String)>) -> Result<String> {
    let ws_url = browser.devtools_url().await?;
    let (ws, _) = tokio_tungstenite::connect_async(&ws_url).await.context("connecting to browser")?;
    let mut cdp = Cdp { ws, next_id: 0 };

    let result = wait_for_login(&mut cdp, credentials).await;
    // The launched process can't be used to close the browser: Edge sometimes
    // restarts itself under a new PID. Closing over DevTools always works.
    let _ = cdp.call("Browser.close", json!({}), None).await;
    result
}

async fn wait_for_login(cdp: &mut Cdp, credentials: Option<(String, String)>) -> Result<String> {
    const CLOSED: &str = "browser window was closed before the login finished";
    let mut filled = credentials.is_none();
    let started = Instant::now();
    let mut empty_ticks = 0;

    loop {
        // A dropped DevTools connection means the browser is gone.
        let Ok(cookie) = roblosecurity(cdp).await else { bail!(CLOSED) };
        if let Some(cookie) = cookie {
            log("[login] browser login finished");
            return Ok(cookie);
        }
        let Ok(pages) = pages(cdp).await else { bail!(CLOSED) };
        if !filled && let Some((user, pass)) = &credentials {
            filled = try_fill(cdp, &pages, user, pass).await.unwrap_or(false);
        }
        // The browser can stay alive in the background after its last window
        // closes, so no open tabs for a few seconds also counts as closed.
        empty_ticks = if pages.is_empty() { empty_ticks + 1 } else { 0 };
        if empty_ticks >= 3 && started.elapsed() > Duration::from_secs(5) {
            bail!(CLOSED);
        }
        if started.elapsed() > TIMEOUT {
            bail!("timed out waiting for the browser login");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn roblosecurity(cdp: &mut Cdp) -> Result<Option<String>> {
    let res = cdp.call("Storage.getCookies", json!({}), None).await?;
    let cookies = res["cookies"].as_array().cloned().unwrap_or_default();
    Ok(cookies
        .iter()
        .find(|c| c["name"] == ".ROBLOSECURITY" && c["domain"].as_str().is_some_and(|d| d.ends_with("roblox.com")))
        .and_then(|c| c["value"].as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_owned))
}

/// Open tabs as (target id, url).
async fn pages(cdp: &mut Cdp) -> Result<Vec<(String, String)>> {
    let targets = cdp.call("Target.getTargets", json!({}), None).await?;
    Ok(targets["targetInfos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["type"] == "page")
        .filter_map(|t| Some((t["targetId"].as_str()?.to_owned(), t["url"].as_str()?.to_owned())))
        .collect())
}

/// Types the username and password into the login tab, as if pasted, and
/// leaves pressing "Log in" (and any captcha) to the user. Returns true once
/// the form was found.
async fn try_fill(cdp: &mut Cdp, pages: &[(String, String)], user: &str, pass: &str) -> Result<bool> {
    let Some((target, _)) = pages.iter().find(|(_, url)| url.contains("roblox.com/login")) else {
        return Ok(false);
    };
    let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), None).await?;
    let session = attached["sessionId"].as_str().ok_or_else(|| anyhow!("no session id"))?.to_owned();
    let result = fill(cdp, &session, user, pass).await;
    let _ = cdp.call("Target.detachFromTarget", json!({ "sessionId": session }), None).await;
    result
}

async fn fill(cdp: &mut Cdp, session: &str, user: &str, pass: &str) -> Result<bool> {
    for (id, text) in [("login-username", user), ("login-password", pass)] {
        let res = cdp
            .call("Runtime.evaluate", json!({ "expression": focus_script(id), "returnByValue": true }), Some(session))
            .await?;
        if res["result"]["value"] != true {
            return Ok(false);
        }
        cdp.call("Input.insertText", json!({ "text": text }), Some(session)).await?;
    }
    Ok(true)
}

/// Focuses and empties an input; false if it isn't on the page (yet).
fn focus_script(id: &str) -> String {
    format!(
        r#"(function() {{
    var el = document.getElementById('{id}');
    if (!el) return false;
    el.focus();
    el.select();
    return document.activeElement === el;
}})()"#
    )
}

struct Cdp {
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl Cdp {
    async fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send(Message::text(msg.to_string())).await?;
        let reply = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(frame) = self.ws.next().await {
                let Message::Text(text) = frame? else { continue };
                let v: Value = serde_json::from_str(&text)?;
                if v["id"] == id {
                    return Ok(v);
                }
            }
            Err(anyhow!("browser connection closed"))
        })
        .await
        .map_err(|_| anyhow!("{method}: no reply from browser"))??;
        if let Some(err) = reply.get("error") {
            bail!("{method}: {}", err["message"].as_str().unwrap_or("error"));
        }
        Ok(reply["result"].clone())
    }
}

struct Browser {
    child: Option<Child>,
    profile: PathBuf,
}

impl Browser {
    /// The browser writes its DevTools port and path to `DevToolsActivePort`
    /// in the profile directory once it is listening.
    async fn devtools_url(&self) -> Result<String> {
        let file = self.profile.join("DevToolsActivePort");
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(20) {
            if let Some(url) = read_active_port(&file) {
                return Ok(url);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        bail!("browser did not open a DevTools port")
    }

    async fn close(&mut self) {
        if let Some(mut child) = self.child.take() {
            for _ in 0..30 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        // Child processes may hold files in the profile for a moment.
        for _ in 0..40 {
            if std::fs::remove_dir_all(&self.profile).is_ok() || !self.profile.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        log(format!("[login] could not delete temporary profile {}", self.profile.display()));
    }
}

/// Deletes "open in browser" profiles whose browser has closed. A running
/// browser keeps its profile's `lockfile` open, so that file can't be deleted.
fn remove_unused_profiles(prefix: &str) {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in entries.flatten() {
        if !e.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let path = e.path();
        let lock = path.join("lockfile");
        if lock.exists() && std::fs::remove_file(&lock).is_err() {
            continue;
        }
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Deletes temporary profiles left behind by earlier logins.
fn remove_stale_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for e in entries.flatten() {
        if e.file_name().to_string_lossy().starts_with("ramrs-login-") {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

fn read_active_port(file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(file).ok()?;
    let mut lines = text.lines();
    let port: u16 = lines.next()?.trim().parse().ok()?;
    let path = lines.next()?.trim();
    path.starts_with('/').then(|| format!("ws://127.0.0.1:{port}{path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_port_file() {
        let dir = std::env::temp_dir().join(format!("ramrs-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("DevToolsActivePort");
        std::fs::write(&f, "9222\n/devtools/browser/abc\n").unwrap();
        assert_eq!(read_active_port(&f).as_deref(), Some("ws://127.0.0.1:9222/devtools/browser/abc"));
        std::fs::write(&f, "9222\n").unwrap();
        assert_eq!(read_active_port(&f), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn focus_script_targets_input() {
        assert!(focus_script("login-password").contains("getElementById('login-password')"));
    }
}
