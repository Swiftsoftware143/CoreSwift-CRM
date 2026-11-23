//! Email compliance — one gate in front of every class of outgoing mail.
//!
//! THE RULE (2026-11-19, David): platform mail, operator alerts, customer notifications and campaigns
//! are all mail this product sends on a customer's behalf, so all of them are held to the same
//! standard. There are three classes and the difference between them is what an opt-out is allowed
//! to stop:
//!
//! * `transactional` — receipts, password resets, security notices. Exempt from opt-out by law and
//!   must keep working for someone who unsubscribed from everything. NOT exempt from harm control: an
//!   address that hard-bounced or complained is never mailed again, in any class.
//! * `notification` — "your ticket was answered", digests, assignment alerts. Stoppable.
//! * `bulk` — campaigns and sequences. Stoppable, and carries the opt-out footer, the postal address
//!   and the one-click headers that Gmail/Yahoo require of bulk senders.
//!
//! The suppression list is PER ACCOUNT, never global: one customer's opt-out must not silence another
//! customer's mail to the same human.
//!
//! Rates are computed from `email_events` on read. Nothing stores a delivery rate — a stored rate is a
//! number that drifts from the events it claims to summarise.

pub mod webhook;

use axum::{
    extract::{Path, State},
    http::{header, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Extension, Json, Router,
};
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::AppState;

pub const CLASS_BULK: &str = "bulk";
pub const CLASS_NOTIFICATION: &str = "notification";
pub const CLASS_TRANSACTIONAL: &str = "transactional";

/// Where the customer-facing pages live. Same host as the rest of the app's mailed links.
const APP_URL: &str = "https://app.coreswiftcrm.com";

/// Addresses are compared lowercased and trimmed: `David@X.com` and `david@x.com` are one human, and
/// a provider event carries whatever case the remote server said.
pub fn normalise(address: &str) -> String {
    address.trim().to_lowercase()
}

fn secret() -> Vec<u8> {
    std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "coreswift-email-compliance".to_string())
        .into_bytes()
}

