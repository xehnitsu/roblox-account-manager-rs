//! Roblox in an embedded WebView2 window (the Edge engine built into Windows):
//! logging in to add an account, and browsing roblox.com as an account.
//!
//! Each window runs in a child process (this exe started with `--webview`),
//! so its event loop owns that process's main thread and a crash there can't
//! take the app down. Secrets go over stdin/stdout, never the command line.
//! Each window uses a temporary profile that is deleted when it closes.
//! `browser_login` (an external browser) is the fallback when WebView2 is
//! missing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::io::Write as _;
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::{Icon, WindowBuilder};
use wry::{WebContext, WebView, WebViewBuilder};

use crate::applog::log;

const LOGIN_URL: &str = "https://www.roblox.com/login";
const HOME_URL: &str = "https://www.roblox.com/home";
const PROFILE_PREFIX: &str = "ramrs-webview-";
const TICK: Duration = Duration::from_millis(500);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How long a session window waits for its cookie before loading anyway.
const COOKIE_WAIT: Duration = Duration::from_secs(5);
/// When a session window checks which page it ended up on.
const LANDING_CHECK: Duration = Duration::from_secs(6);

/// Whether the WebView2 runtime is installed.
pub fn available() -> bool {
    wry::webview_version().is_ok()
}

/// Command-line flag that makes this exe run a single WebView2 window.
pub const CHILD_FLAG: &str = "--webview";

/// Opens the Roblox login page, with the username and password filled in if
/// given, and waits until the user has logged in. Returns the `.ROBLOSECURITY`
/// cookie.
pub async fn login(credentials: Option<(String, String)>) -> Result<String> {
    let input = match &credentials {
        Some((u, p)) => json!({ "username": u, "password": p }),
        None => json!({}),
    };
    let output = tokio::task::spawn_blocking(move || -> Result<std::process::Output> {
        Ok(spawn_child("login", &input)?.wait_with_output()?)
    })
    .await??;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(cookie) = line.strip_prefix("COOKIE ") {
            log("[login] login finished");
            return Ok(cookie.to_owned());
        }
        if let Some(e) = line.strip_prefix("ERROR ") {
            bail!("login window failed: {e}");
        }
    }
    if output.status.success() {
        bail!("login window was closed before the login finished")
    }
    bail!("login window stopped unexpectedly ({})", output.status)
}

/// Opens roblox.com logged in as the account in its own window, which stays
/// open until the user closes it.
pub fn open_session(username: &str, cookie: &str) -> Result<()> {
    spawn_child("session", &json!({ "username": username, "cookie": cookie }))?;
    Ok(())
}

/// Starts this exe as a window process and hands it `input` on stdin.
fn spawn_child(mode: &str, input: &Value) -> Result<std::process::Child> {
    let exe = std::env::current_exe().context("finding the app's exe")?;
    let mut child = Command::new(exe)
        .args([CHILD_FLAG, mode])
        .stdin(Stdio::piped())
        // Only a login window reports back.
        .stdout(if mode == "login" { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null())
        .spawn()
        .context("starting the login window")?;
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
    writeln!(stdin, "{input}")?;
    Ok(child)
}

/// Entry point of a window process (`<exe> --webview <mode>`). Reads its input
/// from stdin. A login window prints `COOKIE <value>` once logged in.
pub fn child_main(mode: &str) -> i32 {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let input: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
    let text = |k: &str| input[k].as_str().map(str::to_owned);
    let result = match mode {
        "login" => {
            let scripts: Vec<String> = match (text("username"), text("password")) {
                (Some(u), Some(p)) => vec![fill_script(&u, &p)],
                _ => Vec::new(),
            };
            let started = Instant::now();
            let mut cookie = None;
            let result = run_window("Log in to Roblox", LOGIN_URL, None, &scripts, |webview| {
                cookie = roblosecurity(webview);
                cookie.is_some() || started.elapsed() > LOGIN_TIMEOUT
            });
            if let Some(c) = &cookie {
                let _ = writeln!(std::io::stdout(), "COOKIE {c}");
            }
            result
        }
        "session" => {
            let title = format!("{} (Roblox)", text("username").unwrap_or_default());
            let cookie = text("cookie").unwrap_or_default();
            run_window(&title, HOME_URL, Some(&cookie), &[LOGOUT_GUARD.to_owned()], |_| false)
        }
        _ => Err(anyhow!("unknown window mode {mode}")),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(std::io::stdout(), "ERROR {e:#}");
            1
        }
    }
}

