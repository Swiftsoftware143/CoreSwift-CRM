//! The FREE mailbox's ADDRESS, on a shared domain CoreSwift owns.
//!
//! WHY A SHARED DOMAIN (CEO spec, David 2026-11-11)
//!
//! A free account owns no domain and no Mailgun account, so the "add your domain then create a
//! mailbox" path that every paid tier uses is a wall for exactly the person the free tier exists to
//! win. This module gives them a real address on a domain we already receive mail on —
//! `<name>@mail.coreswiftcrm.com` — with no DNS work, no provider account and no purchase.
//!
//! HOW IT STAYS TENANT-SAFE
//!
//! The shared domain is not shared as a ROW: each free tenant gets its own `private_email_domains`
//! row for it, carrying that tenant's own sealed copy of the platform credentials. Inbound routing
//! is by ADDRESS (`private_email_boxes.email_address`), so the message still lands only in the
//! tenant that owns the mailbox. Two tenants can therefore never share an address, which is why
//! the claim path enforces uniqueness ACROSS tenants on this domain (`private_email_boxes`' own
//! unique index is per tenant, and the inbound router's `LIMIT 1` would otherwise pick a winner).
//!
//! The Mailgun route that delivers `*@mail.coreswiftcrm.com` to this app is a deploy-time fact of
//! the platform account (one route covers every business domain), not something a tenant creates.
//!
//! OPERATOR CONFIG (env, never a Rust literal):
//!   FREE_INBOX_DOMAIN              the shared domain (default `mail.coreswiftcrm.com`)
//!   EMAIL_API_KEY                  the platform Mailgun key — sealed per tenant at claim time
//!   FREE_INBOX_WEBHOOK_SIGNING_KEY the Mailgun account's webhook signing key — sealed per tenant
//!
//! Absent credentials produce a 503 naming what the operator must set, never a 500: nothing here
//! is a fault in this service.

use axum::{extract::State, Extension, Json};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::models::PrivateEmailBox;
use super::{feature_gate, trial};
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The domain CoreSwift receives free-tier mail on. Overridable by `FREE_INBOX_DOMAIN` so the
/// operator can move it without a code change.
const DEFAULT_FREE_DOMAIN: &str = "mail.coreswiftcrm.com";

/// Local parts nobody may claim: they are provider/abuse mailboxes, and every one of them would
/// collect bounces rather than a person's mail.
const RESERVED_LOCAL_PARTS: &[&str] = &[
    "postmaster",
    "abuse",
    "mailer-daemon",
    "noreply",
    "no-reply",
];

pub fn free_domain() -> String {
    std::env::var("FREE_INBOX_DOMAIN")
        .ok()
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_FREE_DOMAIN.to_string())
}

/// A local part that can live on a shared public domain: lowercase, letters/digits/dot/dash/
/// underscore, 3–32 chars. Everything else is refused with a sentence that says the rule.
fn normalise_local_part(raw: &str) -> Result<String, AppError> {
    let cleaned: String = raw
        .trim()
        .to_lowercase()
        .replace(' ', ".")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let cleaned = cleaned.trim_matches(['.', '-', '_']).to_string();
    if cleaned.len() < 3 || cleaned.len() > 32 {
        return Err(AppError::Validation(
            "Choose a name between 3 and 32 characters — letters, numbers, dots, dashes or \
             underscores. It becomes the part before the @ sign."
                .into(),
        ));
    }
    if RESERVED_LOCAL_PARTS.contains(&cleaned.as_str()) {
        return Err(AppError::Validation(format!(
            "\"{}\" is reserved for the mail system, so it cannot be used as an inbox. Pick another \
             name.",
            cleaned
        )));
    }
    Ok(cleaned)
}

#[derive(Debug, Deserialize, Default)]
pub struct ClaimFreeInboxRequest {
    /// The part before the @. Omitted = derived from the signed-in user's own address.
    #[serde(default)]
    pub local_part: Option<String>,
}

