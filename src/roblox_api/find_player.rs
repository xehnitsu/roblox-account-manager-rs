//! Finds the public server a user is in, the way the original Roblox Account
//! Manager does it. Public server lists don't name their players, but each
//! server lists a token per player that resolves to that player's avatar
//! headshot. A server whose headshots include the user's headshot is theirs.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;
use serde_json::json;

use super::games::{self, Server};
use super::{check, client, send_with_csrf};

const BATCH: usize = 100;
/// Pause between server pages to stay under Roblox's rate limit.
const PAGE_DELAY: Duration = Duration::from_millis(750);

#[derive(Default)]
pub struct Progress {
    pub servers_checked: AtomicUsize,
    pub cancel: AtomicBool,
}

pub async fn user_id(name: &str) -> Result<(u64, String)> {
    #[derive(Deserialize)]
    struct User {
        id: u64,
        name: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<User>,
    }
    let body = json!({ "usernames": [name], "excludeBannedUsers": true });
    let resp = send_with_csrf(|| client().post("https://users.roblox.com/v1/usernames/users").json(&body)).await?;
    let resp: Resp = check(resp).await?.json().await?;
    if let Some(u) = resp.data.into_iter().next() {
        return Ok((u.id, u.name));
    }
    if let Ok(id) = name.parse::<u64>() {
        return Ok((id, name.to_owned()));
    }
    bail!("no user named {name}")
}

/// Scans the place's public servers for the user. Returns None when the user
/// isn't in any of them (or the search was cancelled). Roblox only lists
/// player tokens for a logged-in request, hence the account cookie.
pub async fn find_player(place_id: u64, user_id: u64, cookie: &str, progress: &Progress) -> Result<Option<Server>> {
    let target = headshot(user_id).await?;
    let mut cursor: Option<String> = None;
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let page = games::public_servers(place_id, cursor.as_deref(), Some(cookie)).await?;
        if page.servers.iter().any(|s| s.playing > 0) && page.servers.iter().all(|s| s.player_tokens.is_empty()) {
            bail!("Roblox did not list the players in these servers. The account's cookie may have expired");
        }
        // (server index, token) for every player on the page.
        let tokens: Vec<(usize, &str)> = page
            .servers
            .iter()
            .enumerate()
            .flat_map(|(i, s)| s.player_tokens.iter().map(move |t| (i, t.as_str())))
            .collect();
        for chunk in tokens.chunks(BATCH) {
            if let Some(i) = matching_server(chunk, &target).await? {
                return Ok(Some(page.servers[i].clone()));
            }
        }
        progress.servers_checked.fetch_add(page.servers.len(), Ordering::Relaxed);
        match page.next_cursor {
            Some(c) => cursor = Some(c),
            None => return Ok(None),
        }
        tokio::time::sleep(PAGE_DELAY).await;
    }
}

#[derive(Deserialize)]
struct Thumb {
    #[serde(rename = "requestId", default)]
    request_id: String,
    #[serde(rename = "imageUrl", default)]
    image_url: Option<String>,
}

#[derive(Deserialize)]
struct Thumbs {
    data: Vec<Thumb>,
}

/// The user's 48x48 headshot URL, requested the same way as the server
/// tokens so the URLs compare equal.
async fn headshot(user_id: u64) -> Result<String> {
    let body = json!([{
        "requestId": "target",
        "targetId": user_id,
        "type": "AvatarHeadShot",
        "size": "48x48",
        "format": "png",
        "isCircular": false,
    }]);
    for attempt in 0..4 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let url = thumbs(&body).await?.into_iter().next().and_then(|t| t.image_url).filter(|u| !u.is_empty());
        if let Some(url) = url {
            return Ok(url);
        }
    }
    Err(anyhow!("could not get the user's avatar"))
}

async fn matching_server(tokens: &[(usize, &str)], target: &str) -> Result<Option<usize>> {
    let body: Vec<_> = tokens
        .iter()
        .enumerate()
        .map(|(n, (i, token))| {
            json!({
                "requestId": format!("{n}:{i}"),
                "token": token,
                "type": "AvatarHeadShot",
                "size": "48x48",
                "format": "png",
                "isCircular": false,
            })
        })
        .collect();
    let found = thumbs(&json!(body)).await?.into_iter().find(|t| t.image_url.as_deref() == Some(target));
    Ok(found.and_then(|t| t.request_id.split(':').nth(1)?.parse().ok()))
}

async fn thumbs(body: &serde_json::Value) -> Result<Vec<Thumb>> {
    let resp = send_with_csrf(|| client().post("https://thumbnails.roblox.com/v1/batch").json(body)).await?;
    Ok(check(resp).await?.json::<Thumbs>().await?.data)
}
