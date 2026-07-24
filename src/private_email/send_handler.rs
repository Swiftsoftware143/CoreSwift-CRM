use axum::{extract::State, Extension, Json};
use uuid::Uuid;

use super::encryption;
use super::models::*;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn send_email(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendEmailRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;
    let _user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| AppError::Unauthorized)?;

    // Find the sending mailbox
    let mailbox = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 AND email_address = $2 AND status = 'active'",
    )
    .bind(account_id)
    .bind(&req.from_address)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let mailbox = mailbox.ok_or_else(|| AppError::NotFound("Sending mailbox not found or not active".into()))?;

    // Get domain with decrypted API key
    let domain_row = sqlx::query_as::<_, PrivateEmailDomain>(
        "SELECT * FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(mailbox.domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let domain_row = domain_row.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    let api_key = encryption::decrypt_api_key(account_id, &domain_row.mailgun_api_key)
        .map_err(AppError::Internal)?;

    let base_url = if domain_row.mailgun_region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };

    // Build body with optional signature
    let body = if let Some(ref sig) = mailbox.signature {
        format!("{}\n\n--\n{}", req.body, sig)
    } else {
        req.body.clone()
    };

    // Send via Mailgun
    let client = reqwest::Client::new();
    let mut form: Vec<(String, String)> = vec![
        ("from".into(), req.from_address.clone()),
        ("to".into(), req.to.clone()),
        ("subject".into(), req.subject.clone()),
        ("html".into(), body),
    ];

    if let Some(ref in_reply_to) = req.in_reply_to {
        form.push(("h:In-Reply-To".into(), in_reply_to.clone()));
    }

    let resp = client
        .post(format!("{}/v3/{}/messages", base_url, domain_row.domain))
        .basic_auth("api", Some(&api_key))
        .form(&form)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Mailgun send error: {}", e)))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(AppError::Internal(format!("Mailgun send failed: {}", body)));
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
        "from": req.from_address,
        "to": req.to,
        "subject": req.subject,
    })))
}

/// Low-level send via Mailgun — used by auto-reply engine.
/// Takes decrypted API key directly (no DB lookups).
pub async fn send_via_mailgun(
    base_url: &str,
    api_key: &str,
    domain: &str,
    from_address: &str,
    to: &str,
    subject: &str,
    body_html: &str,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v3/{}/messages", base_url, domain))
        .basic_auth("api", Some(api_key))
        .form(&[
            ("from", from_address),
            ("to", to),
            ("subject", subject),
            ("html", body_html),
        ])
        .send()
        .await
        .map_err(|e| format!("Mailgun send error: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Mailgun send failed: {}", body));
    }
    Ok(())
}