fn sign(payload: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as Mac>::new_from_slice(&secret()).expect("hmac accepts any key len");
    mac.update(payload.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// A tamper-proof opt-out token: tenant + address + class, signed. No expiry — an unsubscribe link in
/// a two-year-old message must still work, and there is no state to lose in a table.
pub fn token_for(tenant_id: Uuid, address: &str, mail_class: &str) -> String {
    let addr = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(normalise(address));
    let payload = format!("{tenant_id}.{addr}.{mail_class}");
    let sig = sign(&payload);
    format!("{payload}.{sig}")
}

pub fn verify_token(token: &str) -> Option<(Uuid, String, String)> {
    let mut parts = token.rsplitn(2, '.');
    let sig = parts.next()?;
    let payload = parts.next()?;
    if sign(payload) != sig {
        return None;
    }
    let mut it = payload.split('.');
    let tenant_id: Uuid = it.next()?.parse().ok()?;
    let address = String::from_utf8(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(it.next()?)
            .ok()?,
    )
    .ok()?;
    let mail_class = it.next()?.to_string();
    Some((tenant_id, address, mail_class))
}

pub fn unsubscribe_url(tenant_id: Uuid, address: &str, mail_class: &str) -> String {
    format!(
        "{APP_URL}/api/public/unsubscribe/{}",
        token_for(tenant_id, address, mail_class)
    )
}

/// Which stored suppression rows stop a message of this class.
fn blocking_classes(mail_class: &str) -> &'static [&'static str] {
    match mail_class {
        CLASS_TRANSACTIONAL => &["transactional"],
        CLASS_NOTIFICATION => &["all", "notification"],
        _ => &["all", "bulk"],
    }
}

/// Somebody asked this account to mail this address less. Returns the reason when that covers the
/// class we were about to send.
pub async fn suppression(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    mail_class: &str,
) -> Option<String> {
    let classes: Vec<String> = blocking_classes(mail_class)
        .iter()
        .map(|c| c.to_string())
        .collect();
    sqlx::query_scalar::<_, String>(
        "SELECT reason FROM email_suppressions
          WHERE tenant_id = $1 AND address = $2 AND mail_class = ANY($3)
          ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(&classes)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
}

/// Record an opt-out / bad address. Idempotent: pressing the button twice is one suppression.
pub async fn suppress(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    reason: &str,
    mail_class: &str,
    source: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_suppressions (tenant_id, address, mail_class, reason, source)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (tenant_id, address, mail_class)
         DO UPDATE SET reason = EXCLUDED.reason, source = EXCLUDED.source, updated_at = now()",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(mail_class)
    .bind(reason)
    .bind(source)
    .execute(db)
    .await?;

    // Only a human's own choice is consent history; a bounce is not an opinion.
    if mail_class != CLASS_TRANSACTIONAL {
        let _ = sqlx::query(
            "INSERT INTO email_consent (tenant_id, address, mail_class, state, how)
             VALUES ($1, $2, $3, 'revoked', $4)",
        )
        .bind(tenant_id)
        .bind(normalise(address))
        .bind(if mail_class == "all" {
            CLASS_BULK
        } else {
            mail_class
        })
        .bind(source)
        .execute(db)
        .await;
    }
    Ok(())
}

/// One row per send attempt, written BEFORE the provider is asked, so a later provider event can be
/// attributed to this account and this class.
pub async fn log_attempt(
    db: &PgPool,
    tenant_id: Uuid,
    outbound_message_id: Option<Uuid>,
    mail_class: &str,
    address: &str,
    subject: Option<&str>,
    provider: Option<&str>,
    status: &str,
    detail: Option<&str>,
) {
    let _ = sqlx::query(
        "INSERT INTO email_message_log
            (tenant_id, outbound_message_id, mail_class, address, subject, provider, status, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant_id)
    .bind(outbound_message_id)
    .bind(if mail_class == "all" {
        CLASS_BULK
    } else {
        mail_class
    })
    .bind(normalise(address))
    .bind(subject)
    .bind(provider)
    .bind(status)
    .bind(detail)
    .execute(db)
    .await;
}

/// Stamp the provider's own message id onto the newest attempt for this address, so the event stream
/// can join back to the row. Best-effort: a missing id costs attribution, never the send.
pub async fn attach_provider_id(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    provider_message_id: &str,
) {
    let _ = sqlx::query(
        "UPDATE email_message_log SET provider_message_id = $3
          WHERE id = (SELECT id FROM email_message_log
                       WHERE tenant_id = $1 AND address = $2 AND provider_message_id IS NULL
                       ORDER BY created_at DESC LIMIT 1)",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(provider_message_id)
    .execute(db)
    .await;
}

pub async fn record_event(
    db: &PgPool,
    tenant_id: Uuid,
    provider: &str,
    provider_message_id: Option<&str>,
    address: Option<&str>,
    event: &str,
    severity: Option<&str>,
    reason: Option<&str>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    raw: Option<Value>,
) {
    // Stored WITHOUT the angle brackets the provider wraps around a message id. The send log's own id
    // came back from Mailgun's send response WITH them, so the two columns would otherwise describe the
    // same message in two spellings and every report that joins them would silently find nothing.
    let normalised_id = provider_message_id.map(|id| {
        id.trim()
            .trim_start_matches('<')
            .trim_end_matches('>')
            .to_string()
    });
    let _ = sqlx::query(
        "INSERT INTO email_events
            (tenant_id, provider, provider_message_id, address, event, severity, reason, occurred_at, raw)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT DO NOTHING",
    )
    .bind(tenant_id)
    .bind(provider)
    .bind(normalised_id.clone())
    .bind(address.map(normalise))
    .bind(event)
    .bind(severity)
    .bind(reason)
    .bind(occurred_at)
    .bind(raw)
    .execute(db)
    .await;
}

/// Delivery evidence for one account, computed from the event ledger over a window.
pub async fn rates(db: &PgPool, tenant_id: Uuid, days: i64) -> Value {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(
        "SELECT
            count(*) FILTER (WHERE event = 'delivered'),
            count(*) FILTER (WHERE event = 'bounced'),
            count(*) FILTER (WHERE event = 'complained'),
            count(*) FILTER (WHERE event = 'dropped'),
            count(*) FILTER (WHERE event = 'failed'),
            (SELECT count(*) FROM email_message_log
              WHERE tenant_id = $1 AND status = 'sent'
                AND created_at > now() - ($2 || ' days')::interval)
           FROM email_events
          WHERE tenant_id = $1 AND occurred_at > now() - ($2 || ' days')::interval",
    )
    .bind(tenant_id)
    .bind(days.to_string())
    .fetch_one(db)
    .await
    .unwrap_or((0, 0, 0, 0, 0, 0));

    let (delivered, bounced, complained, dropped, failed, sent) = row;
    let denom = if sent > 0 { sent as f64 } else { 1.0 };
    let pct = |n: i64| ((n as f64 / denom) * 1000.0).round() / 10.0;

    json!({
        "window_days": days,
        "sent": sent,
        "delivered": delivered,
        "bounced": bounced,
        "complained": complained,
        "dropped": dropped,
        "failed": failed,
        "delivery_rate": pct(delivered),
        "bounce_rate": pct(bounced),
        // Gmail/Yahoo bulk-sender rules: stay under 0.3% or the mailbox provider starts refusing.
        "complaint_rate": pct(complained),
        "complaint_rate_ceiling": 0.3,
        "no_events_yet": delivered + bounced + complained + dropped + failed == 0,
    })
}

/// The gate's verdict for one message, computed once and applied by the transport.
pub struct Gate {
    /// `Some(reason)` = do not send: this account has been asked not to mail this address in this
    /// class, or the address is known bad.
    pub blocked: Option<String>,
    /// Provider-level opt-out headers (`h:List-Unsubscribe`, `h:List-Unsubscribe-Post`), empty for
    /// transactional mail, which is legally exempt and must not invite an opt-out it cannot honour.
    pub headers: Vec<(String, String)>,
    pub footer_html: String,
    pub footer_text: String,
    /// The account's settings blob, so the caller can read its own allowance without a second query.
    pub settings: Value,
}

/// Where the account's postal address lives in settings (CAN-SPAM requires one in bulk mail). Kept in
/// the jsonb settings the account already owns — no migration, and the console can set it.
fn postal_address(settings: &Value) -> Option<String> {
    settings
        .get("branding")
        .and_then(|b| b.get("postal_address"))
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            settings
                .get("postal_address")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// Build the gate for one message. Never fails: a database that cannot answer must not silently turn
/// an opt-out off, so a failed suppression read is treated as "send" only for transactional mail and
/// as "refuse" for the two stoppable classes — the safe side of the only decision that matters.
pub async fn gate(db: &PgPool, tenant_id: Uuid, address: &str, mail_class: &str) -> Gate {
    let addr = normalise(address);
    let mut blocked = None;
    let classes: Vec<String> = blocking_classes(mail_class)
        .iter()
        .map(|c| c.to_string())
        .collect();

    match sqlx::query_scalar::<_, String>(
        "SELECT reason FROM email_suppressions
          WHERE tenant_id = $1 AND address = $2 AND mail_class = ANY($3)
          ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&addr)
    .bind(&classes)
    .fetch_optional(db)
    .await
    {
        Ok(found) => blocked = found,
        Err(e) => {
            tracing::error!("suppression read failed for tenant {tenant_id}: {e}");
            if mail_class != CLASS_TRANSACTIONAL {
                blocked = Some("suppression list unavailable".to_string());
            }
        }
    }

    // Account identity for the footer, and the postal address legal bulk mail must carry.
    let row = sqlx::query_as::<_, (Option<String>, Value)>(
        "SELECT name, coalesce(settings, '{}'::jsonb) FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    let account = row
        .as_ref()
        .and_then(|(name, _)| name.clone())
        .unwrap_or_else(|| "CoreSwift CRM".to_string());
    let postal = row
        .as_ref()
        .and_then(|(_, settings)| postal_address(settings));

    let mut headers = Vec::new();
    let mut footer_html = String::new();
    let mut footer_text = String::new();

    if mail_class != CLASS_TRANSACTIONAL {
        let url = unsubscribe_url(tenant_id, &addr, mail_class);
        headers.push(("h:List-Unsubscribe".to_string(), format!("<{url}>")));
        // RFC 8058: lets Gmail/Yahoo render their own unsubscribe button and post it with no human.
        headers.push((
            "h:List-Unsubscribe-Post".to_string(),
            "List-Unsubscribe=One-Click".to_string(),
        ));
        let where_ = postal.as_deref().unwrap_or("");
        footer_text = format!(
            "\n\n--\n{account}{sep}{where_}\nStop these emails: {url}\n",
            sep = if where_.is_empty() { "" } else { " · " }
        );
        footer_html = format!(
            "<hr style=\"border:0;border-top:1px solid #e5e7eb;margin:28px 0 12px\">\
             <p style=\"font:13px/1.5 system-ui,-apple-system,sans-serif;color:#6b7280;margin:0\">\
             {account}{sep}{where_}<br>\
             <a href=\"{url}\" style=\"color:#4f46e5\">Stop these emails</a></p>",
            sep = if where_.is_empty() { "" } else { " · " }
        );
    }

    Gate {
        blocked,
        headers,
        footer_html,
        footer_text,
        settings: row
            .as_ref()
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| json!({})),
    }
}

/// Sending isolation (2026-11-19). Measured before this: every account without its own domain sends
/// on the platform's shared identity, so one careless sender stains the name that carries everybody
/// else's receipts and replies. The fix is not to refuse them — most accounts have no domain yet and
/// their mail is legitimate — it is to make the shared identity a SMALL allowance and the account's
/// own identity a large one, with the reason stated in the refusal the account can read.
pub struct Quota {
    /// Ceilings, overridable per account in `tenants.settings->'compliance'`.
    pub daily: i64,
    pub hourly: i64,
    pub shared_identity: bool,
}

fn setting_i64(settings: &Value, key: &str) -> Option<i64> {
    settings
        .get("compliance")
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_i64())
}

/// The allowance for this account and this class. `shared` means the message would leave on the
/// platform's own identity, which is a shared reputation and therefore a much smaller allowance.
pub fn quota_for(mail_class: &str, shared: bool, settings: &Value) -> Option<Quota> {
    if mail_class == CLASS_TRANSACTIONAL {
        return None; // receipts and resets are never capped — a cap there is an outage
    }
    let (d_key, h_key) = if shared {
        ("shared_daily_cap", "shared_hourly_cap")
    } else {
        ("bulk_daily_cap", "bulk_hourly_cap")
    };
    let defaults = if shared { (100, 30) } else { (2000, 400) };
    let daily = setting_i64(settings, d_key).unwrap_or(defaults.0);
    let hourly = setting_i64(settings, h_key).unwrap_or(defaults.1);
    Some(Quota {
        daily,
        hourly,
        shared_identity: shared,
    })
}

/// How much of that allowance is already spent. Counted from the send log, never from a counter column
/// that could drift from the rows it claims to summarise.
pub async fn spent(db: &PgPool, tenant_id: Uuid, mail_class: &str) -> (i64, i64) {
    let row = sqlx::query_as::<_, (i64, i64)>(
        "SELECT
            count(*) FILTER (WHERE created_at > now() - interval '24 hours'),
            count(*) FILTER (WHERE created_at > now() - interval '1 hour')
           FROM email_message_log
          WHERE tenant_id = $1 AND mail_class = $2 AND status = 'sent'",
    )
    .bind(tenant_id)
    .bind(if mail_class == "all" {
        CLASS_BULK
    } else {
        mail_class
    })
    .fetch_one(db)
    .await
    .unwrap_or((0, 0));
    row
}

/// Reputation watchdog: an account whose mail is drawing complaints or hard bounces has its bulk lane
/// paused BEFORE the shared pool is damaged. Rates are computed from the event ledger; an account with
/// no events yet is never paused (silence is not evidence).
pub async fn watchdog(db: &PgPool, tenant_id: Uuid) -> Option<String> {
    let row = sqlx::query_as::<_, (i64, i64, i64, i64)>(
        "SELECT
            count(*) FILTER (WHERE ee.event = 'delivered'),
            count(*) FILTER (WHERE ee.event = 'bounced'),
            count(*) FILTER (WHERE ee.event = 'complained'),
            (SELECT count(*) FROM email_message_log ml
              WHERE ml.tenant_id = $1 AND ml.status = 'sent'
                AND ml.created_at > now() - interval '30 days')
           FROM email_events ee
          WHERE ee.tenant_id = $1 AND ee.occurred_at > now() - interval '30 days'",
    )
    .bind(tenant_id)
    .fetch_one(db)
    .await
    .unwrap_or((0, 0, 0, 0));

    let (delivered, bounced, complained, sent) = row;
    let denom = if sent > 0 { sent as f64 } else { 0.0 };
    if denom == 0.0 {
        return None;
    }
    let bounce_rate = (bounced as f64 / denom) * 100.0;
    let complaint_rate = (complained as f64 / denom) * 100.0;
    // 5% bounces is a broken list, 0.3% complaints is a mailbox provider's hard ceiling.
    if complaint_rate >= 0.3 {
        return Some(format!(
            "bulk sending paused: {complaint_rate:.2}% of this account's mail is being reported as \
             spam (the ceiling is 0.30%). Clear the list and reply to support to resume."
        ));
    }
    if bounce_rate >= 5.0 && delivered + bounced >= 20 {
        return Some(format!(
            "bulk sending paused: {bounce_rate:.1}% of this account's mail bounced — that is a list \
             problem, not a sending problem. Clean the addresses and reply to support to resume."
        ));
    }
    None
}

// ── public surface: the opt-out itself ────────────────────────────────────────────────────────────

/// The words on the opt-out page. Every field is editable per account
/// (`tenants.settings->'compliance'->'unsubscribe_page'`) so a customer's opt-out page speaks in their
/// own voice; anything they have not set falls back to the platform's wording. Never empty: an opt-out
/// page with no explanation is worse than a plain one.
pub struct PageCopy {
    pub title: String,
    pub intro: String,
    pub all_label: String,
    pub button_label: String,
    pub done_title: String,
    pub done_body: String,
}

fn copy_field(settings: &Value, key: &str) -> Option<String> {
    settings
        .get("compliance")
        .and_then(|c| c.get("unsubscribe_page"))
        .and_then(|u| u.get(key))
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn page_copy(settings: &Value, address: &str) -> PageCopy {
    let defaults = |k: &str| match k {
        "title" => format!("Mail preferences for {address}"),
        "intro" => "Choose what you would like to stop receiving. Messages you need — receipts, \
                    password resets, security notices — keep working."
            .to_string(),
        "all_label" => "Unsubscribe from everything".to_string(),
        "button_label" => "Save my choices".to_string(),
        "done_title" => "You're unsubscribed".to_string(),
        "done_body" => "That change is saved. Nothing else to do.".to_string(),
        _ => String::new(),
    };
    PageCopy {
        title: copy_field(settings, "title").unwrap_or_else(|| defaults("title")),
        intro: copy_field(settings, "intro").unwrap_or_else(|| defaults("intro")),
        all_label: copy_field(settings, "all_label").unwrap_or_else(|| defaults("all_label")),
        button_label: copy_field(settings, "button_label")
            .unwrap_or_else(|| defaults("button_label")),
        done_title: copy_field(settings, "done_title").unwrap_or_else(|| defaults("done_title")),
        done_body: copy_field(settings, "done_body").unwrap_or_else(|| defaults("done_body")),
    }
}

async fn tenant_settings(db: &PgPool, tenant_id: Uuid) -> Value {
    sqlx::query_scalar::<_, Value>(
        "SELECT coalesce(settings, '{}'::jsonb) FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| json!({}))
}

/// The campaigns THIS address is actually in, read from the account's own campaign rows. Names come
/// from `email_campaigns.name`, so the page can never show a campaign the customer renamed or deleted,
/// and one account's campaigns can never appear on another account's page.
pub async fn campaigns_for(db: &PgPool, tenant_id: Uuid, address: &str) -> Vec<(Uuid, String)> {
    sqlx::query_as::<_, (Uuid, String)>(
        "SELECT c.id, c.name
           FROM email_campaigns c
           JOIN email_campaign_enrollments e ON e.campaign_id = c.id
           JOIN contacts ct ON ct.id = e.entity_id AND ct.tenant_id = c.tenant_id
          WHERE c.tenant_id = $1
            AND e.entity_type = 'contact'
            AND lower(trim(ct.email)) = $2
          GROUP BY c.id, c.name
          ORDER BY c.name",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .fetch_all(db)
    .await
    .unwrap_or_default()
}

/// Has this address opted out of this campaign? The single question a campaign sender must ask.
pub async fn campaign_opted_out(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    campaign_id: Uuid,
) -> bool {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM email_campaign_optouts
          WHERE tenant_id = $1 AND address = $2 AND campaign_id = $3",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(campaign_id)
    .fetch_one(db)
    .await
    .unwrap_or(0)
        > 0
}

async fn opt_into_campaign_off(
    db: &PgPool,
    tenant_id: Uuid,
    address: &str,
    campaign_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO email_campaign_optouts (tenant_id, address, campaign_id, source)
         VALUES ($1, $2, $3, 'unsubscribe_page')
         ON CONFLICT (tenant_id, address, campaign_id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(normalise(address))
    .bind(campaign_id)
    .execute(db)
    .await?;
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A complete document, not a fragment: this is the one page a stranger sees from this product, and
/// an unstyled fragment with no `<html>` element renders differently in every mailbox browser there is.
fn page_shell(title: &str, inner: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <meta name=\"robots\" content=\"noindex,nofollow\">\
         <title>{}</title></head>\
         <body style=\"margin:0;background:#fff\">\
         <div style=\"font:16px/1.6 system-ui,-apple-system,Segoe UI,sans-serif;max-width:560px;\
         margin:10vh auto;padding:0 20px;color:#111\">{}</div></body></html>",
        html_escape(title),
        inner
    )
}

/// `GET` — the page a person actually lands on. No login, no session: the signed token in the link is
/// the whole credential, and it names one account and one address.
async fn show_unsubscribe(State(state): State<AppState>, Path(token): Path<String>) -> Response {
    let Some((tenant_id, address, mail_class)) = verify_token(&token) else {
        return (
            StatusCode::GONE,
            Html(page_shell(
                "Link expired",
                "<h1 style=\"font-size:1.2rem\">This link is not valid</h1><p>Reply to any message \
                 from this sender and ask them to remove you.</p>",
            )),
        )
            .into_response();
    };

    let settings = tenant_settings(&state.db, tenant_id).await;
    let copy = page_copy(&settings, &normalise(&address));
    let campaigns = campaigns_for(&state.db, tenant_id, &address).await;

    let mut rows = String::new();
    for (id, name) in &campaigns {
        rows.push_str(&format!(
            "<label style=\"display:flex;gap:10px;align-items:flex-start;padding:10px 0;\
             border-bottom:1px solid #eee\"><input type=checkbox name=\"campaign\" value=\"{id}\" \
             style=\"margin-top:4px\"><span>{name}</span></label>",
            id = id,
            name = html_escape(name)
        ));
    }
    // The campaign list is only shown when there IS one; a person who is in no campaign still gets the
    // "everything" choice, which is the only thing that can be true for them.
    let campaign_block = if rows.is_empty() {
        String::new()
    } else {
        format!(
            "<p style=\"color:#555;margin:0 0 4px\">Campaigns you are in:</p>{}<div style=\"height:12px\">\
             </div>",
            rows
        )
    };
    let everything_label = html_escape(&copy.all_label);
    let button = html_escape(&copy.button_label);
    let _ = mail_class; // the class the link was mailed for; the page offers the widest honest choice

    let body = format!(
        "<h1 style=\"font-size:1.25rem;margin:0 0 8px\">{title}</h1>\
         <p style=\"color:#555;margin:0 0 20px\">{intro}</p>\
         <form method=post action=\"/api/public/unsubscribe/{token}\">{campaign_block}\
         <label style=\"display:flex;gap:10px;align-items:flex-start;padding:12px 0\">\
         <input type=checkbox name=\"everything\" value=\"1\" style=\"margin-top:4px\">\
         <span><strong>{everything_label}</strong></span></label>\
         <button style=\"font:inherit;padding:12px 22px;border:0;border-radius:8px;background:#4f46e5;\
         color:#fff;cursor:pointer;margin-top:8px\">{button}</button></form>",
        title = html_escape(&copy.title),
        intro = html_escape(&copy.intro),
        token = token,
    );

    (
        StatusCode::OK,
        [(axum::http::header::CACHE_CONTROL, "no-store")],
        Html(page_shell(&copy.title, &body)),
    )
        .into_response()
}

/// `POST` — both arms land here.
///
/// * A browser submits the form (checkbox choices), so the body is urlencoded.
/// * A mailbox provider's one-click button posts RFC 8058's `List-Unsubscribe=One-Click` with no human,
///   and some providers post JSON. Both mean "stop everything", which is the only thing a provider can
///   consent to on the person's behalf.
async fn one_click(
    State(state): State<AppState>,
    Path(token): Path<String>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some((tenant_id, address, mail_class)) = verify_token(&token) else {
        return (StatusCode::GONE, Json(json!({ "error": "invalid link" }))).into_response();
    };

    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let mut everything = false;
    let mut campaigns: Vec<Uuid> = Vec::new();
    let is_form = content_type.contains("application/x-www-form-urlencoded");

    if is_form {
        if let Ok(fields) = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&body) {
            for (key, value) in fields {
                match key.as_str() {
                    "everything" => everything = true,
                    "campaign" => {
                        if let Ok(id) = value.parse::<Uuid>() {
                            campaigns.push(id);
                        }
                    }
                    _ => {}
                }
            }
        }
    } else {
        // JSON or the provider's one-click post both mean the widest choice available.
        everything = true;
        if let Ok(parsed) = serde_json::from_slice::<Value>(&body) {
            if let Some(list) = parsed.get("campaigns").and_then(|v| v.as_array()) {
                everything = false;
                for v in list {
                    if let Some(id) = v.as_str().and_then(|s| s.parse::<Uuid>().ok()) {
                        campaigns.push(id);
                    }
                }
            }
            if parsed.get("campaigns").is_none() {
                everything = true;
            }
        }
    }

    if !everything && campaigns.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "nothing selected" })),
        )
            .into_response();
    }

    if everything {
        let stored_class = if mail_class == CLASS_TRANSACTIONAL {
            "all"
        } else {
            mail_class.as_str()
        };
        if let Err(e) = suppress(
            &state.db,
            tenant_id,
            &address,
            "unsubscribed",
            stored_class,
            "one_click",
        )
        .await
        {
            tracing::error!("unsubscribe failed for tenant {tenant_id}: {e}");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not record the opt-out" })),
            )
                .into_response();
        }
    }

    // A campaign choice is always written for THIS account and THIS address only.
    for campaign_id in &campaigns {
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM email_campaigns WHERE id = $1 AND tenant_id = $2")
                .bind(campaign_id)
                .bind(tenant_id)
                .fetch_optional(&state.db)
                .await
                .ok()
                .flatten();
        if owned.is_none() {
            // A campaign id that this account does not own is ignored, never honoured: the page must
            // not become a way to write into another tenant's rows.
            continue;
        }
        if let Err(e) = opt_into_campaign_off(&state.db, tenant_id, &address, *campaign_id).await {
            tracing::error!("campaign opt-out failed for tenant {tenant_id}: {e}");
        }
    }

    if !everything && campaigns.is_empty() {
        return (StatusCode::OK, Json(json!({ "unsubscribed": false }))).into_response();
    }

    if is_form {
        let settings = tenant_settings(&state.db, tenant_id).await;
        let copy = page_copy(&settings, &normalise(&address));
        let inner = format!(
            "<h1 style=\"font-size:1.2rem\">{}</h1><p style=\"color:#555\">{}</p>",
            html_escape(&copy.done_title),
            html_escape(&copy.done_body)
        );
        return (
            StatusCode::OK,
            [(axum::http::header::CACHE_CONTROL, "no-store")],
            Html(page_shell(&copy.done_title, &inner)),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(json!({
            "unsubscribed": everything,
            "campaigns_stopped": campaigns.len(),
            "address": normalise(&address)
        })),
    )
        .into_response()
}

// ── authenticated surface: what the operator sees ─────────────────────────────────────────────────

/// The account the caller is signed into — the same claim every other authenticated handler reads.
fn tenant_of(c: &Claims) -> Option<Uuid> {
    c.aid.parse().ok()
}

async fn overview(State(state): State<AppState>, Extension(c): Extension<Claims>) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let days: i64 = 30;
    Json(json!({
        "rates": rates(&state.db, tenant_id, days).await,
        "suppressed": sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM email_suppressions WHERE tenant_id = $1")
            .bind(tenant_id).fetch_one(&state.db).await.unwrap_or(0),
        "suppressed_last_30d": sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM email_suppressions WHERE tenant_id = $1 AND created_at > now() - interval '30 days'")
            .bind(tenant_id).fetch_one(&state.db).await.unwrap_or(0),
    }))
    .into_response()
}

/// The vocabulary the database enforces (`email_suppressions_reason_check`, migration 181). The door
/// answers 400 with this list for anything else, because the CHECK would otherwise turn a caller's
/// typo into a 500 that names no cause. The list is deliberately NOT widened: these five are how a
/// hard bounce or a complaint is told apart from a preference, and a bounce is never lifted as if it
/// were an opt-out.
const SUPPRESSION_REASONS: [&str; 5] = [
    "unsubscribed",
    "hard_bounce",
    "complaint",
    "manual",
    "provider",
];

/// `email_suppressions_mail_class_check` — the same rule for the scope a suppression covers.
const SUPPRESSION_MAIL_CLASSES: [&str; 4] = ["all", "bulk", "notification", "transactional"];

#[derive(Deserialize)]
struct ManualSuppression {
    address: String,
    #[serde(default)]
    mail_class: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

async fn list_suppressions(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            String,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT id, address, mail_class, reason, source, created_at FROM email_suppressions
          WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT 1000",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, address, mail_class, reason, source, created_at)| {
            json!({
                "id": id, "address": address, "mail_class": mail_class, "reason": reason,
                "source": source, "created_at": created_at
            })
        })
        .collect();
    Json(json!({ "suppressions": items })).into_response()
}