/// Shows one WebView2 window until it's closed or `on_tick` (called twice a
/// second) returns true, then deletes its profile.
fn run_window(
    title: &str,
    url: &str,
    cookie: Option<&str>,
    scripts: &[String],
    mut on_tick: impl FnMut(&WebView) -> bool,
) -> Result<()> {
    let profile = std::env::temp_dir().join(format!("{PROFILE_PREFIX}{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut event_loop = EventLoopBuilder::new().build();
        let window = WindowBuilder::new()
            .with_title(title)
            .with_inner_size(LogicalSize::new(900.0, 860.0))
            .with_window_icon(icon())
            .build(&event_loop)?;
        let mut context = WebContext::new(Some(profile.clone()));
        // With a cookie, start blank so the first request is already logged in.
        let mut builder = WebViewBuilder::new_with_web_context(&mut context).with_url(if cookie.is_some() {
            "about:blank"
        } else {
            url
        });
        for script in scripts {
            builder = builder.with_initialization_script(script.as_str());
        }
        let webview = builder.build(&window)?;
        // WebView2 stores the cookie in the background, so loading right away
        // can go out without it and land on the login page. Load once the
        // cookie is visible.
        let started = Instant::now();
        let mut waiting = None;
        if let Some(value) = cookie {
            set_session_cookie(&webview, value)?;
            log(format!("[browser] cookie set, visible right away: {}", roblosecurity(&webview).is_some()));
            waiting = Some(url);
        }
        let mut loaded_at = None;
        let mut checked = false;

        event_loop.run_return(|event, _, flow| {
            *flow = ControlFlow::WaitUntil(Instant::now() + TICK);
            match event {
                Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => *flow = ControlFlow::Exit,
                Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                    if let Some(u) = waiting {
                        let visible = roblosecurity(&webview).is_some();
                        if visible || started.elapsed() > COOKIE_WAIT {
                            let ms = started.elapsed().as_millis();
                            log(format!("[browser] loading after {ms} ms, cookie visible: {visible}"));
                            let _ = webview.load_url(u);
                            waiting = None;
                            loaded_at = Some(Instant::now());
                        }
                    }
                    if !checked && loaded_at.is_some_and(|t| t.elapsed() > LANDING_CHECK) {
                        checked = true;
                        let page = webview.url().unwrap_or_default();
                        let lower = page.to_lowercase();
                        if lower.contains("/login") || lower.contains("/newlogin") {
                            log(format!(
                                "[browser] Roblox sent the window to {page}, so it didn't accept the saved cookie"
                            ));
                        } else {
                            log(format!("[browser] window is on {page}"));
                        }
                    }
                    if on_tick(&webview) {
                        *flow = ControlFlow::Exit;
                    }
                }
                _ => {}
            }
        });
        Ok(())
    })();
    remove_profile(&profile);
    result
}