fn env_cred(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// The tenant's own row for the shared domain, created on first claim.
///
/// One row per tenant (the table's `(tenant_id, domain)` unique index), each holding that tenant's
/// sealed copy of the platform credentials — which is what lets the inbound receiver validate the
/// delivery with mailgun's account signing key without ever reading another tenant's ciphertext.
async fn ensure_shared_domain(pool: &PgPool, tenant_id: Uuid) -> Result<Uuid, AppError> {
    let domain = free_domain();

    if let Some((id,)) = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM private_email_domains WHERE tenant_id = $1 AND lower(domain) = lower($2)",
    )
    .bind(tenant_id)
    .bind(&domain)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?
    {
        return Ok(id);
    }

    let mailgun_key = env_cred("EMAIL_API_KEY").ok_or_else(|| {
        AppError::ServiceUnavailable(
            "The free mailbox is not switched on for this deployment yet: the server has no \
             Mailgun key configured. An operator must set EMAIL_API_KEY."
                .into(),
        )
    })?;
    let webhook_key = env_cred("FREE_INBOX_WEBHOOK_SIGNING_KEY")
        .or_else(|| env_cred("MAILGUN_WEBHOOK_SIGNING_KEY"))
        .ok_or_else(|| {
            AppError::ServiceUnavailable(
                "The free mailbox cannot receive mail yet: the server has no Mailgun webhook \
                 signing key configured, so an inbound delivery could not be authenticated. An \
                 operator must set FREE_INBOX_WEBHOOK_SIGNING_KEY."
                    .into(),
            )
        })?;

    let sealed_key = crate::secret_box::seal(tenant_id, &mailgun_key)?;
    let sealed_webhook = crate::secret_box::seal(tenant_id, &webhook_key)?;

    // ON CONFLICT DO NOTHING + re-select: two concurrent claims (a double-clicked button) must both
    // end up with the SAME row rather than one of them 500-ing on the unique index.
    sqlx::query(
        r#"
        INSERT INTO private_email_domains
            (tenant_id, domain, mailgun_api_key, mailgun_region, label, provider_type, verified,
             catch_all_enabled, webhook_signing_key_encrypted)
        VALUES ($1, $2, $3, 'us', $4, 'mailgun', true, false, $5)
        ON CONFLICT (tenant_id, domain) DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .bind(&domain)
    .bind(&sealed_key)
    .bind("CoreSwift free mailbox")
    .bind(&sealed_webhook)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM private_email_domains WHERE tenant_id = $1 AND lower(domain) = lower($2)",
    )
    .bind(tenant_id)
    .bind(&domain)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)
}

/// Pick the local part for a claim: the caller's, else the signed-in user's own email local part,
/// else `inbox`.
async fn suggested_local_part(
    pool: &PgPool,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    requested: Option<&str>,
) -> Result<String, AppError> {
    if let Some(wanted) = requested.filter(|v| !v.trim().is_empty()) {
        return normalise_local_part(wanted);
    }

    // The person's own address is the least surprising default: a free signup becomes
    // `david@mail.coreswiftcrm.com` without them typing anything.
    let email: Option<String> = match user_id {
        Some(uid) => sqlx::query_scalar("SELECT email FROM users WHERE id = $1 AND tenant_id = $2")
            .bind(uid)
            .bind(tenant_id)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Database)?,
        None => None,
    };

    for candidate in [email.as_deref(), Some("inbox")].into_iter().flatten() {
        let local = candidate.split('@').next().unwrap_or("").to_lowercase();
        if let Ok(clean) = normalise_local_part(&local) {
            return Ok(clean);
        }
    }
    Ok("inbox".to_string())
}

