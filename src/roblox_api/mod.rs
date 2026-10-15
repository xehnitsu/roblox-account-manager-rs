//! Thin async wrappers around Roblox's web APIs. No other hosts are contacted.

pub mod auth;
pub mod find_player;
pub mod games;
pub mod presence;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder().user_agent("Roblox/WinInet").timeout(Duration::from_secs(15)).build().expect("HTTP client")
    })
}

/// Latest published Windows player version, e.g. `version-2366ba214ec740ca`.
pub async fn latest_player_version() -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Resp {
        #[serde(rename = "clientVersionUpload")]
        client_version_upload: String,
    }
    let url = "https://clientsettingscdn.roblox.com/v2/client-version/WindowsPlayer";
    let resp: Resp = check(client().get(url).send().await?).await?.json().await?;
    Ok(resp.client_version_upload)
}

pub fn cookie_header(cookie: &str) -> String {
    format!(".ROBLOSECURITY={cookie}")
}

/// Sends a state-changing request, doing the Roblox CSRF dance: the first
/// attempt returns 403 with an `x-csrf-token` header, which we echo back.
/// Retries on 429 with exponential backoff.
pub async fn send_with_csrf(build: impl Fn() -> RequestBuilder) -> Result<Response> {
    send_with_csrf_cached(None, build).await
}

/// Like `send_with_csrf`, but remembers the token under `key` (e.g. the
/// account's cookie), which skips the extra round trip next time.
pub async fn send_with_csrf_cached(key: Option<&str>, build: impl Fn() -> RequestBuilder) -> Result<Response> {
    static TOKENS: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
    let cached = |k: &str| TOKENS.lock().unwrap().as_ref().and_then(|m| m.get(k).cloned());
    let mut token: Option<String> = key.and_then(cached);
    let mut backoff = Duration::from_secs(1);
    for _ in 0..6 {
        let mut req = build();
        if let Some(t) = &token {
            req = req.header("X-CSRF-TOKEN", t);
        }
        let resp = req.send().await?;
        if resp.status() == StatusCode::FORBIDDEN
            && let Some(t) = resp.headers().get("x-csrf-token").and_then(|v| v.to_str().ok())
            && token.as_deref() != Some(t)
        {
            token = Some(t.to_owned());
            continue;
        }
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            tokio::time::sleep(backoff).await;
            backoff *= 2;
            continue;
        }
        if let (Some(k), Some(t)) = (key, &token) {
            TOKENS.lock().unwrap().get_or_insert_default().insert(k.to_owned(), t.clone());
        }
        return Ok(resp);
    }
    bail!("Roblox kept rate-limiting or rejecting the CSRF token; try again shortly")
}

/// Turns a non-2xx response into an error that includes Roblox's message.
pub async fn check(resp: Response) -> Result<Response> {
    if resp.status().is_success() {
        return Ok(resp);
    }
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let body: String = body.chars().take(300).collect();
    bail!("HTTP {status}: {body}")
}
