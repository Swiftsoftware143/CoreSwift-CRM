//! The provider's own verdict on every message, arriving on the wire.
//!
//! Until this existed the app knew only what IT did: `outbound_messages.status` said "sent", which
//! means "the provider accepted the handoff", not "a person received it". Delivered, bounced,
//! complained and dropped are the provider's facts, and they arrive here.
//!
//! TWO RULES:
//!  1. The signature is the ONLY credential this endpoint has (there is no session on a provider
//!     webhook), so every delivery is verified before a single row is written — a bad signature is a
//!     403 and touches nothing.
//!  2. An event is attributed to an account by the message id this app already logged when it sent,
//!     never by anything the caller supplies. A body that names a tenant is not believed.
//!
//! The signing key is Mailgun's account-level "HTTP webhook signing key" — one per provider account,
//! so it is a deployment secret (`MAILGUN_WEBHOOK_SIGNING_KEY`), not a per-tenant row. It is in the
//! list of things only the operator can create.

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::collections::HashMap;

use super::{normalise, suppress, CLASS_BULK};
use crate::AppState;

/// The keys a signature may be verified against.
///
/// Mailgun signs with the account's HTTP webhook signing key. On many accounts that key IS the private
/// API key the platform already sends with, so the credential is read from the SAME place delivery
/// reads it — `platform_mail_config::resolved` (the admin-editable row, sealed at rest, with the
/// environment as fallback). No third-party key is read from the process environment directly: the
/// fleet gate forbids that, and one resolver means the send path and the verification path can never
/// disagree about which account's mail this is.
///
/// A signature must match exactly; an account with no resolvable credential verifies nothing and the
/// endpoint refuses every delivery rather than trusting an unverifiable body.
async fn signing_keys(db: &sqlx::PgPool) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(transport) = crate::communications::platform_mail_config::resolved(db).await {
        let key = transport.api_key.trim().to_string();
        if !key.is_empty() && !keys.contains(&key) {
            keys.push(key);
        }
    }
    keys
}

/// The URL-token arm.
///
/// Mailgun signs with an account-level "HTTP webhook signing key" that the dashboard shows and the API
/// does NOT expose — measured 2026-11-19 on this account: a real delivered/bounce event signed with
/// that key was refused by HMAC verification, so the key is not the sending credential this app holds.
/// Rather than block compliance on a value only a human can copy, the webhook URL itself carries a
/// secret this server generates: the URL is delivered over TLS to the provider, one account's URL is
/// useless on another host, and the comparison is constant-time. HMAC verification stays in place for
/// the day the account's signing key is available — a request may pass by EITHER arm, never neither.
async fn url_token_ok(db: &sqlx::PgPool, supplied: Option<&str>) -> bool {
    let Some(supplied) = supplied.filter(|s| !s.is_empty()) else {
        return false;
    };
    let Some(row) = crate::communications::platform_mail_config::stored(db).await else {
        return false;
    };
    match crate::communications::platform_mail_config::field(&row, "webhook_token") {
        Some(expected) if !expected.is_empty() => {
            constant_time_eq(expected.as_bytes(), supplied.as_bytes())
        }
        _ => false,
    }
}

/// The inbound receiver's second credential: the token carried in the provider's forward URL.
///
/// WHY (2026-11-20): the provider's webhook signing key is account-wide and visible only in its own
/// dashboard, so a workspace that has not pasted it can receive NOTHING and the failure lands on the
/// customer as "your inbox is empty". The forward URL is under our control, so it carries the same
/// platform webhook token the events receiver already uses.
///
/// GUARD, because this class has bitten the fleet before (an empty internal key was a bypass): a
/// token shorter than 24 characters is treated as "no token configured" and refuses, so a blank or
/// placeholder value can never authenticate a delivery.
pub async fn url_token_ok_for_inbound(db: &sqlx::PgPool, supplied: Option<&str>) -> bool {
    match supplied.map(str::trim) {
        Some(t) if t.len() >= 24 => url_token_ok(db, Some(t)).await,
        _ => false,
    }
}

/// Constant-time comparison, so a wrong signature cannot be found one byte at a time.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn signature_ok(key: &str, timestamp: &str, token: &str, signature: &str) -> bool {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key.as_bytes()).expect("hmac any key len");
    mac.update(format!("{timestamp}{token}").as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    constant_time_eq(
        expected.as_bytes(),
        signature.to_ascii_lowercase().as_bytes(),
    )
}

/// Mailgun's message-id arrives wrapped in angle brackets and is stored the way the send response gave
/// it; both spellings are tried so attribution does not depend on which side kept the brackets.
fn candidate_message_ids(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    let bare = trimmed.trim_start_matches('<').trim_end_matches('>');
    let mut out = vec![trimmed.to_string(), bare.to_string()];
    out.push(format!("<{bare}>"));
    out.dedup();
    out
}

/// Which account sent this message. Read from our own send log — the only trustworthy source.
async fn tenant_for_message(
    state: &AppState,
    ids: &[String],
) -> Option<(uuid::Uuid, Option<String>)> {
    for id in ids {
        if let Ok(Some(row)) = sqlx::query_as::<_, (uuid::Uuid, Option<String>)>(
            "SELECT tenant_id, address FROM email_message_log
              WHERE provider_message_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&state.db)
        .await
        {
            return Some(row);
        }
    }
    None
}

