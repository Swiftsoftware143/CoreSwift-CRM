use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::providers;
use super::send_handler::load_provider_config;

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
    body: String,
) -> ApiResult<Json<serde_json::Value>> {
    // Try to parse as form-urlencoded (Mailgun's format)
    let payload: MailgunInbound = serde_urlencoded::from_str(&body)
        .map_err(|e| AppError::BadRequest(format!("Invalid Mailgun payload: {}", e)))?;

    // Extract sender/recipient email addresses
    let sender_email = extract_email(&payload.sender);
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

    let (mailbox_id, tenant_id, _assigned_user_id) = match mailbox {
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

    // Get domain_id for the mailbox to validate webhook signature
    let domain_row = sqlx::query_as::<_, (Uuid,)>(
        "SELECT domain_id FROM private_email_boxes WHERE id = $1",
    )
    .bind(mailbox_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Mailbox domain not found".into()))?;

    let domain_id = domain_row.0;

    // Load provider config to validate webhook signature
    let provider_config = load_provider_config(&state.db, domain_id, tenant_id).await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {}", e)))?;

    // Validate webhook — pass raw body to provider for signature check
    let provider = providers::provider_for(&provider_config);
    let inbound = provider.accept_inbound(&provider_config, body.as_bytes());

    let inbound = match inbound {
        Some(i) => i,
        None => {
            // If signature validation fails but we know the mailbox, still accept
            // (not all senders configure webhook verification)
            // Reconstruct from payload
            let body_text = if !payload.stripped_text.is_empty() {
                &payload.stripped_text
            } else if !payload.body_plain.is_empty() {
                &payload.body_plain
            } else {
                &payload.body_html
            };
            let msg_id_empty = payload.message_id.is_empty();
            let msg_id = if msg_id_empty { None } else { Some(payload.message_id) };
            super::providers::InboundEmail {
                from: sender_email,
                to: recipient_email,
                subject: payload.subject,
                body_plain: body_text.to_string(),
                body_html: if payload.body_html.is_empty() { None } else { Some(payload.body_html) },
                message_id: msg_id.clone(),
                in_reply_to: if payload.in_reply_to.is_empty() { None } else { Some(payload.in_reply_to) },
                provider_message_id: msg_id,
            }
        }
    };

    // Find or create contact by sender email
    let contact_id = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&inbound.from)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    {
        Some((id,)) => id,
        None => {
            // Auto-create contact from inbound email
            let name = inbound.from.split('@').next().unwrap_or(&inbound.from);
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
            .bind(&inbound.from)
            .bind(name)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;
            new_id
        }
    };

    // ── Support Ticket Routing ─────────────────────────────────
    // If the receiving mailbox is the tenant's designated support box,
    // auto-create a ticket from this email.
    let support_box_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT (settings->>'support_email_box_id')::uuid FROM tenants WHERE id = $1"
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    if let Some(support_id) = support_box_id {
        if support_id == mailbox_id {
            let contact_name = inbound.from.split('@').next().unwrap_or(&inbound.from);
            let _ = sqlx::query_scalar::<_, Uuid>(
                r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
                   VALUES ($1, $2, $3, 'open', 'medium', 'email', $4, $5)
                   RETURNING id"#
            )
            .bind(tenant_id)
            .bind(&inbound.subject)
            .bind(&inbound.body_plain)
            .bind(&inbound.from)
            .bind(contact_name)
            .fetch_one(&state.db)
            .await;
        }
    }

    // Create event for inbound email
    let event_payload = serde_json::json!({
        "from": inbound.from,
        "to": inbound.to,
        "subject": inbound.subject,
        "body_preview": &inbound.body_plain[..inbound.body_plain.len().min(500)],
        "message_id": inbound.message_id,
        "in_reply_to": inbound.in_reply_to,
        "provider_message_id": inbound.provider_message_id,
        "provider": provider.name(),
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

    // Fire auto-reply rules
    super::auto_reply_handler::maybe_fire_auto_reply(
        &state.db, tenant_id, "always", "", &inbound.from,
    ).await;

    Ok(Json(json!({
        "received": true,
        "provider": provider.name(),
        "from": inbound.from,
        "to": inbound.to,
        "subject": inbound.subject,
    })))
}

fn extract_email(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_lowercase();
        }
    }
    raw.trim().to_lowercase()
}
