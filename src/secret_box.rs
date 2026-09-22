//! Secret-at-rest helper for third-party credentials (CS-21).
//!
//! `provider_keys.api_key` used to be stored in PLAINTEXT while the app already had AES-256-GCM for
//! private-email credentials. This module is the single place that seals/opens a tenant's provider
//! secret, so every read path goes through one implementation instead of six copies of a query.
//!
//! Format: `enc:v1:<base64(nonce || ciphertext)>`, tenant-scoped (the AES key is derived from the
//! server secret + the tenant id), which means one tenant's ciphertext cannot be opened with another
//! tenant's key even if the row is copied between them.
//!
//! `open()` NEVER fails: it returns the plaintext for a sealed value, transparently passes through a
//! legacy PLAINTEXT value (rows written before this change, which the startup backfill then seals),
//! and returns an empty string if a ciphertext cannot be authenticated. That is what makes the
//! rollout safe — a row that has not been migrated yet still resolves.

use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

const PREFIX: &str = "enc:v1:";

/// Is this value already sealed?
pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

/// Encrypt a provider secret for storage. An EMPTY value stays empty: callers use `api_key <> ''`
/// (and `!key.is_empty()`) to mean "no credential configured", and sealing the empty string would
/// silently turn every empty slot into a configured one.
pub fn seal(tenant_id: Uuid, plaintext: &str) -> Result<String, AppError> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    let ct = crate::private_email::encryption::encrypt_api_key(tenant_id, plaintext)
        .map_err(|e| AppError::Internal(format!("failed to seal provider key: {e}")))?;
    Ok(format!("{PREFIX}{ct}"))
}

/// Decrypt a stored provider secret. Never fails — see the module note.
pub fn open(tenant_id: Uuid, stored: &str) -> String {
    if stored.is_empty() {
        return String::new();
    }
    if let Some(ct) = stored.strip_prefix(PREFIX) {
        return match crate::private_email::encryption::decrypt_api_key(tenant_id, ct) {
            Ok(p) => p,
            Err(e) => {
                // Wrong tenant, truncated value, or rotated server secret. Loud, but never fatal:
                // the caller sees "no usable credential" instead of a 500.
                tracing::warn!(error = %e, "provider key could not be decrypted for this tenant");
                String::new()
            }
        };
    }
    // No prefix: either a legacy PLAINTEXT row (returned as-is, so behaviour is unchanged until the
    // backfill seals it) or a value sealed before the prefix existed. AES-GCM authenticates, so a
    // plaintext value cannot accidentally "decrypt" into something else.
    match crate::private_email::encryption::decrypt_api_key(tenant_id, stored) {
        Ok(p) => p,
        Err(_) => stored.to_string(),
    }
}

/// Does this stored value hold a usable credential? Used by the "is Telnyx configured" checks.
pub fn open_is_empty(tenant_id: Uuid, stored: &str) -> bool {
    open(tenant_id, stored).trim().is_empty()
}

