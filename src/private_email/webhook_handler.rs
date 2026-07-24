use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct MailgunInbound {
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    #[serde(alias = "body-plain")]
    pub body_plain: String,
    #[serde(default)]
    #[serde(alias = "body-html")]
    pub body_html: String,
    #[serde(default)]
    #[serde(alias = "Message-Id")]
    pub message_id: String,
    #[serde(default)]
    #[serde(alias = "In-Reply-To")]
    pub in_reply_to: String,
    #[serde(default)]
    #[serde(alias = "stripped-text")]
    pub stripped_text: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub signature: String,
}

/// Webhook handler for inbound Mailgun emails.
/// POST /api/v1/webhooks/mailgun/inbound
/// This endpoint is unauthenticated (Mailgun calls it).
pub async fn inbound_webhook(
    State(state): State<AppState>,
    Json(payload): Json<MailgunInbound>,
) -> ApiResult<Json<serde_json::Value>> {
    // Extract sender email
    let sender_email = extract_email(&payload.sender);
    // Extract recipient email
    let recipient_email = extract_email(&payload.recipient);

    if sender_email.is_empty() || recipient_email.is_empty() {
        return Ok(Json(json!({"received": false, "error": "missing sender or recipient"})));
    }

    // Find which mailbox this is for (by recipient email)
    let mailbox = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        r#"
        SELECT id, tenant_id, user_id
        FROM private_email_boxes
        WHERE email_address = $1 AND status = 'active'
        LIMIT 1
        "#,
    )
    .bind(&recipient_email)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (_mailbox_id, tenant_id, _assigned_user_id) = match mailbox {
        Some(m) => m,
        None => {
            // No matching mailbox — try catch-all domain routing
            let domain_part = recipient_email.split('@').nth(1).unwrap_or("");
            let catch_all = sqlx::query_as::<_, (Uuid,)>(
                r#"
                SELECT id FROM private_email_domains
                WHERE domain = $1 AND catch_all_enabled = true
                LIMIT 1
                "#,
            )
            .bind(domain_part)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;

            match catch_all {
                Some((_domain_id,)) => {
                    return Ok(Json(json!({
                        "received": true,
                        "routed": "catch_all",
                        "note": "No specific mailbox found, routed via catch-all"
                    })));
                }
                None => {
                    return Ok(Json(json!({"received": false, "error": "no matching mailbox"})));
                }
            }
        }
    };

    let body = if !payload.stripped_text.is_empty() {
        &payload.stripped_text
    } else if !payload.body_plain.is_empty() {
        &payload.body_plain
    } else {
        &payload.body_html
    };

    // Find or create contact by sender email
    let contact_id = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&sender_email)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    {
        Some((id,)) => id,
        None => {
            // Auto-create contact from inbound email
            let name = sender_email.split('@').next().unwrap_or(&sender_email);
            let new_id = Uuid::new_v4();
            sqlx::query(
                r#"
                INSERT INTO contacts (id, tenant_id, email, name, source, created_at, updated_at)
                VALUES ($1, $2, $3, $4, 'inbound_email', NOW(), NOW())
                ON CONFLICT (tenant_id, email) DO UPDATE SET updated_at = NOW()
                "#,
            )
            .bind(new_id)
            .bind(tenant_id)
            .bind(&sender_email)
            .bind(name)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;
            new_id
        }
    };

    // Create event for inbound email
    let event_payload = serde_json::json!({
        "from": sender_email,
        "to": recipient_email,
        "subject": payload.subject,
        "body_preview": &body[..body.len().min(500)],
        "message_id": payload.message_id,
        "in_reply_to": payload.in_reply_to
    });
    sqlx::query(
        r#"
        INSERT INTO events (id, tenant_id, source, event_type, entity_type, entity_id, payload, created_at)
        VALUES (gen_random_uuid(), $1, 'private_email', 'email_received', 'contact', $2, $3, NOW())
        "#,
    )
    .bind(tenant_id)
    .bind(contact_id)
    .bind(&event_payload)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({
        "received": true,
        "from": sender_email,
        "to": recipient_email,
        "subject": payload.subject,
    })))
}

fn extract_email(raw: &str) -> String {
    // Handle "Name <email>" format
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_lowercase();
        }
    }
    raw.trim().to_lowercase()
}
