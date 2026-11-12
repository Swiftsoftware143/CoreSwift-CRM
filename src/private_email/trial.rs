//! The FREE mailbox: 30 days of full send+receive, then RECEIVE-ONLY.
//!
//! WHAT THIS IS (CEO spec, David 2026-11-11 — revised; supersedes the 30-day-history idea)
//!
//! A free account gets ONE mailbox with full function — send and receive — for a trial window. When
//! the window ends the mailbox does NOT disappear and no mail is deleted: it drops to RECEIVE-ONLY.
//! Sending is the paid action, and replying to a customer is the moment a working user converts.
//! Upgrading restores sending on the very next request, mid-trial or long after expiry.
//!
//! WHY THE DECISION IS DERIVED AND NOT READ FROM THE COLUMN
//!
//! `private_email_boxes.access_mode` records the STATE for the console and the audit trail, and the
//! daily sweep (`expire_trials`) is what flips `trial` → `receive_only`. But the send path does not
//! trust that column: it asks the plan (registry `email_sending`) and the mailbox's own
//! `trial_ends_at`. Two consequences, both wanted:
//!
//!   * an upgrade restores sending immediately — no job has to run, and no plan writer has to
//!     remember to touch any mailbox (the same reasoning as the catch-all withdrawal, t_6b54961b);
//!   * a mailbox can never be stuck receive-only because a sweep was missed or the box rebooted.
//!
//! Every number here comes from the module registry (`email_sending`, `email_trial_days`,
//! `email_outbound_per_day`, `email_storage_mb`), never from a constant in this file: the admin
//! panel can change the trial length, the daily ceiling or the storage ceiling without a redeploy.

use axum::{extract::State, Extension, Json};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::models::PrivateEmailBox;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The caller-facing sentence used when the plan sells no sending and the trial window is closed.
/// It names BOTH doors the brief requires — the $1 inbox and the CRM upgrade — because "upgrade" with
/// no door is a dead end, and it says plainly that nothing was lost.
const RECEIVE_ONLY: &str =
    "Your free mailbox is receive-only now, so this message was not sent. Nothing has been deleted \
     and mail is still arriving. Keep just the inbox for $1/month, or upgrade the CRM — either one \
     restores sending immediately.";

/// May this account send right now, and if not, why not.
#[derive(Debug, Clone)]
pub struct SendAccess {
    pub allowed: bool,
    /// The sentence the caller sees (the console prints it verbatim), when refused.
    pub refusal: Option<String>,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub days_left: Option<i64>,
    /// True when the free trial is what is granting sending (so the UI can show the countdown).
    pub on_trial: bool,
}

/// Does this account's PLAN sell sending? (registry `email_sending`, `enabled` = yes)
///
/// `no_plan` resolves as `enabled = true` in `module_registry` (a deliberate legacy tolerance for
/// module keys). For a SENDING question that tolerance would hand a plan-less account unlimited
/// mail, so — exactly as `feature_gate::get_plan_features` does for private email — `no_plan` is
/// treated as "not sold".
pub async fn plan_sells_sending(pool: &PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let ent = crate::module_registry::resolve(pool, tenant_id, "email_sending").await?;
    Ok(ent.enabled && ent.source != "no_plan")
}