async fn add_suppression(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<ManualSuppression>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let mail_class = body
        .mail_class
        .unwrap_or_else(|| "all".to_string())
        .trim()
        .to_lowercase();
    let reason = body
        .reason
        .unwrap_or_else(|| "manual".to_string())
        .trim()
        .to_lowercase();
    if !body.address.contains('@') {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "not an address" })),
        )
            .into_response();
    }
    // The database's own vocabulary, enforced here so a caller's typo is a named 400 instead of a
    // 500 from the CHECK constraint. The CHECK stays as the backstop for every other writer.
    if !SUPPRESSION_REASONS.contains(&reason.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown reason", "allowed": SUPPRESSION_REASONS })),
        )
            .into_response();
    }
    if !SUPPRESSION_MAIL_CLASSES.contains(&mail_class.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unknown mail class", "allowed": SUPPRESSION_MAIL_CLASSES })),
        )
            .into_response();
    }
    match suppress(
        &state.db,
        tenant_id,
        &body.address,
        &reason,
        &mail_class,
        "console",
    )
    .await
    {
        Ok(()) => (StatusCode::OK, Json(json!({ "suppressed": true }))).into_response(),
        Err(e) => {
            tracing::error!("manual suppression failed: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "not recorded" })),
            )
                .into_response()
        }
    }
}

