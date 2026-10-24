use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::{check, client, cookie_header, send_with_csrf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceType {
    Offline,
    Online,
    InGame,
    InStudio,
    Invisible,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Presence {
    pub kind: PresenceType,
    /// Place the user is in. Differs from the launched place after a teleport.
    pub place_id: Option<u64>,
    /// Job ID of the server the user is in, when visible to the caller.
    pub job_id: Option<String>,
}

/// Presence of `user_id` as seen by the account owning `cookie`. Querying an
/// account's own presence with its own cookie reveals the job ID.
pub async fn presence(cookie: &str, user_id: u64) -> Result<Presence> {
    let all = presences(cookie, &[user_id]).await?;
    all.into_iter().next().map(|(_, p)| p).ok_or_else(|| anyhow!("no presence data"))
}

/// Presence of up to 50 users as seen by the account owning `cookie`.
pub async fn presences(cookie: &str, user_ids: &[u64]) -> Result<Vec<(u64, Presence)>> {
    #[derive(Deserialize)]
    struct Item {
        #[serde(rename = "userId")]
        user_id: u64,
        #[serde(rename = "userPresenceType")]
        kind: u8,
        #[serde(rename = "placeId")]
        place_id: Option<u64>,
        #[serde(rename = "gameId")]
        game_id: Option<String>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "userPresences")]
        user_presences: Vec<Item>,
    }
    let body = json!({ "userIds": user_ids });
    let resp = send_with_csrf(|| {
        client()
            .post("https://presence.roblox.com/v1/presence/users")
            .header("Cookie", cookie_header(cookie))
            .json(&body)
    })
    .await?;
    let r: Resp = check(resp).await?.json().await?;
    Ok(r.user_presences
        .into_iter()
        .map(|item| {
            let kind = match item.kind {
                0 => PresenceType::Offline,
                1 => PresenceType::Online,
                2 => PresenceType::InGame,
                3 => PresenceType::InStudio,
                4 => PresenceType::Invisible,
                _ => PresenceType::Unknown,
            };
            let presence = Presence { kind, place_id: item.place_id, job_id: item.game_id.filter(|s| !s.is_empty()) };
            (item.user_id, presence)
        })
        .collect())
}
