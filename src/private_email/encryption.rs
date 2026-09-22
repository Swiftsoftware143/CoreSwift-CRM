//! AES-256-GCM for tenant-supplied secrets at rest.
//!
//! Key = SHA256(master_secret || ":" || tenant_id): the same plaintext under two tenants is two
//! different keys, so a row copied between tenants does not yield a usable credential (CS-21).
//!
//! The master secret comes from the ENVIRONMENT (`CORESWIFT_SECRET`). It used to come from a string
//! literal in this file, which undid the encryption: a database dump plus a read of the source tree
//! decrypted every customer's provider key (proved live 2026-09-22 — a stored 29-char credential was
//! returned by an offline decrypt using the literal below). Writes now FAIL CLOSED when the variable
//! is missing; reads stay tolerant (`decrypt_api_key_legacy`) so rows sealed before the key moved
//! into the environment still resolve, and the boot re-key migrates them onto the configured key.
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// The string literal that used to BE the master key. Kept only so pre-existing rows can be opened
/// once and re-keyed; it is never used to encrypt.
pub const LEGACY_DEFAULT_SECRET: &str = "coreswift-default-secret";

const ENV_VAR: &str = "CORESWIFT_SECRET";
const MIN_SECRET_LEN: usize = 32;

/// Is a usable master secret configured? Writes are refused when this is false.
pub fn is_configured() -> bool {
    env_secret().is_some()
}

fn env_secret() -> Option<String> {
    match std::env::var(ENV_VAR) {
        Ok(v) if v.len() >= MIN_SECRET_LEN => Some(v),
        Ok(v) => {
            tracing::error!(
                len = v.len(),
                min = MIN_SECRET_LEN,
                "{} is too short to be a master key — refusing to use it",
                ENV_VAR
            );
            None
        }
        Err(_) => None,
    }
}

/// Encrypt a secret for storage. Fails closed: with no configured master key there is no safe
/// encryption, and falling back to a key that lives in the source tree is not encryption at rest.
pub fn encrypt_api_key(tenant_id: Uuid, plaintext: &str) -> Result<String, String> {
    let secret = env_secret().ok_or_else(|| {
        format!(
            "{ENV_VAR} is not set (needs >= {MIN_SECRET_LEN} chars): refusing to encrypt a credential \
             with the built-in default key"
        )
    })?;
    encrypt_with(&secret, tenant_id, plaintext)
}

pub fn encrypt_with(secret: &str, tenant_id: Uuid, plaintext: &str) -> Result<String, String> {
    let key = derive_key_from(secret, tenant_id);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher init: {}", e))?;

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| format!("encrypt: {}", e))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(&combined))
}

/// Decrypt a stored secret with the CONFIGURED master key.
pub fn decrypt_api_key(tenant_id: Uuid, encrypted: &str) -> Result<String, String> {
    let secret = env_secret().ok_or_else(|| format!("{ENV_VAR} is not set"))?;
    decrypt_with(&secret, tenant_id, encrypted)
}

/// Decrypt with the built-in default key: read-only support for rows sealed before the master key
/// moved into the environment (the boot re-key rewrites them; this path is the safety net).
pub fn decrypt_api_key_legacy(tenant_id: Uuid, encrypted: &str) -> Result<String, String> {
    decrypt_with(LEGACY_DEFAULT_SECRET, tenant_id, encrypted)
}

pub fn decrypt_with(secret: &str, tenant_id: Uuid, encrypted: &str) -> Result<String, String> {
    let key = derive_key_from(secret, tenant_id);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("cipher init: {}", e))?;

    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| format!("base64 decode: {}", e))?;

    if combined.len() < 12 {
        return Err("ciphertext too short".into());
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| format!("decrypt: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| format!("utf8: {}", e))
}

fn derive_key_from(secret: &str, tenant_id: Uuid) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(tenant_id.as_bytes());
    let hash = hasher.finalize();

    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}