async fn remove_suppression(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    // Tenant-scoped on the write itself: one account can never lift another's suppression.
    // Rows affected is the whole verdict: `remove` of an id that belongs to another tenant (or that
    // does not exist) is NOT a no-op success — it answers 404, so the console can tell "lifted" from
    // "never yours", and the tenancy boundary is visible instead of silently unaddressable.
    let done = sqlx::query("DELETE FROM email_suppressions WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await
        .map(|r| r.rows_affected())
        .unwrap_or(0);
    if done == 0 {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "no such suppression in this account" })),
        )
            .into_response();
    }
    Json(json!({ "removed": done })).into_response()
}

/// A plain addresses CSV — the download a customer takes to an address-verification service. One
/// address per line, no other columns, so it pastes into any checker.
async fn addresses_csv(State(state): State<AppState>, Extension(c): Extension<Claims>) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let rows = sqlx::query_scalar::<_, String>(
        "SELECT DISTINCT lower(trim(email)) FROM contacts
          WHERE tenant_id = $1 AND email IS NOT NULL AND trim(email) <> ''
          ORDER BY 1",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut out = String::from("email\n");
    for a in &rows {
        out.push_str(a);
        out.push('\n');
    }
    csv_response("contact-addresses.csv", out)
}

/// The suppression list as a CSV, for a customer's own records.
async fn suppressions_csv(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let rows = sqlx::query_as::<
        _,
        (
            String,
            String,
            String,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
        ),
    >(
        "SELECT address, mail_class, reason, source, created_at FROM email_suppressions
          WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();
    let mut out = String::from("address,scope,reason,source,recorded_at\n");
    for (address, mail_class, reason, source, created_at) in rows {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            address,
            mail_class,
            reason,
            source.unwrap_or_default(),
            created_at.to_rfc3339()
        ));
    }
    csv_response("unsubscribes.csv", out)
}

