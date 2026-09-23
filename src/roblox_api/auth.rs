use anyhow::{Context, Result, anyhow, bail};
use reqwest::StatusCode;
use reqwest::header::SET_COOKIE;
use serde::Deserialize;
use serde_json::json;

use super::{check, client, cookie_header, send_with_csrf};

#[derive(Debug, Clone, Deserialize)]
pub struct UserInfo {
    pub id: u64,
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}

/// Validates a cookie and returns the account it belongs to.
pub async fn authenticated_user(cookie: &str) -> Result<UserInfo> {
    let resp = client()
        .get("https://users.roblox.com/v1/users/authenticated")
        .header("Cookie", cookie_header(cookie))
        .send()
        .await?;
    if resp.status() == StatusCode::UNAUTHORIZED {
        bail!("cookie is invalid or expired");
    }
    Ok(check(resp).await?.json().await?)
}

pub async fn avatar_headshot(user_id: u64) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "imageUrl")]
        image_url: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<Item>,
    }
    let url = format!(
        "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds={user_id}&size=48x48&format=Png&isCircular=false"
    );
    let resp: Resp = check(client().get(url).send().await?).await?.json().await?;
    Ok(resp.data.into_iter().next().and_then(|i| i.image_url))
}

/// Short-lived ticket the Roblox client exchanges for a session.
pub async fn auth_ticket(cookie: &str) -> Result<String> {
    let resp = send_with_csrf(|| {
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
        bail!("Roblox rejected the cookie (HTTP {}); re-add the account", resp.status());
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
    /// solve these; the user logs in through their browser instead.
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
