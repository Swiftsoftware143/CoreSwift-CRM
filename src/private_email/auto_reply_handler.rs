use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct AutoReplyRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub domain_id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub name: String,
    pub trigger_type: String,
    pub trigger_value: Option<String>,
    pub subject: Option<String>,
    pub body_html: String,
    pub delay_minutes: i32,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAutoReplyRequest {
    pub domain_id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub name: String,
    pub trigger_type: String,
    pub trigger_value: Option<String>,
    pub subject: Option<String>,
    pub body_html: String,
    #[serde(default)]
    pub delay_minutes: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAutoReplyRequest {
    pub is_active: Option<bool>,
    pub subject: Option<String>,
    pub body_html: Option<String>,
    pub delay_minutes: Option<i32>,
}

pub async fn list_auto_replies(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query_as::<_, AutoReplyRow>(
        "SELECT * FROM private_email_auto_replies WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&rows).unwrap()))
}

pub async fn create_auto_reply(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateAutoReplyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // The domain (and mailbox) must belong to THIS account. Inserting a foreign or unknown id
    // tripped the foreign key and surfaced as 500 "Database error" (proven live 2026-09-22), which
    // reads as a broken server instead of the 404 it is — and it is the cross-tenant write path,
    // so it is refused explicitly rather than by accident.
    let owns_domain: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM private_email_domains WHERE id = $1 AND tenant_id = $2)",
    )
    .bind(req.domain_id)
    .bind(account_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;
    if !owns_domain {
        return Err(AppError::NotFound(
            "Domain not found on this account".to_string(),
        ));
    }
    if let Some(mailbox_id) = req.mailbox_id {
        let owns_mailbox: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM private_email_boxes WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(mailbox_id)
        .bind(account_id)
        .fetch_one(&state.db)
        .await
        .map_err(AppError::Database)?;
        if !owns_mailbox {
            return Err(AppError::NotFound(
                "Mailbox not found on this account".to_string(),
            ));
        }
    }

    let row = sqlx::query_as::<_, AutoReplyRow>(
        r#"
        INSERT INTO private_email_auto_replies (tenant_id, domain_id, mailbox_id, name, trigger_type, trigger_value, subject, body_html, delay_minutes)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(req.domain_id)
    .bind(req.mailbox_id)
    .bind(&req.name)
    .bind(&req.trigger_type)
    .bind(&req.trigger_value)
    .bind(&req.subject)
    .bind(&req.body_html)
    .bind(req.delay_minutes)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}

pub async fn update_auto_reply(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(reply_id): Path<Uuid>,
    Json(req): Json<UpdateAutoReplyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query_as::<_, AutoReplyRow>(
        r#"
        UPDATE private_email_auto_replies
        SET is_active = COALESCE($3, is_active),
            subject = COALESCE($4, subject),
            body_html = COALESCE($5, body_html),
            delay_minutes = COALESCE($6, delay_minutes),
            updated_at = NOW()
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(reply_id)
    .bind(account_id)
    .bind(req.is_active)
    .bind(&req.subject)
    .bind(&req.body_html)
    .bind(req.delay_minutes)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(r) => Ok(Json(serde_json::to_value(&r).unwrap())),
        None => Err(AppError::NotFound("Auto-reply rule not found".into())),
    }
}

pub async fn delete_auto_reply(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(reply_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result =
        sqlx::query("DELETE FROM private_email_auto_replies WHERE id = $1 AND tenant_id = $2")
            .bind(reply_id)
            .bind(account_id)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Auto-reply rule not found".into()));
    }

    Ok(Json(serde_json::json!({"deleted": true})))
}

// ---- Auto-reply trigger engine ----
// Called from tag assignment, list join, contact create, pipeline stage change handlers

/// Check if any auto-reply rules fire for this event and send the email.
/// Called after: tag assigned, list member added, contact created, pipeline stage changed.
pub async fn maybe_fire_auto_reply(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    trigger_type: &str,
    trigger_value: &str, // tag name, list name, stage name, or ""
    recipient_email: &str,
    // The delivery announced itself as machine-written (`Auto-Submitted` and friends); see
    // `crate::private_email::providers::machine_generated_from_pairs`.
    machine_generated: bool,
) {
    // Three refusals before a single rule is read. Each is one of the ways an auto-responder
    // misbehaves in the wild — answering another robot (a loop), answering a null sender (a bounce
    // storm), or answering ourselves (two rules pointing at each other). They live HERE rather than
    // at the call site so every future trigger inherits them.
    if machine_generated {
        tracing::debug!(tenant = %tenant_id, "auto-reply withheld: the message is machine-generated");
        return;
    }
    if looks_automated_sender(recipient_email) {
        tracing::debug!(
            tenant = %tenant_id,
            sender = %recipient_email,
            "auto-reply withheld: the sender is an automated address"
        );
        return;
    }
    // Never answer one of this account's own addresses. A database that cannot answer is treated
    // as "do not send": a missed automatic reply is cheaper than a loop.
    match sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM private_email_boxes WHERE tenant_id = $1 AND lower(email_address) = lower($2))",
    )
    .bind(tenant_id)
    .bind(recipient_email)
    .fetch_one(pool)
    .await
    {
        Ok(true) => {
            tracing::debug!(
                tenant = %tenant_id,
                "auto-reply withheld: the sender is one of this account's own addresses"
            );
            return;
        }
        Ok(false) => {}
        Err(e) => {
            tracing::warn!(
                tenant = %tenant_id,
                error = %e,
                "auto-reply withheld: could not tell whether the sender is one of this account's addresses"
            );
            return;
        }
    }

    // Find matching active rules
    let rules = sqlx::query_as::<_, AutoReplyRow>(
        r#"
        SELECT * FROM private_email_auto_replies
        WHERE tenant_id = $1 AND trigger_type = $2 AND is_active = true
        AND (trigger_value IS NULL OR trigger_value = '' OR trigger_value = $3)
        ORDER BY delay_minutes ASC
        "#,
    )
    .bind(tenant_id)
    .bind(trigger_type)
    .bind(trigger_value)
    .fetch_all(pool)
    .await;

    let rules = match rules {
        Ok(r) => r,
        Err(_) => return,
    };

    if rules.is_empty() {
        // Check "always" rules too
        let always_rules = sqlx::query_as::<_, AutoReplyRow>(
            "SELECT * FROM private_email_auto_replies WHERE tenant_id = $1 AND trigger_type = 'always' AND is_active = true"
        )
        .bind(tenant_id)
        .fetch_all(pool)
        .await;

        let always_rules = match always_rules {
            Ok(r) => r,
            Err(_) => return,
        };

        if always_rules.is_empty() {
            return;
        }

        // Fire always rules
        for rule in always_rules {
            if claim_send_slot(pool, rule.id, recipient_email).await {
                fire_single_rule(pool, &rule, recipient_email).await;
            }
        }
        return;
    }

    for rule in rules {
        // One reply per person per rule per 24 hours. The claim is an atomic conditional upsert, so
        // two deliveries racing on the same sender answer once, not twice.
        if !claim_send_slot(pool, rule.id, recipient_email).await {
            continue;
        }
        if rule.delay_minutes > 0 {
            // Delayed — insert into delayed queue
            let _ = sqlx::query(
                r#"
                INSERT INTO delayed_actions (id, tenant_id, action_type, entity_type, entity_id, payload, execute_at, created_at)
                VALUES (gen_random_uuid(), $1, 'send_auto_reply', 'contact', NULL, $2, NOW() + ($3 || ' minutes')::INTERVAL, NOW())
                "#,
            )
            .bind(tenant_id)
            .bind(serde_json::json!({
                "auto_reply_id": rule.id,
                "recipient": recipient_email,
                "mailbox_id": rule.mailbox_id,
                "domain_id": rule.domain_id,
            }))
            .bind(rule.delay_minutes)
            .execute(pool)
            .await;
        } else {
            fire_single_rule(pool, &rule, recipient_email).await;
        }
    }
}

