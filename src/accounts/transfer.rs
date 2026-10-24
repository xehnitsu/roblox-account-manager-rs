//! Export / import file for moving accounts and settings to another PC.

//! Layout: `MAGIC (6 bytes) || salt || nonce || AES-256-GCM ciphertext`, password-keyed with Argon2.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::crypto;
use super::model::Account;
use crate::config::Config;

const MAGIC: &[u8; 6] = b"RAMRSX";
pub const EXTENSION: &str = "ramrs";

#[derive(Serialize, Deserialize)]
pub struct Bundle {
    pub accounts: Vec<Account>,
    pub settings: Config,
}

pub fn export(path: &Path, password: &str, bundle: &Bundle) -> Result<()> {
    let plain = serde_json::to_vec(bundle)?;
    let mut out = MAGIC.to_vec();
    out.extend(crypto::aes_encrypt(password, &plain)?);
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

pub fn import(path: &Path, password: &str) -> Result<Bundle> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let Some(blob) = bytes.strip_prefix(MAGIC) else { bail!("{} is not an export file", path.display()) };
    let plain = crypto::aes_decrypt(password, blob).context("wrong password or damaged file")?;
    serde_json::from_slice(&plain).context("export file is damaged")
}

/// Adds imported accounts, or updates the existing account with the same
/// Roblox user ID. Returns (added, updated).
pub fn merge(accounts: &mut Vec<Account>, incoming: Vec<Account>) -> (usize, usize) {
    let (mut added, mut updated) = (0, 0);
    for new in incoming {
        match accounts.iter_mut().find(|a| a.user_id == new.user_id) {
            Some(a) => {
                *a = Account { id: a.id, ..new };
                updated += 1;
            }
            None => {
                accounts.push(new);
                added += 1;
            }
        }
    }
    (added, updated)
}

pub fn downloads() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_default();
    home.join("Downloads")
}

/// `roblox-accounts-<date>.ramrs`.
pub fn default_file_name() -> String {
    format!("roblox-accounts-{}.{EXTENSION}", chrono::Local::now().format("%Y-%m-%d"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::AuthMethod;

    fn account(user_id: u64, group: &str) -> Account {
        Account {
            id: uuid::Uuid::new_v4(),
            user_id,
            username: format!("user{user_id}"),
            display_name: String::new(),
            avatar_url: None,
            auth_method: AuthMethod::Cookie,
            cookie: format!("cookie{user_id}"),
            login_username: None,
            login_password: None,
            group: group.into(),
            notes: String::new(),
        }
    }

    #[test]
    fn roundtrip_and_merge() {
        let path = std::env::temp_dir().join(format!("ramrs-export-{}.{EXTENSION}", uuid::Uuid::new_v4()));
        let settings = Config { group_order: vec!["main".into()], ..Default::default() };
        export(&path, "pw", &Bundle { accounts: vec![account(1, "main"), account(2, "")], settings }).unwrap();
        assert!(import(&path, "wrong").is_err());
        let bundle = import(&path, "pw").unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(bundle.settings.group_order, ["main"]);

        let mut existing = vec![account(1, "old")];
        let id = existing[0].id;
        assert_eq!(merge(&mut existing, bundle.accounts), (1, 1));
        assert_eq!((existing[0].id, existing[0].group.as_str()), (id, "main"));
        assert_eq!(existing.len(), 2);
    }
}