#[derive(Deserialize)]
struct PageCopyIn {
    title: Option<String>,
    intro: Option<String>,
    all_label: Option<String>,
    button_label: Option<String>,
    done_title: Option<String>,
    done_body: Option<String>,
}

/// "This tenant's owner, or a platform operator" — through the ONE helper that already knows the
/// platform carries three role vocabularies (`owner`, `account_owner`/`admin`, `agency_admin`).
///
/// Measured live 2026-11-19: comparing against `account_owner | admin` alone answered 403 to a REAL
/// account owner whose row says `owner`, i.e. it locked out every customer on the platform while
/// reading correctly. The same defect had already been found once in `native_apps::connect_app`.
fn is_manager(c: &Claims) -> bool {
    crate::auth::middleware::is_account_admin(&c.role)
}

/// `GET /api/mail-compliance/unsubscribe-page` — what the opt-out page will say, plus how many
/// campaigns the account has (the page lists only the ones a person is actually in).
async fn get_unsubscribe_page(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    let settings = tenant_settings(&state.db, tenant_id).await;
    let copy = page_copy(&settings, "<the address>");
    let row = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    >(
        "SELECT
            settings->'compliance'->'unsubscribe_page'->>'title',
            settings->'compliance'->'unsubscribe_page'->>'intro',
            settings->'compliance'->'unsubscribe_page'->>'all_label',
            settings->'compliance'->'unsubscribe_page'->>'button_label',
            settings->'compliance'->'unsubscribe_page'->>'done_title',
            settings->'compliance'->'unsubscribe_page'->>'done_body'
           FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten()
    .unwrap_or((None, None, None, None, None, None));

    Json(json!({
        "effective": {
            "title": copy.title, "intro": copy.intro, "all_label": copy.all_label,
            "button_label": copy.button_label, "done_title": copy.done_title,
            "done_body": copy.done_body,
        },
        "overrides": {
            "title": row.0, "intro": row.1, "all_label": row.2,
            "button_label": row.3, "done_title": row.4, "done_body": row.5,
        },
        "can_edit": is_manager(&c),
        "campaigns": sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM email_campaigns WHERE tenant_id = $1",)
            .bind(tenant_id).fetch_one(&state.db).await.unwrap_or(0),
    }))
    .into_response()
}

/// `PUT /api/mail-compliance/unsubscribe-page` — the account's own words on its own opt-out page.
/// Emptying a field restores the platform default rather than leaving a blank page.
async fn put_unsubscribe_page(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<PageCopyIn>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    if !is_manager(&c) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "only the account owner or an admin can change this page" })),
        )
            .into_response();
    }
    // Empty means "use the platform wording": storing an empty string would render a page with no
    // explanation, which is a worse opt-out page than the one it replaced.
    let clean = |v: Option<String>, max: usize| -> Option<String> {
        v.map(|s| s.trim().chars().take(max).collect::<String>())
            .filter(|s| !s.is_empty())
    };
    let patch = json!({
        "title": clean(body.title, 120),
        "intro": clean(body.intro, 600),
        "all_label": clean(body.all_label, 120),
        "button_label": clean(body.button_label, 60),
        "done_title": clean(body.done_title, 120),
        "done_body": clean(body.done_body, 600),
    });
    // Written field by field: a null clears that override and the default comes back.
    // jsonb_set only creates the LAST key of a path: a path whose INTERMEDIATE key does not exist yet
    // returns the document unchanged, silently. Measured live 2026-11-19 — the save answered 200, the
    // UPDATE reported one row, and the row was byte-identical, because `compliance` did not exist yet
    // under `settings`. Writing the top-level key in ONE step is therefore the only correct form here
    // (the same trap is documented in src/account/branding.rs).
    let result = sqlx::query(
        "UPDATE tenants
            SET settings = jsonb_set(
                  coalesce(settings, '{}'::jsonb),
                  '{compliance}',
                  coalesce(settings->'compliance', '{}'::jsonb)
                    || jsonb_build_object(
                         'unsubscribe_page',
                         coalesce(settings->'compliance'->'unsubscribe_page', '{}'::jsonb) || $2::jsonb
                       ),
                  true)
          WHERE id = $1",
    )
    .bind(tenant_id)
    .bind(patch)
    .execute(&state.db)
    .await;
    match result {
        Ok(_) => {
            // A cleared field must actually disappear, so the default returns. jsonb `||` keeps nulls,
            // and a stored null is not "unset" for the reader above — strip them in a second pass.
            let _ = sqlx::query(
                "UPDATE tenants SET settings = jsonb_strip_nulls(settings) WHERE id = $1",
            )
            .bind(tenant_id)
            .execute(&state.db)
            .await;
            let settings = tenant_settings(&state.db, tenant_id).await;
            let copy = page_copy(&settings, "<the address>");
            Json(json!({
                "saved": true,
                "effective": {
                    "title": copy.title, "intro": copy.intro, "all_label": copy.all_label,
                    "button_label": copy.button_label, "done_title": copy.done_title,
                    "done_body": copy.done_body,
                }
            }))
            .into_response()
        }
        Err(e) => {
            tracing::error!("unsubscribe page copy save failed for tenant {tenant_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "not saved" })),
            )
                .into_response()
        }
    }
}

