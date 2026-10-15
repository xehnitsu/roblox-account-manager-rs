use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::{check, client, cookie_header, send_with_csrf};

#[derive(Debug, Clone, Deserialize)]
pub struct Server {
    /// Job ID.
    pub id: String,
    #[serde(rename = "maxPlayers", default)]
    pub max_players: u32,
    #[serde(default)]
    pub playing: u32,
    #[serde(default)]
    pub fps: Option<f64>,
    #[serde(default)]
    pub ping: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerPage {
    pub servers: Vec<Server>,
    pub next_cursor: Option<String>,
}

/// One page (up to 100) of public servers. The public API does not expose a
/// region field, so region is not available here.
pub async fn public_servers(place_id: u64, cursor: Option<&str>) -> Result<ServerPage> {
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<Server>,
        #[serde(rename = "nextPageCursor")]
        next_page_cursor: Option<String>,
    }
    let mut url = format!(
        "https://games.roblox.com/v1/games/{place_id}/servers/Public?sortOrder=Desc&excludeFullGames=false&limit=100"
    );
    if let Some(c) = cursor {
        url.push_str("&cursor=");
        url.push_str(&url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>());
    }
    let resp: Resp = check(client().get(url).send().await?).await?.json().await?;
    Ok(ServerPage { servers: resp.data, next_cursor: resp.next_page_cursor })
}

/// URL of a place's game icon, or None if Roblox has none (e.g. moderated).
pub async fn place_icon(place_id: u64) -> Result<Option<String>> {
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
        "https://thumbnails.roblox.com/v1/places/gameicons?placeIds={place_id}&returnPolicy=PlaceHolder&size=150x150&format=Png&isCircular=false"
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

pub async fn place_name(place_id: u64) -> Result<String> {
    #[derive(Deserialize)]
    struct Universe {
        #[serde(rename = "universeId")]
        universe_id: Option<u64>,
    }
    #[derive(Deserialize)]
    struct Game {
        name: String,
    }
    #[derive(Deserialize)]
    struct Games {
        data: Vec<Game>,
    }
    let u: Universe =
        check(client().get(format!("https://apis.roblox.com/universes/v1/places/{place_id}/universe")).send().await?)
            .await?
            .json()
            .await?;
    let universe_id = u.universe_id.ok_or_else(|| anyhow!("place {place_id} not found"))?;
    let g: Games =
        check(client().get(format!("https://games.roblox.com/v1/games?universeIds={universe_id}")).send().await?)
            .await?
            .json()
            .await?;
    g.data.into_iter().next().map(|g| g.name).ok_or_else(|| anyhow!("game not found"))
}

/// Resolves a `roblox.com/share?code=...&type=Server` link to `(place_id, link_code)`.
pub async fn resolve_share_link(cookie: &str, code: &str) -> Result<(u64, String)> {
    let body = json!({ "linkId": code, "linkType": "Server" });
    let resp = send_with_csrf(|| {
        client()
            .post("https://apis.roblox.com/sharelinks/v1/resolve-link")
            .header("Cookie", cookie_header(cookie))
            .json(&body)
    })
    .await?;
    let v: serde_json::Value = check(resp).await?.json().await?;
    let data = &v["privateServerInviteData"];
    let place_id = data["placeId"].as_u64().ok_or_else(|| anyhow!("share link has no place"))?;
    let link_code = data["linkCode"].as_str().ok_or_else(|| anyhow!("share link is invalid or expired"))?.to_owned();
    Ok((place_id, link_code))
}
