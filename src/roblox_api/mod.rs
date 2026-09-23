//! Thin async wrappers around Roblox's web APIs. No other hosts are contacted.

pub mod auth;
pub mod games;
pub mod presence;

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Result, bail};
use reqwest::{Client, RequestBuilder, Response, StatusCode};

pub fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder().user_agent("Roblox/WinInet").timeout(Duration::from_secs(15)).build().expect("HTTP client")
    })
}

pub fn cookie_header(cookie: &str) -> String {
    format!(".ROBLOSECURITY={cookie}")
}

/// Sends a state-changing request, doing the Roblox CSRF dance: the first
/// attempt returns 403 with an `x-csrf-token` header, which we echo back.
/// Retries on 429 with exponential backoff.
pub async fn send_with_csrf(build: impl Fn() -> RequestBuilder) -> Result<Response> {
    let mut token: Option<String> = None;
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