/// One numeric limit from the registry: `None` means UNLIMITED or not configured (an enabled
/// feature with no `limit_value`), which is how a paid plan spells "no ceiling".
pub async fn limit_of(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<Option<i64>, AppError> {
    let ent = crate::module_registry::resolve(pool, tenant_id, key).await?;
    if !ent.enabled {
        return Ok(None);
    }
    Ok(match ent.limit_value {
        Some(v) if v > 0.0 => Some(v as i64),
        _ => None,
    })
}

/// Days a free mailbox keeps full function, from the registry (`email_trial_days`).
/// Falls back to 30 when the feature is missing from an un-migrated database — the shipped value —
/// so a missing row degrades to the sold number instead of "no trial at all".
pub async fn trial_days(pool: &PgPool, tenant_id: Uuid) -> Result<i64, AppError> {
    Ok(limit_of(pool, tenant_id, "email_trial_days")
        .await?
        .unwrap_or(30))
}

/// THE send decision. See the module docs for why it is derived rather than read.
pub async fn send_access(pool: &PgPool, tenant_id: Uuid) -> Result<SendAccess, AppError> {
    let sells = plan_sells_sending(pool, tenant_id).await?;
    send_access_with(pool, tenant_id, sells).await
}

/// The same decision when the caller already asked the plan (one query instead of two on the send
/// path, which pays this on every message).
async fn send_access_with(
    pool: &PgPool,
    tenant_id: Uuid,
    sells_sending: bool,
) -> Result<SendAccess, AppError> {
    if sells_sending {
        return Ok(SendAccess {
            allowed: true,
            refusal: None,
            trial_ends_at: None,
            days_left: None,
            on_trial: false,
        });
    }

    // The free plan does not sell sending: the MAILBOX's trial window is the only thing that can
    // grant it. One mailbox per free account (registry), so one window.
    let row: Option<(Option<DateTime<Utc>>,)> = sqlx::query_as(
        r#"
        SELECT trial_ends_at
          FROM private_email_boxes
         WHERE tenant_id = $1 AND status = 'active' AND access_mode = 'trial'
         ORDER BY created_at ASC
         LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    let now = Utc::now();
    match row.and_then(|(ends,)| ends) {
        Some(ends) if ends > now => Ok(SendAccess {
            allowed: true,
            refusal: None,
            trial_ends_at: Some(ends),
            // Ceil, so "ends in 6 hours" reads as 1 day left and never as 0 while sending works.
            days_left: Some(cdiv_days(ends, now)),
            on_trial: true,
        }),
        ends => Ok(SendAccess {
            allowed: false,
            refusal: Some(RECEIVE_ONLY.to_string()),
            trial_ends_at: ends,
            days_left: None,
            on_trial: false,
        }),
    }
}

/// Whole (ceil) days from `now` to `ends`; used for the console's "N days left".
fn cdiv_days(ends: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    let secs = (ends - now).num_seconds();
    if secs <= 0 {
        0
    } else {
        (secs + 86_399) / 86_400
    }
}

/// THE one enforcement point for outbound mail (`send_handler::deliver`, which every sender funnels
/// through: `POST /send` and the inbox's reply/forward arm).
///
/// Order matters: the plan/trial question first (it is the product rule), then the fair-use daily
/// ceiling, then storage. Each refusal is a sentence a customer can act on — 402 for "buy sending",
/// 429 for "you hit today's fair-use ceiling", 402 for "storage is full" — never a 500.
pub async fn enforce_send(
    db: &PgPool,
    tenant_id: Uuid,
    mailbox: &PrivateEmailBox,
) -> Result<(), AppError> {
    let sells = plan_sells_sending(db, tenant_id).await?;
    let access = send_access_with(db, tenant_id, sells).await?;
    if !access.allowed {
        return Err(AppError::UpgradeRequired(
            access.refusal.unwrap_or_else(|| RECEIVE_ONLY.to_string()),
        ));
    }

    // A mailbox the sweep already flipped to receive-only must not send while the plan is still
    // free — the window is closed even if a stale row says otherwise.
    if !sells && mailbox.access_mode == "receive_only" {
        return Err(AppError::UpgradeRequired(RECEIVE_ONLY.to_string()));
    }

    // Fair-use ceiling for the trial (free plans only: a paid plan carries no `limit_value`).
    if let Some(cap) = limit_of(db, tenant_id, "email_outbound_per_day").await? {
        let (sent,): (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*)
              FROM private_email_messages
             WHERE tenant_id = $1
               AND direction = 'outbound'
               AND created_at > NOW() - INTERVAL '24 hours'
            "#,
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .map_err(AppError::Database)?;
        if sent >= cap {
            return Err(AppError::RateLimited(format!(
                "You have sent {} messages in the last 24 hours, which is the fair-use ceiling for \
                 a free mailbox ({} a day). This protects the address's deliverability — it resets \
                 on a rolling 24-hour basis, and a paid plan raises it.",
                sent, cap
            )));
        }
    }

    // ── STORAGE DOES NOT BLOCK SENDING (B206, David 2026-11-11) ──────────────────────────────────
    // This function used to stop a send once `email_storage_mb` was reached, and it counted only
    // stored messages. That was the clock-era rule wearing a quota's clothes: it refused the ONE
    // action a support inbox exists for while doing nothing about the number itself. The decided
    // rule is narrower and honest — a full mailbox keeps accepting plain mail and keeps sending it;
    // the only thing it pauses is adding NEW attachments/files, which is enforced at the upload
    // door (`storage::enforce_new_file_room`). Deleting nothing and refusing nothing is the promise;
    // the meter, the banner and the monthly reminder are how the owner is told to make room.

    Ok(())
}

/// The states, recomputed for one tenant, and persisted so the row never disagrees with the send
/// decision: `paid` when the plan sells sending, else `trial` while the window is open, else
/// `receive_only`.
///
/// WHY THIS EXISTS (kanban t_9207d95b). `access_mode` is written at creation (`trial` on a free
/// plan, `paid` on a paid one) and by the sweep (`trial` → `receive_only`). Nothing moved a mailbox
/// the other way, so a free mailbox upgraded mid-trial — or long after it expired — kept the word
/// `receive_only` on its row for ever even though `send_access` had already granted sending (the
/// send decision is derived, so mail flowed; the ROW was the liar). That also meant the sweep could
/// later stamp `mailbox.trial_expired` on a paying customer's audit trail.
///
/// The plan writers call this in the SAME request that changes the plan, so the console, the audit
/// trail and the send path agree the moment the upgrade answers. It is idempotent twice over: a
/// row already in the wanted state is skipped, and the UPDATE is guarded on the old value it read,
/// so two passes (or two instances) cannot write a second audit row.
pub async fn reconcile_access_modes(pool: &PgPool, tenant_id: Uuid) -> Result<u64, AppError> {
    let sells = plan_sells_sending(pool, tenant_id).await?;

    let boxes: Vec<(Uuid, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        r#"
        SELECT id, access_mode, trial_ends_at
          FROM private_email_boxes
         WHERE tenant_id = $1 AND status = 'active'
        "#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    let now = Utc::now();
    let mut changed = 0u64;
    for (box_id, mode, ends) in boxes {
        let (desired, reason) = if sells {
            ("paid", "the plan sells sending")
        } else if ends.map(|e| e > now).unwrap_or(false) {
            (
                "trial",
                "the plan sells no sending, but this mailbox is still inside its trial window",
            )
        } else {
            (
                "receive_only",
                "the plan sells no sending and the trial window has closed",
            )
        };
        if desired == mode {
            continue;
        }

        let affected = sqlx::query(
            r#"
            UPDATE private_email_boxes
               SET access_mode = $1, updated_at = NOW()
             WHERE id = $2 AND access_mode = $3
            "#,
        )
        .bind(desired)
        .bind(box_id)
        .bind(&mode)
        .execute(pool)
        .await
        .map_err(AppError::Database)?
        .rows_affected();
        if affected == 0 {
            continue; // another pass won the race — no second audit row
        }
        changed += 1;

        crate::audit::logger::log_event(
            pool,
            tenant_id,
            None,
            "mailbox.access_mode_changed",
            "private_email_box",
            Some(box_id),
            Some(json!({"from": mode, "to": desired, "reason": reason})),
            None,
        )
        .await;

        tracing::info!(
            mailbox = %box_id,
            tenant = %tenant_id,
            from = %mode,
            to = %desired,
            reason,
            "mailbox state settled to match the plan"
        );
    }
    Ok(changed)
}

/// The daily sweep: `trial` → `receive_only` for every mailbox whose window has closed.
///
/// Idempotent by construction (`WHERE access_mode = 'trial'` on both the read and the write), so
/// running it hourly, twice at boot, or from two instances cannot flip a mailbox twice or write two
/// audit rows. RECEIVE is untouched: this changes permission to SEND, never a message, never a
/// mailbox row.
///
/// A tenant whose plan SELLS sending is settled to `paid` instead of being expired: the window is a
/// fact about the plan, not about the calendar, and a paying customer must never receive a "trial
/// expired" audit row (see `reconcile_access_modes`).
pub async fn expire_trials(pool: &PgPool) -> Result<u64, AppError> {
    let due: Vec<(Uuid, Uuid, Option<DateTime<Utc>>)> = sqlx::query_as(
        r#"
        SELECT id, tenant_id, trial_ends_at
          FROM private_email_boxes
         WHERE access_mode = 'trial'
           AND trial_ends_at IS NOT NULL
           AND trial_ends_at <= NOW()
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    let mut flipped = 0u64;
    for (box_id, tenant_id, ends_at) in due {
        if plan_sells_sending(pool, tenant_id).await? {
            // The plan was upgraded through a writer that does not settle mailboxes (an operator
            // feature override, a cross-app seat). Sending is already allowed; make the row say so
            // rather than expiring a paying customer.
            reconcile_access_modes(pool, tenant_id).await?;
            continue;
        }

        let changed = sqlx::query(
            r#"
            UPDATE private_email_boxes
               SET access_mode = 'receive_only',
                   trial_expired_at = NOW(),
                   updated_at = NOW()
             WHERE id = $1 AND access_mode = 'trial'
            "#,
        )
        .bind(box_id)
        .execute(pool)
        .await?
        .rows_affected();
        if changed == 0 {
            continue; // another pass won the race — no double audit row, no double count
        }
        flipped += 1;

        // The audit row (brief §4.2): ONE per transition, written through the app's own audit
        // logger so it lands in `audit_logs` beside every other operator-visible change, not in a
        // private side table. A failed insert must not abort the sweep: the flip is the durable
        // fact and the row is its evidence.
        let payload = json!({
            "mailbox_id": box_id,
            "trial_ends_at": ends_at,
            "access_mode": "receive_only",
        });
        crate::audit::logger::log_event(
            pool,
            tenant_id,
            None,
            "mailbox.trial_expired",
            "private_email_box",
            Some(box_id),
            Some(payload),
            None,
        )
        .await;

        tracing::info!(
            mailbox = %box_id,
            tenant = %tenant_id,
            trial_ends_at = ?ends_at,
            "free mailbox trial ended — now receive-only (mail kept, sending needs a paid plan)"
        );
    }
    Ok(flipped)
}

/// Everything the console needs to render the trial honestly, in one read: the plan answer, the
/// countdown, today's fair-use usage and the storage meter. Never a permission decision — the send
/// path asks `send_access`/`enforce_send`, and the two are computed from the same registry rows so
/// the banner and the refusal cannot disagree.
pub async fn mailbox_status(pool: &PgPool, tenant_id: Uuid) -> Result<Value, AppError> {
    let sells = plan_sells_sending(pool, tenant_id).await?;
    let access = send_access(pool, tenant_id).await?;
    let trial = trial_days(pool, tenant_id).await?;

    let boxes = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 AND status = 'active' ORDER BY created_at ASC",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    let (sent_today,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM private_email_messages WHERE tenant_id = $1 AND direction = 'outbound' AND created_at > NOW() - INTERVAL '24 hours'",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    let mailboxes: Vec<Value> = boxes
        .iter()
        .map(|b| {
            // Per-mailbox countdown, from the same clock the send path uses.
            let days_left = b
                .trial_ends_at
                .map(|ends| cdiv_days(ends, Utc::now()).max(0));
            json!({
                "id": b.id,
                "email_address": b.email_address,
                "access_mode": b.access_mode,
                "trial_ends_at": b.trial_ends_at,
                "trial_expired_at": b.trial_expired_at,
                "days_left": days_left,
            })
        })
        .collect();

    let storage_usage = super::storage::usage(pool, tenant_id).await;
    let storage_retention = super::storage::retention(pool, tenant_id).await;

    Ok(json!({
        "plan_sells_sending": sells,
        "can_send": access.allowed,
        "send_blocked_reason": access.refusal,
        "on_trial": access.on_trial,
        "trial_days": trial,
        "trial_ends_at": access.trial_ends_at,
        "days_left": access.days_left,
        "sent_last_24h": sent_today,
        "send_cap_per_day": limit_of(pool, tenant_id, "email_outbound_per_day").await?,
        // The storage state, from the ONE calculation the meter, the banner and the 100% refusal
        // share (`storage::usage`). `storage_limit_mb` keeps its old name for the console.
        "storage_used_bytes": storage_usage.used_bytes,
        "storage_limit_mb": storage_usage.quota_bytes / (1024 * 1024),
        "storage_quota_bytes": storage_usage.quota_bytes,
        "storage_percent": storage_usage.percent(),
        "storage_level": storage_usage.level(),
        "attachments_accepted": storage_usage.accepts_new_files(),
        "retention_days": storage_retention.days,
        "retention_first_expiry_at": storage_retention.first_expiry_at,
        "retention_sentence": super::storage::retention_sentence(&storage_retention),
        "mailboxes": mailboxes,
        // The plan's mailbox allowance, resolved from the registry. A defined number (or null when
        // the plan sells unlimited mailboxes); never "module on, limit missing", which is the
        // inconsistency this screen used to have no way to see.
        "mailbox_limit": limit_of(pool, tenant_id, "email_mailboxes").await?,
        "domain_limit": limit_of(pool, tenant_id, "email_domains").await?,
    }))
}

/// GET /api/private-email/mailbox-status — the console's view of the trial and the fair-use meters.
///
/// Read-only and cheap; it is what lets the inbox say "Free inbox: 12 days left" and, after the
/// window closes, show the receive-only state WITH the upgrade path instead of a dead composer.
///
/// BYOK (David, 2026-11-11): there is no CoreSwift-hosted trial address any more. A free account
/// starts with NO mailbox and NO clock — the trial begins when the first mailbox is created on a
/// domain the customer has verified with their own Mailgun account (see
/// `mailbox_handler::provision_mailbox`), so DNS propagation never eats the free month.
pub async fn mailbox_status_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(mailbox_status(&state.db, tenant_id).await?))
}

#[cfg(test)]
mod tests {
    use super::cdiv_days;
    use chrono::{Duration, Utc};

    /// The countdown must never say "0 days left" while the mailbox can still send, and must never
    /// say "1 day left" for a window that already closed.
    #[test]
    fn the_countdown_rounds_up_and_stops_at_zero() {
        let now = Utc::now();
        assert_eq!(cdiv_days(now + Duration::seconds(1), now), 1);
        assert_eq!(cdiv_days(now + Duration::hours(6), now), 1);
        assert_eq!(cdiv_days(now + Duration::days(30), now), 30);
        assert_eq!(cdiv_days(now - Duration::seconds(1), now), 0);
        assert_eq!(cdiv_days(now - Duration::days(9), now), 0);
    }
}