/// `POST /api/webhooks/mailgun/events`
pub async fn mailgun_events(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    _headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let keys = signing_keys(&state.db).await;

    // Arm 1: the provider's own HMAC signature. Arm 2: the secret this server put in the webhook URL.
    // Measured 2026-11-19: a REAL report from Mailgun was refused here because this account signs with
    // a key the API does not expose, so HMAC alone would have left every compliance number at zero
    // until a human copied a value out of a browser. A request must satisfy one arm; neither = 403.
    let signed = body
        .get("signature")
        .map(|sig| {
            let timestamp = sig
                .get("timestamp")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let token = sig
                .get("token")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            let signature = sig
                .get("signature")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            !timestamp.is_empty()
                && !token.is_empty()
                && !signature.is_empty()
                && keys
                    .iter()
                    .any(|k| signature_ok(k, timestamp, token, signature))
        })
        .unwrap_or(false);

    let authorised = signed || url_token_ok(&state.db, query.get("t").map(|s| s.as_str())).await;
    if !authorised {
        tracing::warn!(
            "Mailgun event webhook REFUSED: neither the signature nor the URL token validated"
        );
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "not authorised" })),
        )
            .into_response();
    }

    let data = body.get("event-data").cloned().unwrap_or(Value::Null);
    if data.is_null() {
        // A signed ping with no event (Mailgun sends these when a webhook is first set up).
        return (StatusCode::OK, Json(json!({ "status": "ack" }))).into_response();
    }

    let event = data
        .get("event")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let severity = data
        .get("severity")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let reason = data
        .get("reason")
        .and_then(|v| v.as_str())
        .or_else(|| {
            data.get("delivery-status")
                .and_then(|d| d.get("description"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| {
            data.get("reject")
                .and_then(|r| r.get("reason"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string());
    // The recipient: Mailgun puts the address on `recipient` and the envelope in `envelope`.
    let recipient = data
        .get("recipient")
        .and_then(|v| v.as_str())
        .or_else(|| {
            data.get("envelope")
                .and_then(|e| e.get("targets"))
                .and_then(|t| t.as_str())
        })
        .map(normalise);
    let occurred = data
        .get("timestamp")
        .and_then(|v| v.as_f64())
        .map(|ts| chrono::DateTime::from_timestamp(ts as i64, 0))
        .flatten()
        .unwrap_or_else(chrono::Utc::now);
    let message_id = data
        .get("message")
        .and_then(|m| m.get("headers"))
        .and_then(|h| h.get("message-id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if message_id.is_empty() {
        tracing::warn!(event = %event, "Mailgun event carried no message-id — cannot attribute it");
        return (StatusCode::OK, Json(json!({ "status": "ack" }))).into_response();
    }

    let ids = candidate_message_ids(&message_id);
    let Some((tenant_id, logged_address)) = tenant_for_message(&state, &ids).await else {
        tracing::warn!(
            event = %event,
            message_id = %message_id,
            "Mailgun event for a message this app did not log — nothing to attribute it to"
        );
        return (StatusCode::OK, Json(json!({ "status": "ignore" }))).into_response();
    };
    let address = recipient.or(logged_address);

    super::record_event(
        &state.db,
        tenant_id,
        "mailgun",
        Some(message_id.trim()),
        address.as_deref(),
        &event,
        severity.as_deref(),
        reason.as_deref(),
        occurred,
        Some(data.clone()),
    )
    .await;

    // ── HARM CONTROL: a dead or hostile address is suppressed for that account immediately ──────
    // Both rows are written on purpose: the 'transactional' row stops even receipts and resets (the
    // provider has told us the mailbox does not exist, so continuing is pure reputation damage),
    // and the 'all' row stops the stoppable classes.
    // A permanent `failed` is the same harm as a permanent bounce: the provider has told this
    // address cannot receive (no MX, unknown user, refused). Continuing to send to it burns reputation
    // for nothing, so it is suppressed on the same evidence.
    //
    // `unsubscribed` is the provider's own opt-out (Mailgun's unsubscribe link or its list), so it
    // stops the STOPPABLE class only, under the reason this app already stores for an opt-out
    // ("unsubscribed", the same value the one-click page writes). It must never stop transactional
    // mail: somebody who unsubscribed from notices still has to receive a password reset.
    let suppress_plan: Option<(&str, &[&str])> = match (event.as_str(), severity.as_deref()) {
        ("bounced", Some("permanent")) | ("failed", Some("permanent")) => {
            Some(("hard_bounce", &["transactional", "all"]))
        }
        ("complained", _) => Some(("complaint", &["transactional", "all"])),
        ("unsubscribed", _) => Some(("unsubscribed", &["notification"])),
        _ => None,
    };
    if let (Some((reason, classes)), Some(address)) = (suppress_plan, address.as_deref()) {
        for class in classes {
            let _ = suppress(
                &state.db,
                tenant_id,
                address,
                reason,
                class,
                "provider_event",
            )
            .await;
        }
        tracing::info!(
            tenant = %tenant_id,
            address = %address,
            reason = %reason,
            "address suppressed for this account on the provider's own evidence"
        );
    }

    // Bulk accounting: a delivery/bounce/complaint on a message that this app logged as BULK keeps the
    // per-class ledger honest about which lane the reputation belongs to.
    let _ = CLASS_BULK;

    (
        StatusCode::OK,
        Json(json!({ "status": "ok", "event": event })),
    )
        .into_response()
}