/// POST /api/private-email/free-inbox — give this FREE account its one mailbox, now.
///
/// Idempotent: claiming twice returns the mailbox that already exists instead of erroring, so the
/// console can call it on first load without a "did I already do this?" dance. The 2nd MAILBOX is
/// refused by the shipped registry check (`check_mailbox_limit`), which is the same sentence every
/// other create path uses.
pub async fn claim_free_inbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ClaimFreeInboxRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).ok();

    // A plan that sells sending already owns this ground: the shared address is the FREE path, and
    // handing a paying customer a `@mail.coreswiftcrm.com` address instead of their own domain
    // would be a downgrade dressed as a feature.
    if trial::plan_sells_sending(&state.db, tenant_id).await? {
        return Err(AppError::UpgradeRequired(
            "Your plan sends from your own domain, so add that domain under Domains and create the \
             mailbox there. The free CoreSwift address is for the Free plan."
                .into(),
        ));
    }

    // THE plan ceiling: 1 mailbox on Free, enforced by the registry, with the normal sentence.
    feature_gate::check_mailbox_limit(&state.db, tenant_id).await?;

    let domain_id = ensure_shared_domain(&state.db, tenant_id).await?;
    let domain = free_domain();
    let local_part =
        suggested_local_part(&state.db, tenant_id, user_id, req.local_part.as_deref()).await?;
    let address = format!("{}@{}", local_part, domain);

    // Uniqueness ACROSS tenants on the shared domain: the inbound router matches on the address
    // alone, so a second tenant holding the same address would silently steal (or lose) mail.
    if let Some((existing_tenant, existing_id, existing_address)) =
        sqlx::query_as::<_, (Uuid, Uuid, String)>(
            "SELECT tenant_id, id, email_address FROM private_email_boxes
          WHERE lower(email_address) = lower($1) AND status = 'active' LIMIT 1",
        )
        .bind(&address)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?
    {
        if existing_tenant == tenant_id {
            // Already yours — this is the idempotent re-claim, not an error.
            let box_row = sqlx::query_as::<_, PrivateEmailBox>(
                "SELECT * FROM private_email_boxes WHERE id = $1",
            )
            .bind(existing_id)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::Database)?;
            return Ok(Json(claim_payload(&box_row, &domain)));
        }
        return Err(AppError::Conflict(format!(
            "{} is already taken on {}. Try another name — your address is the part before the @.",
            existing_address, domain
        )));
    }

    let days = trial::trial_days(&state.db, tenant_id).await?;

    // The trial window starts NOW and is stored on the mailbox: `access_mode = 'trial'` plus the
    // deadline the send path and the console both read. `NOW()` is the database clock (UTC), so the
    // window cannot drift with the app host's timezone.
    let box_row = sqlx::query_as::<_, PrivateEmailBox>(
        r#"
        INSERT INTO private_email_boxes
            (tenant_id, domain_id, user_id, local_part, email_address, access_mode,
             trial_started_at, trial_ends_at)
        VALUES ($1, $2, $3, $4, $5, 'trial', NOW(), NOW() + ($6 || ' days')::interval)
        RETURNING *
        "#,
    )
    .bind(tenant_id)
    .bind(domain_id)
    .bind(user_id)
    .bind(&local_part)
    .bind(&address)
    .bind(days.to_string())
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    tracing::info!(
        tenant = %tenant_id,
        address = %box_row.email_address,
        trial_days = days,
        "free mailbox claimed (full send+receive for the trial, then receive-only)"
    );

    Ok(Json(claim_payload(&box_row, &domain)))
}

/// GET /api/private-email/mailbox-status — the console's view of the trial and the fair-use meters.
///
/// Read-only and cheap; it is what lets the inbox say "Your free mailbox: 12 days left" and, after
/// the window closes, show the receive-only state WITH the upgrade path instead of a dead composer.
pub async fn mailbox_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(trial::mailbox_status(&state.db, tenant_id).await?))
}

fn claim_payload(box_row: &PrivateEmailBox, domain: &str) -> Value {
    let days_left: Option<i64> = box_row
        .trial_ends_at
        .map(|ends: DateTime<Utc>| ((ends - Utc::now()).num_seconds() + 86_399) / 86_400)
        .map(|d| d.max(0));
    json!({
        "mailbox": box_row,
        "address": box_row.email_address,
        "domain": domain,
        "access_mode": box_row.access_mode,
        "trial_started_at": box_row.trial_started_at,
        "trial_ends_at": box_row.trial_ends_at,
        "days_left": days_left,
    })
}

#[cfg(test)]
mod tests {
    use super::normalise_local_part;

    #[test]
    fn local_parts_are_made_safe_and_refused_when_unusable() {
        assert_eq!(normalise_local_part("  David G ").unwrap(), "david.g");
        assert_eq!(normalise_local_part("Info").unwrap(), "info");
        // Too short, reserved, and punctuation-only are all refused with a sentence, not a panic.
        assert!(normalise_local_part("ab").is_err());
        assert!(normalise_local_part("postmaster").is_err());
        assert!(normalise_local_part("***").is_err());
        // 32 is the cap; 33 is refused.
        assert!(normalise_local_part(&"a".repeat(32)).is_ok());
        assert!(normalise_local_part(&"a".repeat(33)).is_err());
    }
}
