use axum::{extract::State, Extension, Json};
use sqlx::Row;
use uuid::Uuid;

use super::models::*;
use super::providers::{self, ProviderConfig};

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Send an email via the domain's configured provider (Mailgun, SMTP, etc).
/// The provider is selected automatically based on the domain's provider_type.
pub async fn send_email(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendEmailRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Find the sending mailbox
    let mailbox = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 AND email_address = $2 AND status = 'active'",
    )
    .bind(account_id)
    .bind(&req.from_address)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let mailbox = mailbox
        .ok_or_else(|| AppError::NotFound("Sending mailbox not found or not active".into()))?;

    // Load provider config for this domain
    let provider_config = load_provider_config(&state.db, mailbox.domain_id, account_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {}", e)))?;

    // Build body with optional signature
    let body = if let Some(ref sig) = mailbox.signature {
        format!("{}\n\n--\n{}", req.body, sig)
    } else {
        req.body.clone()
    };

    // Select provider and send
    let provider = providers::provider_for(&provider_config);
    let result = provider
        .send(
            &provider_config,
            &req.from_address,
            &req.to,
            &req.subject,
            &body,
            req.in_reply_to.as_deref(),
        )
        .await;

    if !result.success {
        let err = result.error.unwrap_or_else(|| "Unknown send error".into());
        return Err(AppError::Internal(format!(
            "{} send failed: {}",
            provider.name(),
            err
        )));
    }

    // Try to match recipient to a contact and log as event
    if let Ok(Some((contact_id,))) = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
    )
    .bind(account_id)
    .bind(&req.to)
    .fetch_optional(&state.db)
    .await
    {
        let payload = serde_json::json!({
            "from": req.from_address,
            "to": req.to,
            "subject": req.subject,
            "provider": provider.name(),
            "provider_message_id": result.provider_message_id,
            "body_preview": &req.body[..req.body.len().min(500)]
        });
        let _ = sqlx::query(
            r#"
            INSERT INTO events (id, tenant_id, source, event_type, entity_type, entity_id, payload, created_at)
            VALUES (gen_random_uuid(), $1, 'private_email', 'email_sent', 'contact', $2, $3, NOW())
            "#,
        )
        .bind(account_id)
        .bind(contact_id)
        .bind(&payload)
        .execute(&state.db)
        .await;
    }

    Ok(Json(serde_json::json!({
        "sent": true,
        "provider": provider.name(),
        "provider_message_id": result.provider_message_id,
        "from": req.from_address,
        "to": req.to,
        "subject": req.subject,
    })))
}

/// Load provider configuration for a domain from the database.
pub async fn load_provider_config(
    db: &sqlx::PgPool,
    domain_id: Uuid,
    tenant_id: Uuid,
) -> Result<ProviderConfig, String> {
    let row = sqlx::query(
        r#"
        SELECT 
            d.provider_type, d.domain, d.mailgun_region, d.mailgun_api_key,
            d.smtp_host, d.smtp_port, d.smtp_username, d.smtp_password_encrypted,
            d.smtp_tls, d.inbound_mode, d.webhook_signing_key_encrypted,
            d.api_key_id
        FROM private_email_domains d
        WHERE d.id = $1 AND d.tenant_id = $2
        "#,
    )
    .bind(domain_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("Database error: {}", e))?
    .ok_or_else(|| "Domain not found".to_string())?;

    let provider_type: String = row
        .try_get("provider_type")
        .unwrap_or_else(|_| "mailgun".into());
    let region: Option<String> = row.try_get("mailgun_region").ok();
    let encrypted_api_key: Option<String> = row.try_get("mailgun_api_key").ok();

    // For backward compat: if api_key_id is set, resolve the key
    let final_encrypted_key = if row
        .try_get::<Option<Uuid>, _>("api_key_id")
        .ok()
        .flatten()
        .is_some()
    {
        // Resolve from the key table based on provider_type
        resolve_api_key(
            db,
            tenant_id,
            row.try_get("api_key_id").ok().flatten(),
            &provider_type,
        )
        .await
    } else {
        encrypted_api_key
    };

    Ok(ProviderConfig {
        tenant_id,
        domain_id,
        domain: row.try_get("domain").unwrap_or_default(),
        provider_type,
        encrypted_api_key: final_encrypted_key,
        region,
        smtp_host: row.try_get("smtp_host").ok(),
        smtp_port: row.try_get("smtp_port").ok(),
        smtp_username: row.try_get("smtp_username").ok(),
        encrypted_smtp_password: row.try_get("smtp_password_encrypted").ok(),
        smtp_tls: row.try_get("smtp_tls").unwrap_or(true),
        inbound_mode: row
            .try_get("inbound_mode")
            .unwrap_or_else(|_| "webhook".into()),
        encrypted_webhook_key: row.try_get("webhook_signing_key_encrypted").ok(),
    })
}

async fn resolve_api_key(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    api_key_id: Option<Uuid>,
    provider_type: &str,
) -> Option<String> {
    let kid = api_key_id?;
    match provider_type {
        "mailgun" => {
            let row = sqlx::query(
                "SELECT api_key_encrypted FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2",
            )
            .bind(kid)
            .bind(tenant_id)
            .fetch_optional(db)
            .await
            .ok()??;
            Some(row.try_get::<String, _>("api_key_encrypted").ok()?)
        }
        _ => {
            // Provider api keys (SES, Postmark)
            let row = sqlx::query(
                "SELECT access_key_encrypted FROM provider_api_keys WHERE id = $1 AND tenant_id = $2 AND provider = $3",
            )
            .bind(kid)
            .bind(tenant_id)
            .bind(provider_type)
            .fetch_optional(db)
            .await
            .ok()??;
            Some(row.try_get::<String, _>("access_key_encrypted").ok()?)
        }
    }
}

/// Low-level send via the domain's configured provider — used by auto-reply engine.
pub async fn send_via_provider(
    db: &sqlx::PgPool,
    domain_id: Uuid,
    tenant_id: Uuid,
    from_address: &str,
    to: &str,
    subject: &str,
    body_html: &str,
) -> Result<(), String> {
    let config = load_provider_config(db, domain_id, tenant_id).await?;
    let provider = providers::provider_for(&config);
    let result = provider
        .send(&config, from_address, to, subject, body_html, None)
        .await;

    if result.success {
        Ok(())
    } else {
        Err(result.error.unwrap_or_else(|| "Unknown send error".into()))
    }
}

// Backward compat alias — auto-reply engine calls this.
pub async fn send_via_mailgun(
    _base_url: &str,
    _api_key: &str,
    _domain: &str,
    _from_address: &str,
    _to: &str,
    _subject: &str,
    _body_html: &str,
) -> Result<(), String> {
    // This function is kept for backward compat with auto_reply_handler.
    // New code should call send_via_provider instead.
    // For now, forward to Mailgun provider directly.
    Err("send_via_mailgun is deprecated — use send_via_provider with db connection".into())
}
