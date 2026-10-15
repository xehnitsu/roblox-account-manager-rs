use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::{check, client, cookie_header, send_with_csrf};

#[derive(Debug, Clone, Default, Deserialize)]
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
    /// Opaque tokens for the players' avatar headshots (see `find_player`).
    #[serde(rename = "playerTokens", default)]
    pub player_tokens: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ServerPage {
    pub servers: Vec<Server>,
    pub next_cursor: Option<String>,
}

/// One page (up to 100) of public servers. The public API does not expose a
/// region field, so region is not available here. Player tokens are only
/// filled in when an account's cookie is given.
pub async fn public_servers(place_id: u64, cursor: Option<&str>, cookie: Option<&str>) -> Result<ServerPage> {
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
    // `send_with_csrf` also retries when rate-limited.
    let resp = send_with_csrf(|| {
        let req = client().get(&url);
        match cookie {
            Some(c) => req.header("Cookie", cookie_header(c)),
            None => req,
        }
    })
    .await?;
    let resp: Resp = check(resp).await?.json().await?;
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

async fn universe_id(place_id: u64) -> Result<u64> {
    #[derive(Deserialize)]
    struct Universe {
        #[serde(rename = "universeId")]
        universe_id: Option<u64>,
    }
    let u: Universe =
        check(client().get(format!("https://apis.roblox.com/universes/v1/places/{place_id}/universe")).send().await?)
            .await?
            .json()
            .await?;
    u.universe_id.ok_or_else(|| anyhow!("place {place_id} not found"))
}

/// All places of the game (universe) the place belongs to, as `(id, name)`.
pub async fn universe_places(place_id: u64) -> Result<Vec<(u64, String)>> {
    #[derive(Deserialize)]
    struct Place {
        id: u64,
        name: String,
    }
    #[derive(Deserialize)]
    struct Page {
        data: Vec<Place>,
        #[serde(rename = "nextPageCursor")]
        next_page_cursor: Option<String>,
    }
    let universe_id = universe_id(place_id).await?;
    let mut places = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = format!("https://develop.roblox.com/v1/universes/{universe_id}/places?sortOrder=Asc&limit=100");
        if let Some(c) = &cursor {
            url.push_str("&cursor=");
            url.push_str(&url::form_urlencoded::byte_serialize(c.as_bytes()).collect::<String>());
        }
        let page: Page = check(send_with_csrf(|| client().get(&url)).await?).await?.json().await?;
        places.extend(page.data.into_iter().map(|p| (p.id, p.name)));
        match page.next_page_cursor {
            Some(c) => cursor = Some(c),
            None => return Ok(places),
        }
    }
}

pub async fn place_name(place_id: u64) -> Result<String> {
    #[derive(Deserialize)]
    struct Game {
        name: String,
    }
    #[derive(Deserialize)]
    struct Games {
        data: Vec<Game>,
    }
    let universe_id = universe_id(place_id).await?;
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

/// A game in search results.
#[derive(Debug, Clone)]
pub struct GameHit {
    pub name: String,
    pub root_place_id: u64,
    pub players: u64,
}

/// Searches Roblox games by name, like the website's search bar.
pub async fn search(query: &str) -> Result<Vec<GameHit>> {
    #[derive(Deserialize)]
    struct Content {
        #[serde(rename = "contentType", default)]
        content_type: String,
        #[serde(default)]
        name: String,
        #[serde(rename = "rootPlaceId", default)]
        root_place_id: u64,
        #[serde(rename = "playerCount", default)]
        player_count: u64,
    }
    #[derive(Deserialize)]
    struct Group {
        #[serde(default)]
        contents: Vec<Content>,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "searchResults", default)]
        search_results: Vec<Group>,
    }
    let q: String = url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect();
    let url = format!(
        "https://apis.roblox.com/search-api/omni-search?searchQuery={q}&sessionId={}&pageType=all",
        uuid::Uuid::new_v4()
    );
    let resp: Resp = check(send_with_csrf(|| client().get(&url)).await?).await?.json().await?;
    Ok(resp
        .search_results
        .into_iter()
        .flat_map(|g| g.contents)
        .filter(|c| c.content_type == "Game" && c.root_place_id != 0)
        .map(|c| GameHit { name: c.name, root_place_id: c.root_place_id, players: c.player_count })
        .collect())
}
