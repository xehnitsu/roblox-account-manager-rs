//! Encryption primitives.
//!
//! Two modes:
//! - DPAPI (default): bound to the current Windows user on this machine.
//! - Password: AES-256-GCM with an Argon2id-derived key.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result, anyhow, bail};

pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;

/// Extra entropy mixed into DPAPI so other apps using plain DPAPI for the same
/// user cannot trivially decrypt our blob.
const DPAPI_ENTROPY: &[u8] = b"RobloxAccountManagerRS/v1";

pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("OS RNG failed: {e}"))?;
    Ok(buf)
}

pub fn derive_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    argon2::Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("key derivation failed: {e}"))?;
    Ok(key)
}

/// Returns `salt || nonce || ciphertext`.
pub fn aes_encrypt(password: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let nonce: [u8; NONCE_LEN] = random_bytes()?;
    let key = derive_key(password, &salt)?;
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key));
    let ct = cipher.encrypt(&Nonce::from(nonce), plaintext).map_err(|_| anyhow!("AES-GCM encryption failed"))?;
    let mut out = Vec::with_capacity(SALT_LEN + NONCE_LEN + ct.len());
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn aes_decrypt(password: &str, blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < SALT_LEN + NONCE_LEN + 16 {
        bail!("encrypted data is truncated");
    }
    let (salt, rest) = blob.split_at(SALT_LEN);
    let (nonce, ct) = rest.split_at(NONCE_LEN);
    let key = derive_key(password, salt)?;
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key));
    let nonce: [u8; NONCE_LEN] = nonce.try_into().expect("length checked");
    cipher.decrypt(&Nonce::from(nonce), ct).map_err(|_| anyhow!("wrong password or corrupted file"))
}

pub fn dpapi_encrypt(plaintext: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData};
    let input = CRYPT_INTEGER_BLOB { cbData: plaintext.len() as u32, pbData: plaintext.as_ptr() as *mut u8 };
    let entropy = CRYPT_INTEGER_BLOB { cbData: DPAPI_ENTROPY.len() as u32, pbData: DPAPI_ENTROPY.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input/entropy point to live slices for the duration of the call;
    // output is allocated by DPAPI and released with LocalFree below.
    unsafe {
        CryptProtectData(
            &input,
            windows::core::PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .context("CryptProtectData failed")?;
        Ok(take_blob(output))
    }
}

pub fn dpapi_decrypt(blob: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData};
    let input = CRYPT_INTEGER_BLOB { cbData: blob.len() as u32, pbData: blob.as_ptr() as *mut u8 };
    let entropy = CRYPT_INTEGER_BLOB { cbData: DPAPI_ENTROPY.len() as u32, pbData: DPAPI_ENTROPY.as_ptr() as *mut u8 };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // SAFETY: same contract as in `dpapi_encrypt`.
    unsafe {
        CryptUnprotectData(&input, None, Some(&entropy), None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut output)
            .context("CryptUnprotectData failed (file from another user or machine?)")?;
        Ok(take_blob(output))
    }
}

/// Copies a DPAPI-allocated blob into a Vec and frees the original.
unsafe fn take_blob(blob: windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    if blob.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI guarantees pbData points to cbData readable bytes.
    let data = unsafe { std::slice::from_raw_parts(blob.pbData, blob.cbData as usize) }.to_vec();
    // SAFETY: pbData was allocated by DPAPI with LocalAlloc.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(blob.pbData as _)));
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_roundtrip() {
        let ct = aes_encrypt("hunter2", b"secret payload").unwrap();
        assert_eq!(aes_decrypt("hunter2", &ct).unwrap(), b"secret payload");
        assert!(aes_decrypt("wrong", &ct).is_err());
    }

    #[test]
    fn dpapi_roundtrip() {
        let ct = dpapi_encrypt(b"secret payload").unwrap();
        assert_ne!(ct, b"secret payload");
        assert_eq!(dpapi_decrypt(&ct).unwrap(), b"secret payload");
    }
}