/// Seal every provider key that is still plaintext. Idempotent, safe to run on every boot, and it
/// never touches a row it cannot read.
pub async fn backfill_provider_keys(db: &PgPool) -> Result<usize, AppError> {
    let rows: Vec<(Uuid, Uuid, String)> =
        sqlx::query_as("SELECT id, tenant_id, api_key FROM provider_keys WHERE api_key <> ''")
            .fetch_all(db)
            .await?;

    let mut sealed = 0usize;
    for (id, tenant_id, stored) in rows {
        if is_sealed(&stored) {
            continue;
        }
        // Only seal what we can read back: if the value were already ciphertext from an older
        // scheme, sealing the ciphertext would double-encrypt it.
        let plaintext = open(tenant_id, &stored);
        if plaintext.is_empty() {
            tracing::warn!(row = %id, "provider key is neither sealed nor readable as plaintext — left untouched");
            continue;
        }
        let out = seal(tenant_id, &plaintext)?;
        sqlx::query("UPDATE provider_keys SET api_key = $1, updated_at = now() WHERE id = $2")
            .bind(&out)
            .bind(id)
            .execute(db)
            .await?;
        // Count what was actually WRITTEN, and separately report whether it reads back — counting
        // only on the read-back made the log read "sealed=0" while a row had just been sealed, which
        // is exactly the kind of misleading line that wastes someone's afternoon.
        sealed += 1;
        if open(tenant_id, &out).is_empty() {
            tracing::error!(row = %id, "a freshly sealed provider key does not read back — INVESTIGATE");
        }
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_keys WHERE api_key <> '' AND api_key NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    tracing::info!(
        sealed_now = sealed,
        plaintext_remaining = remaining,
        "provider key encryption check"
    );
    Ok(sealed)
}

// ── CS-21b: seal-on-WRITE audit ────────────────────────────────────────────────────────────────
//
// Every read path is safe (`open` tolerates legacy plaintext), but nothing FAILED LOUDLY when a
// column held a plaintext secret: `provider_keys` did for months. A new column, a new handler, or
// one manual `UPDATE` can reintroduce that silently. This audit is the assertion — it runs on every
// boot and, with `CORESWIFT_SECRET_AUDIT=1`, as a one-shot check that exits non-zero.

/// `(table, secret column)` for tables that carry a `tenant_id`. A value here is acceptable when it
/// is `enc:v1:`-sealed or a ciphertext this tenant's key can actually open.
const TENANT_SECRET_COLUMNS: &[(&str, &str)] = &[
    ("provider_keys", "api_key"),
    ("private_email_api_keys", "api_key_encrypted"),
    ("private_email_domains", "mailgun_api_key"),
    ("private_email_domains", "smtp_password_encrypted"),
    ("private_email_domains", "webhook_signing_key_encrypted"),
];

/// `(table, secret column)` for GLOBAL config rows. There is no tenant id to derive a key from, so a
/// non-empty value here can never be sealed — the only safe state is empty, and a populated value is
/// reported as a finding so whoever fills it in knows to move it to a tenant slot.
const GLOBAL_SECRET_COLUMNS: &[(&str, &str)] = &[
    ("telnyx_config", "api_key"),
    ("telnyx_config", "webhook_secret"),
];

/// One stored value that must not be there.
#[derive(Debug, serde::Serialize)]
pub struct SecretFinding {
    pub table: &'static str,
    pub column: &'static str,
    pub row_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub reason: &'static str,
}

/// The audit's result. `plaintext` is the dangerous case (a credential readable by anyone with the
/// dump); `unreadable` is a credential the app itself cannot open — usually a rotated
/// `CORESWIFT_SECRET` or a row written by another deployment. Both are reported; only the first is a
/// failure, because a rotation is an incident to look at, not a write path that is leaking.
#[derive(Debug, Default, serde::Serialize)]
pub struct SecretAudit {
    pub plaintext: Vec<SecretFinding>,
    pub unreadable: Vec<SecretFinding>,
}

/// Would this value have to be ciphertext to look the way it does?
///
/// Used only to separate "a credential was written in the clear" from "ciphertext this key cannot
/// open". A real credential is almost never valid standard base64 AND long enough to be
/// nonce+ciphertext+tag (>= 28 bytes); anything holding `-`, `_`, `:` or a space is plainly a
/// credential. The residual blind spot — a long, purely `[A-Za-z0-9+/]` plaintext secret — still
/// lands in `unreadable`, i.e. still reported, just not labelled as plaintext.
fn ciphertext_shaped(stored: &str) -> bool {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    match B64.decode(stored) {
        Ok(bytes) => bytes.len() >= 28,
        Err(_) => false,
    }
}

/// Every stored secret that is in the clear or that the app cannot open. A healthy database has
/// `plaintext == []`; `unreadable` should be empty too, and is worth investigating when it is not.
pub async fn audit_plaintext_secrets(db: &PgPool) -> Result<SecretAudit, AppError> {
    let mut out = SecretAudit::default();

    for (table, column) in TENANT_SECRET_COLUMNS {
        let sql = format!(
            "SELECT id, tenant_id, {column} AS v FROM {table} \
             WHERE {column} IS NOT NULL AND {column} <> ''"
        );
        let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(&sql).fetch_all(db).await?;
        let mut sealed = 0usize;
        let mut opened = 0usize;
        for (row_id, tenant_id, stored) in rows {
            if is_sealed(&stored) {
                sealed += 1;
                continue;
            }
            // Not prefixed: it must still be ciphertext from the older `encryption::encrypt_api_key`
            // shape (that is what the private-email tables store). AES-GCM authenticates, so a
            // plaintext value cannot pass this by accident.
            if crate::private_email::encryption::decrypt_api_key(tenant_id, &stored).is_ok() {
                opened += 1;
                continue;
            }
            let shaped = ciphertext_shaped(&stored);
            let finding = SecretFinding {
                table,
                column,
                row_id,
                tenant_id: Some(tenant_id),
                reason: if shaped {
                    "ciphertext this tenant's key cannot open (rotated secret, or written by another deployment)"
                } else {
                    "not enc:v1:-sealed and not decryptable ciphertext — this looks like a plaintext credential"
                },
            };
            if shaped {
                out.unreadable.push(finding);
            } else {
                out.plaintext.push(finding);
            }
        }
        tracing::info!(
            table,
            column,
            sealed,
            opened,
            plaintext = out.plaintext.len(),
            unreadable = out.unreadable.len(),
            "secret column audit"
        );
    }

    for (table, column) in GLOBAL_SECRET_COLUMNS {
        let sql = format!(
            "SELECT id, {column} AS v FROM {table} WHERE {column} IS NOT NULL AND {column} <> ''"
        );
        let rows: Vec<(Uuid, String)> = sqlx::query_as(&sql).fetch_all(db).await?;
        for (row_id, _stored) in rows {
            out.plaintext.push(SecretFinding {
                table,
                column,
                row_id,
                tenant_id: None,
                reason:
                    "global config row: no tenant key to seal with, so the value must stay empty",
            });
        }
    }

    Ok(out)
}
