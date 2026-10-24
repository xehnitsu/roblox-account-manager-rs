use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::header::SET_COOKIE;
use serde::Deserialize;
use serde_json::json;

use super::{check, client, cookie_header, send_with_csrf, send_with_csrf_cached};

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub id: u64,
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// Whether a cookie is still logged in. Errors other than a rejected cookie
/// (network, rate limit) are returned as errors.
pub async fn cookie_valid(cookie: &str) -> Result<bool> {
    match authenticated_user(cookie).await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string() == EXPIRED => Ok(false),
        Err(e) => Err(e),
    }
}

const EXPIRED: &str = "cookie is invalid or expired";

pub async fn authenticated_user(cookie: &str) -> Result<UserInfo> {
    let resp = client()
        .get("https://users.roblox.com/v1/users/authenticated")
        .header("Cookie", cookie_header(cookie))
        .send()
        .await?;
    if resp.status() == StatusCode::UNAUTHORIZED {
        bail!(EXPIRED);
    }
    Ok(check(resp).await?.json().await?)
}

pub async fn avatar_headshot(user_id: u64) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "imageUrl")]
        image_url: Option<String>,
        state: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<Item>,
    }
    let url = format!(
        "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds={user_id}&size=48x48&format=Png&isCircular=false"
    );
    // A thumbnail that isn't rendered yet comes back as "Pending" with an empty URL.
    for attempt in 0..4 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        }
        let resp: Resp = check(client().get(&url).send().await?).await?.json().await?;
        let Some(item) = resp.data.into_iter().next() else { return Ok(None) };
        if item.state.as_deref() == Some("Pending") {
            continue;
        }
        return Ok(item.image_url.filter(|u| !u.is_empty()));
    }
    Ok(None)
}

/// Short-lived ticket the Roblox client exchanges for a session.
/// Tickets fetched ahead of a launch (see `prefetch_ticket`): cookie, when, ticket.
static PREFETCHED: std::sync::Mutex<Vec<(String, std::time::Instant, String)>> = std::sync::Mutex::new(Vec::new());
const PREFETCH_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(60);

pub async fn prefetch_ticket(cookie: String) {
    let fresh = |list: &Vec<(String, std::time::Instant, String)>| {
        list.iter().any(|(c, at, _)| *c == cookie && at.elapsed() < PREFETCH_MAX_AGE / 2)
    };
    if fresh(&PREFETCHED.lock().unwrap()) {
        return;
    }
    if let Ok(ticket) = auth_ticket(&cookie).await {
        let mut list = PREFETCHED.lock().unwrap();
        list.retain(|(c, at, _)| *c != cookie && at.elapsed() < PREFETCH_MAX_AGE);
        list.push((cookie, std::time::Instant::now(), ticket));
    }
}

/// A prefetched ticket if one is fresh (tickets are single use, so it's
/// removed), otherwise a new one.
pub async fn take_ticket(cookie: &str) -> Result<String> {
    let cached = {
        let mut list = PREFETCHED.lock().unwrap();
        list.retain(|(_, at, _)| at.elapsed() < PREFETCH_MAX_AGE);
        list.iter().position(|(c, _, _)| c == cookie).map(|i| list.remove(i).2)
    };
    match cached {
        Some(t) => Ok(t),
        None => auth_ticket(cookie).await,
    }
}

pub async fn auth_ticket(cookie: &str) -> Result<String> {
    let resp = send_with_csrf_cached(Some(cookie), || {
        client()
            .post("https://auth.roblox.com/v1/authentication-ticket/")
            .header("Cookie", cookie_header(cookie))
            .header("Referer", "https://www.roblox.com/develop")
            .header("RBX-For-Gameauth", "true")
            .header("Content-Type", "application/json")
            .body("{}")
    })
    .await?;
    if matches!(resp.status(), StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        bail!("Roblox rejected the cookie (HTTP {}), re-add the account", resp.status());
    }
    let resp = check(resp).await?;
    resp.headers()
        .get("rbx-authentication-ticket")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("response had no rbx-authentication-ticket header"))
}

#[derive(Debug)]
pub enum LoginOutcome {
    Success {
        cookie: String,
    },
    /// Captcha, 2-step verification or another challenge. We never try to
    /// solve these. The user logs in through their browser instead.
    ChallengeRequired {
        kind: String,
    },
}

pub async fn login(username: &str, password: &str) -> Result<LoginOutcome> {
    let body = json!({ "ctype": "Username", "cvalue": username, "password": password });
    let resp = send_with_csrf(|| {
        client()
            .post("https://auth.roblox.com/v2/login")
            .header("Referer", "https://www.roblox.com/")
            .header("Origin", "https://www.roblox.com")
            .json(&body)
    })
    .await?;

    if let Some(kind) = resp.headers().get("rblx-challenge-type").and_then(|v| v.to_str().ok()) {
        return Ok(LoginOutcome::ChallengeRequired { kind: kind.to_owned() });
    }

    let cookie = resp
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find_map(|v| v.strip_prefix(".ROBLOSECURITY="))
        .map(|v| v.split(';').next().unwrap_or("").to_owned())
        .filter(|v| !v.is_empty());

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if let Some(cookie) = cookie {
        return Ok(LoginOutcome::Success { cookie });
    }

    let lower = text.to_ascii_lowercase();
    if lower.contains("twostepverificationdata") {
        return Ok(LoginOutcome::ChallengeRequired { kind: "two-step verification".into() });
    }
    if lower.contains("challenge") || lower.contains("captcha") || lower.contains("robot") {
        return Ok(LoginOutcome::ChallengeRequired { kind: "captcha".into() });
    }
    let msg = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["errors"][0]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(anyhow!("login failed (HTTP {status}): {msg}")).context("credential login")
}
