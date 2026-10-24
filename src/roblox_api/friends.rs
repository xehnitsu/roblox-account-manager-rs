//! Online friends of an account, with where they are.

use std::collections::HashMap;

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::presence::{Presence, presences};
use super::{check, client, cookie_header, send_with_csrf};

#[derive(Debug, Clone)]
pub struct Friend {
    pub user_id: u64,
    pub name: String,
    pub display_name: String,
    pub presence: Presence,
}

pub async fn online(cookie: &str, user_id: u64) -> Result<Vec<Friend>> {
    #[derive(Deserialize)]
    struct Item {
        id: u64,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<Item>,
    }
    let url = format!("https://friends.roblox.com/v1/users/{user_id}/friends/online");
    let resp = send_with_csrf(|| client().get(&url).header("Cookie", cookie_header(cookie))).await?;
    let ids: Vec<u64> = check(resp).await?.json::<Resp>().await?.data.into_iter().map(|i| i.id).collect();
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for chunk in ids.chunks(50) {
        found.extend(presences(cookie, chunk).await?);
    }
    let names = names(&ids).await.unwrap_or_default();
    Ok(found
        .into_iter()
        .map(|(id, presence)| {
            let (name, display_name) = names.get(&id).cloned().unwrap_or_else(|| (id.to_string(), String::new()));
            Friend { user_id: id, name, display_name, presence }
        })
        .collect())
}

async fn names(ids: &[u64]) -> Result<HashMap<u64, (String, String)>> {
    #[derive(Deserialize)]
    struct User {
        id: u64,
        name: String,
        #[serde(rename = "displayName")]
        display_name: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        data: Vec<User>,
    }
    let mut out = HashMap::new();
    for chunk in ids.chunks(100) {
        let body = json!({ "userIds": chunk, "excludeBannedUsers": false });
        let resp = send_with_csrf(|| client().post("https://users.roblox.com/v1/users").json(&body)).await?;
        for u in check(resp).await?.json::<Resp>().await?.data {
            out.insert(u.id, (u.name, u.display_name));
        }
    }
    Ok(out)
}