/// Adds the `.ROBLOSECURITY` cookie for every roblox.com host. wry's
/// `set_cookie` can't do this: the cookie crate drops the leading dot from the
/// domain, and WebView2 then only sends the cookie to roblox.com itself, not
/// www.roblox.com.
fn set_session_cookie(webview: &WebView, value: &str) -> Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_2;
    use windows::core::{HSTRING, Interface as _};
    use wry::WebViewExtWindows as _;

    // SAFETY: plain COM calls on the window's own WebView2, on its thread.
    unsafe {
        let manager = webview.webview().cast::<ICoreWebView2_2>()?.CookieManager()?;
        let cookie = manager.CreateCookie(
            &HSTRING::from(".ROBLOSECURITY"),
            &HSTRING::from(value),
            &HSTRING::from(".roblox.com"),
            &HSTRING::from("/"),
        )?;
        cookie.SetIsSecure(true)?;
        cookie.SetIsHttpOnly(true)?;
        manager.AddOrUpdateCookie(&cookie)?;
    }
    Ok(())
}

fn roblosecurity(webview: &WebView) -> Option<String> {
    webview
        .cookies_for_url("https://www.roblox.com")
        .ok()?
        .into_iter()
        .find(|c| c.name() == ".ROBLOSECURITY" && !c.value().is_empty())
        .map(|c| c.value().to_owned())
}

/// Fills in the login form once it appears. Pressing "Log in" (and any
/// captcha) is left to the user.
fn fill_script(username: &str, password: &str) -> String {
    let u = serde_json::to_string(username).unwrap_or_default();
    let p = serde_json::to_string(password).unwrap_or_default();
    format!(
        r#"(function() {{
    if (location.hostname !== 'www.roblox.com' || !location.pathname.startsWith('/login')) return;
    var tries = 0;
    var timer = setInterval(function() {{
        var user = document.getElementById('login-username');
        var pass = document.getElementById('login-password');
        if (++tries > 150) clearInterval(timer);
        if (!user || !pass) return;
        clearInterval(timer);
        var setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
        setter.call(user, {u});
        user.dispatchEvent(new Event('input', {{ bubbles: true }}));
        setter.call(pass, {p});
        pass.dispatchEvent(new Event('input', {{ bubbles: true }}));
        pass.focus();
    }}, 200);
}})();"#
    )
}

/// Blocks Roblox's logout requests in session windows: logging out would end
/// the session and invalidate the saved cookie.
const LOGOUT_GUARD: &str = r#"(function() {
    var blocked = /auth\.roblox\.com\/v\d+\/logout|logoutfromallsessions/i;
    function warn() {
        alert('Logging out is disabled in this window: it would invalidate the saved cookie. Close the window instead.');
    }
    var fetch0 = window.fetch;
    window.fetch = function(input, init) {
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        if (blocked.test(url)) { warn(); return Promise.reject(new Error('logout blocked')); }
        return fetch0.apply(this, arguments);
    };
    var open0 = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function(method, url) {
        this.__logout = blocked.test(String(url));
        return open0.apply(this, arguments);
    };
    var send0 = XMLHttpRequest.prototype.send;
    XMLHttpRequest.prototype.send = function() {
        if (this.__logout) { warn(); this.abort(); return; }
        return send0.apply(this, arguments);
    };
})();"#;

fn icon() -> Option<Icon> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).ok()?;
    Icon::from_rgba(icon.rgba, icon.width, icon.height).ok()
}

/// WebView2's helper processes may hold the profile for a moment after the
/// window closes.
fn remove_profile(profile: &Path) {
    for _ in 0..40 {
        if std::fs::remove_dir_all(profile).is_ok() || !profile.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    log(format!("could not delete temporary profile {}", profile.display()));
}

/// Deletes profiles left behind by an earlier run. Call before any window opens.
pub fn remove_stale_profiles() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else { return };
    let stale: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(PROFILE_PREFIX))
        .map(|e| e.path())
        .collect();
    for path in stale {
        let _ = std::fs::remove_dir_all(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_script_escapes() {
        let s = fill_script("bob", "p\"a'ss\\");
        assert!(s.contains(r#"setter.call(pass, "p\"a'ss\\");"#));
    }

    #[test]
    fn logout_guard_pattern() {
        assert!(LOGOUT_GUARD.contains(r"auth\.roblox\.com\/v\d+\/logout"));
    }
}
