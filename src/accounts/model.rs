use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthMethod {
    Cookie,
    Credentials,
}

/// One managed Roblox account. The whole account list is encrypted at rest
/// (see `storage.rs`), so secrets are plain strings inside this struct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: Uuid,
    pub user_id: u64,
    pub username: String,
    pub display_name: String,
    #[serde(default)]
    pub avatar_url: Option<String>,
    pub auth_method: AuthMethod,
    pub cookie: String,
    /// Username kept for credential accounts so the cookie can be refreshed later.
    #[serde(default)]
    pub login_username: Option<String>,
    #[serde(default)]
    pub login_password: Option<String>,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub notes: String,
}

impl Account {
    pub fn label(&self) -> String {
        if self.display_name.is_empty() || self.display_name == self.username {
            self.username.clone()
        } else {
            format!("{} (@{})", self.display_name, self.username)
        }
    }
}