#[derive(Deserialize)]
struct LanesIn {
    automation_host: Option<String>,
    calendar_host: Option<String>,
}

/// A hostname the account may claim for a lane: a plain DNS name, no scheme, no path, no port.
fn clean_host(v: Option<String>) -> Option<String> {
    let h = v?.trim().to_lowercase();
    if h.is_empty() {
        return None;
    }
    let ok = h.len() <= 253
        && h.contains('.')
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && !h.starts_with('.')
        && !h.ends_with('.')
        && !h.starts_with('-');
    if ok {
        Some(h)
    } else {
        None
    }
}

/// Is this host a subdomain of a domain the account has actually claimed? A lane runs on the
/// customer's OWN name or not at all — otherwise the screen would let one account advertise an address
/// on somebody else's domain.
fn under_claimed_domain(host: &str, claimed: &[String]) -> bool {
    claimed.iter().any(|d| {
        let d = d.trim().to_lowercase();
        !d.is_empty() && (host == d || host.ends_with(&format!(".{d}")))
    })
}

async fn resolves(host: &str) -> bool {
    match tokio::net::lookup_host((host, 443)).await {
        Ok(mut addrs) => addrs.next().is_some(),
        Err(_) => false,
    }
}

/// `GET /api/mail-compliance/domains` — one domain, three lanes (David, 2026-11-18).
///
/// The INBOX lane is the domain the account already verified for mail; the AUTOMATION lane is a
/// subdomain of it that carries campaigns so a bad blast cannot damage the name that carries replies;
/// the CALENDAR lane is the host a public booking page is advertised on. Each lane answers for itself:
/// a lane is offered only when its OWN records verify.
async fn get_domains(State(state): State<AppState>, Extension(c): Extension<Claims>) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };

    let rows = sqlx::query_as::<_, (String, Option<String>, bool)>(
        "SELECT domain, inbound_provider, verified FROM private_email_domains
          WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let settings = tenant_settings(&state.db, tenant_id).await;
    let chosen = |lane: &str, key: &str| -> Option<String> {
        settings
            .get("domains")
            .and_then(|d| d.get(lane))
            .and_then(|l| l.get(key))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let claimed: Vec<String> = rows.iter().map(|(d, _, _)| d.clone()).collect();
    let primary = claimed.first().cloned();

    let inbox: Vec<Value> = rows
        .iter()
        .map(|(domain, provider, verified)| {
            json!({
                "domain": domain,
                "provider": provider,
                "verified": verified,
                "state": if *verified { "active" } else { "waiting_on_records" },
            })
        })
        .collect();

    // Automation: a subdomain of the account's own domain. Never the root — that is the whole point.
    let automation_host =
        chosen("automation", "host").or_else(|| primary.as_ref().map(|d| format!("mg.{d}")));
    let automation_verified = match automation_host.as_deref() {
        Some(h) => {
            let host_claimed = claimed.iter().any(|d| d.eq_ignore_ascii_case(h));
            host_claimed && resolves(h).await
        }
        None => false,
    };
    let automation = json!({
        "lane": "automation",
        "host": automation_host,
        "state": if primary.is_none() {
            "needs_a_domain"
        } else if automation_verified {
            "active"
        } else {
            "waiting_on_records"
        },
        "why": "Campaigns and sequences send from here, so a bad blast cannot damage the name your \
                replies come from.",
        "records_hint": automation_host.as_ref().map(|h| json!([
            {"record": "CNAME", "name": format!("{h}"), "value": "mailgun.org (or the value your provider shows when you add this subdomain)"},
            {"record": "TXT", "name": format!("{h}"), "value": "v=spf1 include:mailgun.org ~all"},
            {"record": "TXT", "name": format!("{}.domainkey.{}", "k1", h), "value": "the DKIM value your provider shows for this subdomain"}
        ])),
    });

    // Calendar: the host a booking page is advertised on. Resolution is the whole test — a booking
    // page that does not resolve is a lost appointment, so it is never advertised early.
    let calendar_host =
        chosen("calendar", "host").or_else(|| primary.as_ref().map(|d| format!("booking.{d}")));
    let calendar_verified = match calendar_host.as_deref() {
        Some(h) => resolves(h).await,
        None => false,
    };
    let calendar = json!({
        "lane": "calendar",
        "host": calendar_host,
        "state": if primary.is_none() {
            "needs_a_domain"
        } else if calendar_verified {
            "active"
        } else {
            "waiting_on_records"
        },
        "why": "Public booking pages for your own customers. Until the name resolves, pages are served \
                on the platform host so nothing is ever dead.",
        "records_hint": calendar_host.as_ref().map(|h| json!([
            {"record": "CNAME", "name": format!("{h}"), "value": "inbox.coreswiftcrm.com"}
        ])),
    });

    Json(json!({
        "inbox": inbox,
        "automation": automation,
        "calendar": calendar,
        "can_edit": is_manager(&c),
    }))
    .into_response()
}

/// `PUT /api/mail-compliance/domains` — record the hostnames the account wants for its two optional
/// lanes. Only names under a domain the account has already claimed are accepted.
async fn put_domains(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<LanesIn>,
) -> Response {
    let Some(tenant_id) = tenant_of(&c) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "no tenant" })),
        )
            .into_response();
    };
    if !is_manager(&c) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "only the account owner or an admin can change this" })),
        )
            .into_response();
    }
    let claimed: Vec<String> = sqlx::query_scalar::<_, String>(
        "SELECT domain FROM private_email_domains WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .unwrap_or_default();

    let automation = clean_host(body.automation_host);
    let calendar = clean_host(body.calendar_host);
    for (lane, host) in [("automation", &automation), ("calendar", &calendar)] {
        if let Some(h) = host {
            if !under_claimed_domain(h, &claimed) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": format!(
                            "the {lane} lane must be a subdomain of a domain this account has added"
                        )
                    })),
                )
                    .into_response();
            }
        }
    }

    let patch = json!({
        "automation": {"host": automation},
        "calendar": {"host": calendar},
    });
    let result = sqlx::query(
        "UPDATE tenants
            SET settings = jsonb_set(
                  coalesce(settings, '{}'::jsonb), '{domains}',
                  coalesce(settings->'domains', '{}'::jsonb) || $2::jsonb, true)
          WHERE id = $1",
    )
    .bind(tenant_id)
    .bind(&patch)
    .execute(&state.db)
    .await;
    match result {
        Ok(_) => {
            // A cleared lane must fall back to the suggested host, exactly like the page wording.
            let _ = sqlx::query(
                "UPDATE tenants SET settings = jsonb_strip_nulls(settings) WHERE id = $1",
            )
            .bind(tenant_id)
            .execute(&state.db)
            .await;
            Json(json!({ "saved": true })).into_response()
        }
        Err(e) => {
            tracing::error!("domain lane save failed for tenant {tenant_id}: {e}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "not saved" })),
            )
                .into_response()
        }
    }
}