// Local parts that belong to a machine, never to the person who just wrote to you.
//
// A reply to one of these is a reply to nobody: `mailer-daemon`/`postmaster` are the mail system
// itself (answering a bounce is how a mail loop is born), and the rest are addresses a sender uses
// precisely so that no human reads the answer.
pub fn looks_automated_sender(from: &str) -> bool {
    let local = from
        .trim()
        .split('@')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        local.as_str(),
        "mailer-daemon"
            | "postmaster"
            | "no-reply"
            | "noreply"
            | "do-not-reply"
            | "donotreply"
            | "auto-reply"
            | "autoreply"
            | "bounces"
            | "bounce"
    ) || local.starts_with("bounce+")
}

/// Reserve the ONE automatic reply this rule may send this person in the next 24 hours.
///
/// `true` means the slot is yours and the caller should send. `false` means this person already had
/// an automatic reply from this rule inside the window, or the claim itself failed — in which case
/// stay silent: a missed reply is cheaper than a repeat.
///
/// The claim is a single conditional upsert. The row is inserted when absent and refreshed only
/// when it is older than the window, and `RETURNING` yields a row exactly when one of those
/// happened, so two deliveries racing on the same sender produce one reply — no transaction, no
/// lock, no application-level clock.
async fn claim_send_slot(pool: &sqlx::PgPool, rule_id: Uuid, recipient: &str) -> bool {
    let recipient = recipient.trim().to_ascii_lowercase();
    if recipient.is_empty() {
        return false;
    }
    let claimed = sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO private_email_auto_reply_sent (rule_id, recipient, sent_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (rule_id, recipient) DO UPDATE SET sent_at = NOW()
        WHERE private_email_auto_reply_sent.sent_at < NOW() - INTERVAL '24 hours'
        RETURNING rule_id
        "#,
    )
    .bind(rule_id)
    .bind(&recipient)
    .fetch_optional(pool)
    .await;

    match claimed {
        Ok(row) => row.is_some(),
        Err(e) => {
            tracing::warn!(rule = %rule_id, error = %e, "auto-reply withheld: the throttle could not be claimed");
            false
        }
    }
}

async fn fire_single_rule(pool: &sqlx::PgPool, rule: &AutoReplyRow, recipient: &str) {
    let subject = rule.subject.clone().unwrap_or_else(|| " ".into());

    let mailbox = if let Some(mb_id) = rule.mailbox_id {
        sqlx::query_as::<_, (String, Uuid)>(
            "SELECT email_address, domain_id FROM private_email_boxes WHERE id = $1 AND status = 'active'"
        )
        .bind(mb_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    } else {
        None
    };

    let reply_to = match &mailbox {
        Some((addr, _)) => addr.clone(),
        None => return,
    };

    let domain_id = mailbox.as_ref().map(|(_, d)| *d).unwrap_or(rule.domain_id);

    // Auto-replies are AUTOMATION: they leave from the account's mg.<domain> lane, with replies
    // routed back to the mailbox the person wrote to (kanban t_044d5d62).
    let _ = super::send_handler::send_automation_reply(
        pool,
        domain_id,
        rule.tenant_id,
        &reply_to,
        recipient,
        &subject,
        &rule.body_html,
    )
    .await;
}
