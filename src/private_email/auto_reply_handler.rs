use axum::{extract::{Path, State}, Extension, Json};
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
        "SELECT * FROM private_email_auto_replies WHERE tenant_id = $1 ORDER BY created_at DESC"
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

    let result = sqlx::query(
        "DELETE FROM private_email_auto_replies WHERE id = $1 AND tenant_id = $2"
    )
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
    trigger_value: &str,  // tag name, list name, stage name, or ""
    recipient_email: &str,
) {
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
            fire_single_rule(pool, &rule, recipient_email).await;
        }
        return;
    }

    for rule in rules {
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

async fn fire_single_rule(
    pool: &sqlx::PgPool,
    rule: &AutoReplyRow,
    recipient: &str,
) {
   
    use super::encryption;

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

    let from_address = match &mailbox {
        Some((addr, _)) => addr.clone(),
        None => return, // no active mailbox to send from
    };

    let domain_id = mailbox.as_ref().map(|(_, d)| *d).unwrap_or(rule.domain_id);

    let domain = sqlx::query_as::<_, (String, String, String)>(
        "SELECT domain, mailgun_api_key, mailgun_region FROM private_email_domains WHERE id = $1"
    )
    .bind(domain_id)
    .fetch_optional(pool)
    .await;

    let (mg_domain, encrypted_key, region) = match domain {
        Ok(Some(d)) => d,
        _ => return,
    };

    let tenant_id = rule.tenant_id;
    let api_key = match encryption::decrypt_api_key(tenant_id, &encrypted_key) {
        Ok(k) => k,
        Err(_) => return,
    };

    let base_url = if region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };

    let client = reqwest::Client::new();
    let _ = client
        .post(format!("{}/v3/{}/messages", base_url, mg_domain))
        .basic_auth("api", Some(&api_key))
        .form(&[
            ("from", &from_address),
            ("to", &recipient.to_string()),
            ("subject", &subject),
            ("html", &rule.body_html),
        ])
        .send()
        .await;
}
