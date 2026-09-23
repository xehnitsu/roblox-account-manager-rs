//! Encrypted account file: `%APPDATA%\RobloxAccountManagerRS\accounts.dat`.
//!
//! Layout: `MAGIC (6 bytes) || mode (1 byte) || payload`
//! - mode 0: payload is a DPAPI blob
//! - mode 1: payload is `salt || nonce || AES-256-GCM ciphertext`
//!
//! The decrypted payload is JSON (`Vec<Account>`).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::crypto;
use super::model::Account;

const MAGIC: &[u8; 6] = b"RAMRS1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    Dpapi,
    Password(String),
}

impl Protection {
    pub fn is_password(&self) -> bool {
        matches!(self, Protection::Password(_))
    }
}

pub fn accounts_path() -> PathBuf {
    crate::config::data_dir().join("accounts.dat")
}

/// Whether the file on disk needs a password to open. `None` if no file exists yet.
pub fn stored_mode(path: &Path) -> Result<Option<bool>> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < MAGIC.len() + 1 || &bytes[..MAGIC.len()] != MAGIC {
        bail!("{} is not a valid account file", path.display());
    }
    Ok(Some(bytes[MAGIC.len()] == 1))
}

pub fn load(path: &Path, protection: &Protection) -> Result<Vec<Account>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    if bytes.len() < MAGIC.len() + 1 || &bytes[..MAGIC.len()] != MAGIC {
        bail!("{} is not a valid account file", path.display());
    }
    let mode = bytes[MAGIC.len()];
    let payload = &bytes[MAGIC.len() + 1..];
    let plain = match (mode, protection) {
        (0, _) => crypto::dpapi_decrypt(payload)?,
        (1, Protection::Password(pw)) => crypto::aes_decrypt(pw, payload)?,
        (1, Protection::Dpapi) => bail!("account file is password protected"),
        (m, _) => bail!("unknown encryption mode {m}"),
    };
    serde_json::from_slice(&plain).context("account file JSON is corrupt")
}

/// Writes atomically (temp file + rename) so a crash never leaves a half-written file.
pub fn save(path: &Path, protection: &Protection, accounts: &[Account]) -> Result<()> {
    let plain = serde_json::to_vec(accounts)?;
    let (mode, payload) = match protection {
        Protection::Dpapi => (0u8, crypto::dpapi_encrypt(&plain)?),
        Protection::Password(pw) => (1u8, crypto::aes_encrypt(pw, &plain)?),
    };
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + payload.len());
    out.extend_from_slice(MAGIC);
    out.push(mode);
    out.extend_from_slice(&payload);

    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("dat.tmp");
    fs::write(&tmp, &out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::model::AuthMethod;

    fn sample() -> Vec<Account> {
        vec![Account {
            id: uuid::Uuid::new_v4(),
            user_id: 1,
            username: "builderman".into(),
            display_name: "builderman".into(),
            avatar_url: None,
            auth_method: AuthMethod::Cookie,
            cookie: "_|WARNING:-test".into(),
            login_username: None,
            login_password: None,
            group: "main".into(),
            notes: String::new(),
        }]
    }

    #[test]
    fn roundtrip_both_modes() {
        let dir = std::env::temp_dir().join(format!("ramrs-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("accounts.dat");

        save(&path, &Protection::Dpapi, &sample()).unwrap();
        assert_eq!(stored_mode(&path).unwrap(), Some(false));
        assert_eq!(load(&path, &Protection::Dpapi).unwrap()[0].username, "builderman");

        let pw = Protection::Password("pw".into());
        save(&path, &pw, &sample()).unwrap();
        assert_eq!(stored_mode(&path).unwrap(), Some(true));
        assert!(load(&path, &Protection::Dpapi).is_err());
        assert!(load(&path, &Protection::Password("nope".into())).is_err());
        assert_eq!(load(&path, &pw).unwrap()[0].group, "main");

        let _ = fs::remove_dir_all(dir);
    }
}
