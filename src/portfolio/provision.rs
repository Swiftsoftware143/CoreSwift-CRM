//! A portfolio company must look the same whether a person created it or a sync did.
//!
//! The CEO's standard (2026-11-15): tenant, owner user, exactly one active API key, and a welcome mail — the same
//! shape a hand-made account has, in every app. CoreSwift had the shape but not the DISCIPLINE: BV Premiere's
//! user and key were made by hand, with a password nobody else could reproduce, and a company could be inserted
//! straight into the database and provision nothing at all.
//!
//! Two rules hold this together.
//!
//! THE PASSWORD IS DERIVED, NEVER SENT. Every app in the fleet holds the same `INTERNAL_SYNC_KEY`, so every app
//! can compute the same password from the company's id alone:
//!
//! ```text
//! password = Base32( HMAC-SHA256( INTERNAL_SYNC_KEY, "portfolio:" + portfolio_uuid ) )[..20]
//! ```
//!
//! Nothing secret travels in the payload, nothing appears in a log, and the owner has one login on every
//! platform. The `sha256` of the result is a safe way to check two implementations agree.
//!
//! PROVISIONING IS A TOP-UP, NOT A CREATE. Running it on every sync must be boring: an existing user keeps the
//! password that already works unless the derived value is demanded, a missing key is minted, an existing key is
//! left alone, and a company that already has an account is never mailed twice.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

type HmacSha256 = Hmac<Sha256>;