fn csv_response(name: &str, body: String) -> Response {
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment"));
    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/csv; charset=utf-8"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/overview", get(overview))
        .route(
            "/suppressions",
            get(list_suppressions).post(add_suppression),
        )
        .route("/suppressions/:id", delete(remove_suppression))
        .route("/suppressions.csv", get(suppressions_csv))
        .route("/addresses.csv", get(addresses_csv))
        .route(
            "/unsubscribe-page",
            get(get_unsubscribe_page).put(put_unsubscribe_page),
        )
        .route("/domains", get(get_domains).put(put_domains))
        // The session layer this surface never had (kanban t_bec0f198). Every handler below takes
        // `Extension<Claims>` and scopes by `tenant_of(&c)`, and the app-wide credential boundary
        // only REFUSES an anonymous caller — it does not put `Claims` on the request. Without this
        // layer every path answered 500 "Missing request extension: Claims", so the whole surface
        // was dead. Same layer, same order as every sibling module router.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
        .with_state(state)
}

/// Merged at the ROOT of the app: these paths are their own namespace, and the opt-out arms are
/// deliberately anonymous (the token in the message IS the credential).
pub fn public_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/api/public/unsubscribe/:token",
            get(show_unsubscribe).post(one_click),
        )
        // The provider's own verdict stream. Anonymous by necessity and verified by signature —
        // the HMAC is the credential, and an unsigned body writes nothing.
        .route(
            "/api/webhooks/mailgun/events",
            post(webhook::mailgun_events),
        )
        .with_state(state)
}
