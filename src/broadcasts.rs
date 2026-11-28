//! BROADCAST — send one message, once, to a list or to everyone carrying a tag (David, 2026-11-26).
//!
//! This did not exist. What the product had was a single-recipient message (`POST
//! /api/communications/messages`, one `to` address) and *campaigns*, which are drip sequences: steps carry
//! a `delay_days` and enrolment is one contact at a time. So "mail this list once" had no door — the nearest
//! trick was a zero-delay campaign step and enrolling people individually.
//!
//! HOW IT SENDS, and why it is not a new send path: a broadcast RESOLVES its audience and **enqueues one
//! `outbound_messages` row per recipient**, then the worker delivers them. Every gate therefore applies
//! unchanged — the per-account suppression list, the opt-out footer, the one-click headers, the platform
//! mail pause, the transport each workspace actually has, and the delivery log a later provider event is
//! attributed against. A second sender would have to re-earn all of that, and would drift.
//!
//! THE AUDIENCE IS RESOLVED ONCE, AT SEND TIME, and the answer is reported per reason:
//!   - a list (`list_id`) and/or a tag (by id or name) — the union of both when both are given;
//!   - someone with no email address cannot be mailed: counted, never silently dropped;
//!   - a duplicate address is mailed once (two contacts, one inbox);
//!   - an address this account was asked to stop mailing is SKIPPED and named as such — the worker would
//!     refuse it anyway, but a broadcast that reports them as "sent" is a lie the operator would act on.