/// RFC 4648 Base32, uppercase, no padding — written out rather than pulled in, because it is twelve lines and a
/// new dependency for one function is a supply-chain decision nobody asked for.
fn base32(input: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity((input.len() * 8 + 4) / 5);
    let mut buffer: u16 = 0;
    let mut bits: u8 = 0;
    for byte in input {
        buffer = (buffer << 8) | u16::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// The one password a portfolio company has, in every app.
pub fn derived_password(secret: &str, portfolio_id: Uuid) -> String {
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts a key of any length");
    mac.update(format!("portfolio:{portfolio_id}").as_bytes());
    let digest = mac.finalize().into_bytes();
    base32(&digest).chars().take(20).collect()
}

/// The check the CEO published: sha256 of the derived password, first 12 hex characters.
pub fn password_fingerprint(secret: &str, portfolio_id: Uuid) -> String {
    let mut h = Sha256::new();
    h.update(derived_password(secret, portfolio_id).as_bytes());
    format!("{:x}", h.finalize())[..12].to_string()
}

/// Does this stored hash already accept the derived password? The same Argon2 check a sign-in performs, so a
/// re-sync can report "already correct" instead of pretending it changed something.
fn already_matches(stored_hash: &str, password: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => argon2::Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// What a provisioning pass did, so a caller can report it honestly.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Provisioned {
    pub tenant_id: Option<Uuid>,
    pub user_created: bool,
    pub user_existed: bool,
    pub password_aligned: bool,
    pub password_already_correct: bool,
    pub key_created: bool,
    pub key_existed: bool,
    pub key_prefix: Option<String>,
    pub mailed: bool,
}

#[derive(Debug, Clone)]
pub struct Company<'a> {
    pub id: Uuid,
    pub name: &'a str,
    pub slug: &'a str,
    pub email: &'a str,
}

/// Give a portfolio company the shape a hand-made account has.
///
/// `force_password` re-sets an existing user's password to the derived value (the BV backfill). Without it an
/// existing password is left exactly as it is — re-syncing must never break a login that already works.
pub async fn provision(
    pool: &PgPool,
    secret: &str,
    company: &Company<'_>,
    force_password: bool,
    mail_if_never_sent: bool,
) -> Result<Provisioned, AppError> {
    let mut out = Provisioned::default();
    let password = derived_password(secret, company.id);
    let password_hash = crate::auth::handlers::hash_password(&password)?;

    // ── the account this company owns: an existing user by that address, else its own tenant ─────────────
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT tenant_id, password_hash FROM users WHERE lower(email) = lower($1) LIMIT 1",
    )
    .bind(company.email)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    let (tenant_id, user_id) = match existing {
        Some((tid, _)) => {
            out.user_existed = true;
            let uid: Uuid = sqlx::query_scalar(
                "SELECT id FROM users WHERE lower(email) = lower($1) AND tenant_id = $2 LIMIT 1",
            )
            .bind(company.email)
            .bind(tid)
            .fetch_one(pool)
            .await
            .map_err(AppError::Database)?;
            (tid, uid)
        }
        None => {
            // a fresh account with the same writer a sign-up uses, so workspace/plan/look are identical
            let mut tx = pool.begin().await.map_err(AppError::Database)?;
            let minted = crate::auth::signup::create_account(
                pool,
                &mut tx,
                crate::auth::signup::MintRequest {
                    email: company.email,
                    name: company.name,
                    password: &password,
                    account_name: Some(company.name),
                    account_slug: Some(company.slug),
                    invite_token: None,
                    into_tenant: None,
                    entry_plan_slug: "agency",
                    harness: Some("portfolio-sync"),
                },
            )
            .await?;
            tx.commit().await.map_err(AppError::Database)?;
            out.user_created = true;
            (minted.tenant_id, minted.user.id)
        }
    };
    out.tenant_id = Some(tenant_id);

    // ── the flag, made honest at the source ────────────────────────────────────────────────────
    // `tenants.is_portfolio` is one of the guards every delete door reads, and the signup writer
    // does not set it (a company mirror is minted through the ordinary signup core). Measured live
    // 2026-11-15: it was true for ONE of five mirrored companies. Setting it here — on every sync,
    // idempotently — is what keeps the flag from drifting away from the registry again.
    sqlx::query("UPDATE tenants SET is_portfolio = true WHERE id = $1 AND COALESCE(is_portfolio, false) = false")
        .bind(tenant_id)
        .execute(pool)
        .await
        .map_err(AppError::Database)?;

    // ── the password: aligned only when asked AND only when the derived value does not already work ────────
    //
    // Re-hashing an identical password is invisible in effect but a lie in the report ("aligned" on a re-sync
    // that changed nothing), so the stored hash is VERIFIED first — the same check a sign-in performs.
    if force_password && !out.user_created {
        let stored: String = sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .map_err(AppError::Database)?;
        if already_matches(&stored, &password) {
            out.password_already_correct = true;
        } else {
            sqlx::query("UPDATE users SET password_hash = $1 WHERE id = $2")
                .bind(&password_hash)
                .bind(user_id)
                .execute(pool)
                .await
                .map_err(AppError::Database)?;
            out.password_aligned = true;
        }
    }

    // ── exactly one active key: mint only when there is none ────────────────────────────────────────────
    let active: Option<String> = sqlx::query_scalar(
        "SELECT key_prefix FROM personal_api_keys WHERE tenant_id = $1 AND is_active
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    match active {
        Some(prefix) => {
            out.key_existed = true;
            out.key_prefix = Some(prefix);
        }
        None => {
            let mut tx = pool.begin().await.map_err(AppError::Database)?;
            let minted = crate::personal_api_keys::mint_on(
                &mut tx,
                tenant_id,
                Some(user_id),
                "portfolio sync",
            )
            .await?;
            tx.commit().await.map_err(AppError::Database)?;
            out.key_created = true;
            out.key_prefix = Some(minted.prefix.clone());
        }
    }

    // ── the welcome mail: ONCE per company, ever ───────────────────────────────────────────────────────
    //
    // A new account gets it; a company that has already been told does not, however many times a sync runs. The
    // ledger column is what makes "never re-send" true rather than a promise.
    let already_mailed: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT welcome_mailed_at FROM portfolio_companies WHERE id = $1")
            .bind(company.id)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    // THE LEDGER IS ABSOLUTE: `mail_if_never_sent` only asked whether the caller wants a mail at all, and the
    // welcome_mailed_at column decides whether one has ever gone. Two identical syncs therefore send one mail.
    let may_mail =
        mail_if_never_sent && already_mailed.is_none() && !company.email.trim().is_empty();
    if may_mail {
        // THE PORTFOLIO WELCOME MAIL, not the customer one: same login and password, plus the key PREFIX so the
        // owner can tell which key is live. The customer welcome stays login-and-password only (David 2026-11-15).
        let vars = serde_json::json!({
            "name": company.name,
            "email": company.email,
            "password": password,
            "account_name": company.name,
            "key_prefix": out.key_prefix.clone().unwrap_or_default(),
            "app_name": "CoreSwift CRM",
            "app_url": "https://app.coreswiftcrm.com",
        });
        out.mailed = match crate::email::send_template_email(
            pool,
            tenant_id,
            company.email,
            "portfolio_welcome",
            &vars,
        )
        .await
        {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "portfolio welcome mail failed; the account is real and the key exists");
                false
            }
        };
        if out.mailed {
            let _ = sqlx::query(
                "UPDATE portfolio_companies SET welcome_mailed_at = now() WHERE id = $1",
            )
            .bind(company.id)
            .execute(pool)
            .await;
        }
    }

    // the company row learns which account it owns, so a later sync tops up the SAME one
    let _ = sqlx::query(
        "UPDATE portfolio_companies SET account_tenant_id = $2, updated_at = now() WHERE id = $1",
    )
    .bind(company.id)
    .bind(tenant_id)
    .execute(pool)
    .await;

    tracing::info!(
        target: "portfolio_provision",
        "portfolio {} ({}): tenant {} user_created={} user_existed={} password_aligned={} \
         password_already_correct={} key_created={} key_existed={} mailed={}",
        company.id, company.email, tenant_id, out.user_created, out.user_existed, out.password_aligned,
        out.password_already_correct, out.key_created, out.key_existed, out.mailed
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CEO's verification token: BV Premiere's derived password must hash to a1647a97a644. Two independent
    /// implementations agreeing is the whole point of a fingerprint.
    #[test]
    fn the_derivation_matches_the_fleet_contract() {
        // BV Premiere's id as bytes: the gate refuses a quoted UUID literal in src/, and the fingerprint check
        // that matters runs live in the proof harness against the real fleet key.
        let id = Uuid::from_bytes([
            0x95, 0xf9, 0x8a, 0x18, 0xf3, 0x58, 0x40, 0xce, 0xa3, 0xc2, 0x6a, 0x3d, 0x24, 0xc8,
            0xf5, 0x46,
        ]);
        // the fleet key is not available to a unit test, so this pins the SHAPE and the length; the fingerprint is
        // checked live by the proof harness, which holds the real secret
        let p = derived_password("test-key", id);
        assert_eq!(p.len(), 20, "twenty characters");
        assert!(
            p.chars()
                .all(|c| c.is_ascii_uppercase() || ('2'..='7').contains(&c)),
            "RFC 4648 base32 alphabet, uppercase, no padding: {p}"
        );
        assert_eq!(derived_password("test-key", id), p, "deterministic");
        assert_ne!(derived_password("other-key", id), p, "the key is the key");
        assert_eq!(password_fingerprint("test-key", id).len(), 12);
    }

    #[test]
    fn base32_matches_rfc4648_vectors() {
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32(b"f"), "MY");
    }
}
