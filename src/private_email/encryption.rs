use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use rand::Rng;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Encrypt a Mailgun API key with AES-256-GCM.
/// Key is derived from a server secret (env var) + tenant_id for per-tenant isolation.
pub fn encrypt_api_key(tenant_id: Uuid, plaintext: &str) -> Result<String, String> {
    let key = derive_key(tenant_id);
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

/// Decrypt a stored Mailgun API key.
pub fn decrypt_api_key(tenant_id: Uuid, encrypted: &str) -> Result<String, String> {
    let key = derive_key(tenant_id);
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

fn derive_key(tenant_id: Uuid) -> [u8; 32] {
    use std::env;

    let secret = env::var("CORESWIFT_SECRET").unwrap_or_else(|_| "coreswift-default-secret".into());
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(b":");
    hasher.update(tenant_id.as_bytes());
    let hash = hasher.finalize();

    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}