use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct Audience {
    /// A saved list (`lists.id`). Membership is read now, not when the broadcast was written.
    #[serde(default)]
    pub list_id: Option<Uuid>,
    /// A tag by id...
    #[serde(default)]
    pub tag_id: Option<Uuid>,
    /// ...or by name, because the console shows names and an id is not something a person types.
    #[serde(default)]
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BroadcastRequest {
    pub subject: String,
    pub body: String,
    #[serde(flatten)]
    pub audience: Audience,
    /// False (default) = resolve and report only. True = enqueue for the worker.
    #[serde(default)]
    pub send: bool,
}

/// One resolved audience, with the reasons people were left out.
struct Resolved {
    recipients: Vec<(Uuid, String)>,
    no_address: i64,
    suppressed: Vec<(String, String)>,
    /// Addresses that are present but NOT mail-able (CEO, 2026-11-27: "preview counts the literal string
    /// not-an-email as would_send"). Presence was never the test; validity is.
    invalid: Vec<String>,
    duplicates: i64,
}

/// Is this address actually mail-able? Presence in an `email` column was the old test, and it let the
/// literal string "not-an-email" through as `would_send`. Deliberately conservative and syntax-only -
/// deliverability is the provider's verdict, not ours - but it refuses the shapes that can only bounce:
/// no `@`, more than one, an empty local part, a domain with no dot, spaces, leading/trailing dots.
fn is_mailable_address(email: &str) -> bool {
    let e = email.trim();
    if e.is_empty() || e.len() > 254 || e.contains(char::is_whitespace) {
        return false;
    }
    let mut parts = e.split('@');
    let (Some(local), Some(domain), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    if local.is_empty() || local.len() > 64 || local.starts_with('.') || local.ends_with('.') {
        return false;
    }
    if domain.len() < 3
        || !domain.contains('.')
        || domain.starts_with('.')
        || domain.ends_with('.')
        || domain.contains("..")
    {
        return false;
    }
    domain
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

fn tenant_of(c: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)
}

/// Resolve the audience for one account. Never trusts an id: a list or tag belonging to somebody else is
/// simply not in the query, so it resolves to nobody rather than to their contacts.
async fn resolve(s: &AppState, t: Uuid, a: &Audience) -> Result<Resolved, AppError> {
    if a.list_id.is_none() && a.tag_id.is_none() && a.tag.as_deref().unwrap_or("").trim().is_empty()
    {
        return Err(AppError::BadRequest(
            "choose who this goes to: a list, or a tag".into(),
        ));
    }

    // A nil uuid is not an audience: it used to resolve to nobody and answer 200 with would_send 0,
    // which reads as "your audience is empty" when the caller actually sent a bad id.
    if a.list_id == Some(Uuid::nil()) {
        return Err(AppError::NotFound(
            "that list is not on this account".into(),
        ));
    }
    if a.tag_id == Some(Uuid::nil()) {
        return Err(AppError::NotFound("that tag is not on this account".into()));
    }
    if let Some(list_id) = a.list_id {
        let exists: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
                .bind(list_id)
                .bind(t)
                .fetch_optional(&s.db)
                .await?;
        if exists.is_none() {
            return Err(AppError::NotFound(
                "that list is not on this account".into(),
            ));
        }
    }

    let tag_id: Option<Uuid> = match (a.tag_id, a.tag.as_deref()) {
        (Some(id), _) => Some(id),
        (None, Some(name)) if !name.trim().is_empty() => {
            let found: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM tags WHERE tenant_id = $1 AND lower(name) = lower($2)",
            )
            .bind(t)
            .bind(name.trim())
            .fetch_optional(&s.db)
            .await?;
            Some(
                found
                    .ok_or_else(|| AppError::NotFound("that tag is not on this account".into()))?,
            )
        }
        _ => None,
    };

    // The union of list membership and tag assignment; DISTINCT because somebody on both must be mailed once.
    let rows: Vec<(Uuid, String)> = sqlx::query_as(
        r#"SELECT DISTINCT c.id, c.email
             FROM contacts c
            WHERE c.tenant_id = $1
              AND c.email IS NOT NULL AND btrim(c.email) <> ''
              AND (
                    ($2::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM list_members m
                         WHERE m.list_id = $2 AND m.contact_id = c.id AND m.tenant_id = $1))
                 OR ($3::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM tag_assignments ta
                         WHERE ta.tag_id = $3 AND ta.entity_type = 'contact'
                           AND ta.entity_id = c.id AND ta.tenant_id = $1))
              )"#,
    )
    .bind(t)
    .bind(a.list_id)
    .bind(tag_id)
    .fetch_all(&s.db)
    .await?;

    // Who in the audience has no address at all. Counted as its own verdict rather than folded into "0".
    let no_address: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(DISTINCT c.id)
             FROM contacts c
            WHERE c.tenant_id = $1
              AND (c.email IS NULL OR btrim(c.email) = '')
              AND (
                    ($2::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM list_members m
                         WHERE m.list_id = $2 AND m.contact_id = c.id AND m.tenant_id = $1))
                 OR ($3::uuid IS NOT NULL AND EXISTS (
                        SELECT 1 FROM tag_assignments ta
                         WHERE ta.tag_id = $3 AND ta.entity_type = 'contact'
                           AND ta.entity_id = c.id AND ta.tenant_id = $1))
              )"#,
    )
    .bind(t)
    .bind(a.list_id)
    .bind(tag_id)
    .fetch_one(&s.db)
    .await?;

    let mut seen = std::collections::HashSet::new();
    let mut duplicates = 0i64;
    let mut unique: Vec<(Uuid, String)> = Vec::new();
    for (id, email) in rows {
        let key = email.trim().to_ascii_lowercase();
        if seen.insert(key) {
            unique.push((id, email.trim().to_string()));
        } else {
            duplicates += 1;
        }
    }

    // The suppression list, read through the ONE compliance module: the reason a person opted out is what
    // the operator needs to see, not a bare count.
    let mut recipients = Vec::with_capacity(unique.len());
    let mut suppressed = Vec::new();
    let mut invalid = Vec::new();
    unique.retain(|(_, email)| {
        if is_mailable_address(email) {
            true
        } else {
            invalid.push(email.clone());
            false
        }
    });
    for (id, email) in unique {
        // THE CLASS MATTERS: a broadcast is a campaign, and only the stoppable classes are refused. A
        // transaction receipt is not blocked by a marketing opt-out, and this must not pretend otherwise.
        match crate::mail_compliance::suppression(&s.db, t, &email, "campaign").await {
            Some(reason) => suppressed.push((email, reason)),
            None => recipients.push((id, email)),
        }
    }

    Ok(Resolved {
        recipients,
        no_address,
        suppressed,
        invalid,
        duplicates,
    })
}

/// POST /api/broadcasts/preview — how many would be mailed, and who would not, before anything is sent.
pub async fn preview(
    State(s): State<AppState>,
    axum::Extension(c): axum::Extension<Claims>,
    Json(r): Json<BroadcastRequest>,
) -> ApiResult<Json<Value>> {
    let t = tenant_of(&c)?;
    let res = resolve(&s, t, &r.audience).await?;
    // A GREEN ZERO WITH NO REASON IS THE DEFECT THIS CLOSES. When nothing can be mailed the caller gets
    // the sentence that says why, not a bare 0 they have to interpret.
    let empty_reason = if res.recipients.is_empty() {
        json!("nobody in this audience can be mailed")
    } else {
        Value::Null
    };
    Ok(Json(json!({
        "would_send": res.recipients.len(),
        "no_address": res.no_address,
        "suppressed": res.suppressed.len(),
        "suppressed_examples": res.suppressed.iter().take(5)
            .map(|(e, why)| json!({"email": e, "reason": why})).collect::<Vec<_>>(),
        // Same bucket the send path refuses, counted the same way, so preview and send cannot disagree.
        "invalid": res.invalid.len(),
        "invalid_examples": res.invalid.iter().take(5).collect::<Vec<_>>(),
        "duplicate_addresses": res.duplicates,
        "sample": res.recipients.iter().take(5).map(|(_, e)| e.clone()).collect::<Vec<_>>(),
        "audience_empty_reason": empty_reason,
    })))
}

/// POST /api/broadcasts — resolve, enqueue one message per recipient, and report exactly what happened.
///
/// `send: false` (or omitted) is a preview: nothing is written. The two shapes are the same request so the
/// screen cannot preview one audience and send another.
pub async fn send(
    State(s): State<AppState>,
    axum::Extension(c): axum::Extension<Claims>,
    Json(r): Json<BroadcastRequest>,
) -> ApiResult<Json<Value>> {
    let t = tenant_of(&c)?;
    let subject = r.subject.trim().to_string();
    let body = r.body.trim().to_string();
    if !r.send {
        return preview(State(s), axum::Extension(c), Json(r)).await;
    }
    if subject.is_empty() {
        return Err(AppError::BadRequest("a broadcast needs a subject".into()));
    }
    if body.is_empty() {
        return Err(AppError::BadRequest("a broadcast needs a body".into()));
    }

    let res = resolve(&s, t, &r.audience).await?;
    if res.recipients.is_empty() {
        return Err(AppError::BadRequest(
            "nobody in that audience can be mailed — check the list, the tag, and whether the addresses were opted out"
                .into(),
        ));
    }

    let mut queued = 0i64;
    for (contact_id, email) in &res.recipients {
        // 'queued' — NOT 'pending'. The delivery worker claims rows with `status = 'queued'` and nothing
        // in this app ever reads 'pending' (measured t_ef792898: a broadcast wrote one 'pending' row that
        // no job would ever pick up, so the whole feature queued mail it could never send; every other
        // door — campaigns, communications, email, automation — inserts 'queued').
        sqlx::query(
            r#"INSERT INTO outbound_messages
                   (tenant_id, contact_id, channel, to_address, subject, body, status, is_html)
               VALUES ($1, $2, 'email', $3, $4, $5, 'queued', false)"#,
        )
        .bind(t)
        .bind(contact_id)
        .bind(email)
        .bind(&subject)
        .bind(&body)
        .execute(&s.db)
        .await
        .map_err(AppError::Database)?;
        queued += 1;
    }

    Ok(Json(json!({
        "queued": queued,
        "no_address": res.no_address,
        "suppressed": res.suppressed.len(),
        "suppressed_examples": res.suppressed.iter().take(5)
            .map(|(e, why)| json!({"email": e, "reason": why})).collect::<Vec<_>>(),
        "invalid": res.invalid.len(),
        "invalid_examples": res.invalid.iter().take(5).collect::<Vec<_>>(),
        "duplicate_addresses": res.duplicates,
        // Stated, because the queue is not the inbox: the worker delivers these, applying the suppression
        // list, the footer, the unsubscribe headers and the platform mail pause.
        "note": "queued for delivery; the send pass applies the footer, the opt-out headers and the mail pause",
    })))
}

#[cfg(test)]
mod address_validity_tests {
    use super::is_mailable_address;

    /// The CEO's finding, pinned: a present-but-unmailable string must never be `would_send`.
    #[test]
    fn a_literal_string_is_not_an_address() {
        assert!(!is_mailable_address("not-an-email"));
        assert!(!is_mailable_address("dana.whitfield"));
        assert!(!is_mailable_address("@example.com"));
        assert!(!is_mailable_address("dana@"));
        assert!(!is_mailable_address("dana@localhost"));
        assert!(!is_mailable_address("dana@@example.com"));
        assert!(!is_mailable_address("dana whitfield@example.com"));
        assert!(!is_mailable_address(".dana@example.com"));
        assert!(!is_mailable_address("dana@example..com"));
        assert!(!is_mailable_address(""));
    }

    #[test]
    fn ordinary_addresses_stay_mailable() {
        assert!(is_mailable_address("dana.whitfield@gmail.com"));
        assert!(is_mailable_address("x@y.test"));
        assert!(is_mailable_address("probe+tag@example.invalid"));
        assert!(is_mailable_address("  padded@example.com  "));
    }
}
