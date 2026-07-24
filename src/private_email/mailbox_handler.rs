use axum::{extract::{Path, State}, Extension, Json};
use serde_json::json;
use uuid::Uuid;

use super::encryption;
use super::feature_gate;
use super::models::*;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn provision_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ProvisionMailboxRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    feature_gate::check_mailbox_limit(&state.db, account_id).await?;

    // Get the domain
    let domain = sqlx::query_as::<_, PrivateEmailDomain>(
        "SELECT * FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(req.domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let domain = domain.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    // Check for duplicate
    let email_address = format!("{}@{}", req.local_part, domain.domain);
    let existing = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 AND email_address = $2",
    )
    .bind(account_id)
    .bind(&email_address)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    if existing.is_some() {
        return Err(AppError::BadRequest("Email address already exists".into()));
    }

    // Create mailbox in Mailgun
    let api_key = encryption::decrypt_api_key(account_id, &domain.mailgun_api_key)
        .map_err(AppError::Internal)?;

    let base_url = if domain.mailgun_region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };

    // Create the mailbox via Mailgun API
    let mailgun_id = create_mailgun_mailbox(base_url, &api_key, &domain.domain, &req.local_part).await
        .map_err(AppError::Internal)?;

    // Create route for inbound forwarding
    let _route_id = create_mailgun_route(base_url, &api_key, &domain.domain).await
        .map_err(AppError::Internal)?;

    let row = sqlx::query_as::<_, PrivateEmailBox>(
        r#"
        INSERT INTO private_email_boxes (tenant_id, domain_id, user_id, local_part, email_address, mailgun_mailbox_id)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(req.domain_id)
    .bind(req.user_id)
    .bind(&req.local_part)
    .bind(&email_address)
    .bind(&mailgun_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}

pub async fn list_mailboxes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let boxes = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&boxes).unwrap()))
}

pub async fn delete_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(box_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let mailbox = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE id = $1 AND tenant_id = $2",
    )
    .bind(box_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let mailbox = mailbox.ok_or_else(|| AppError::NotFound("Mailbox not found".into()))?;

    // Soft-deactivate
    sqlx::query(
        "UPDATE private_email_boxes SET status = 'deprovisioned', updated_at = NOW() WHERE id = $1",
    )
    .bind(box_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({"deleted": true, "email": mailbox.email_address})))
}

pub async fn update_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(box_id): Path<Uuid>,
    Json(req): Json<UpdateMailboxRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query_as::<_, PrivateEmailBox>(
        r#"
        UPDATE private_email_boxes
        SET signature = COALESCE($3, signature),
            forwarding_enabled = COALESCE($4, forwarding_enabled),
            updated_at = NOW()
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(box_id)
    .bind(account_id)
    .bind(&req.signature)
    .bind(req.forwarding_enabled)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(mb) => Ok(Json(serde_json::to_value(&mb).unwrap())),
        None => Err(AppError::NotFound("Mailbox not found".into())),
    }
}

async fn create_mailgun_mailbox(
    base_url: &str,
    api_key: &str,
    domain: &str,
    local_part: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v3/{}/addresses", base_url, domain))
        .basic_auth("api", Some(api_key))
        .form(&[("address", format!("{}@{}", local_part, domain))])
        .send()
        .await
        .map_err(|e| format!("Mailgun API error: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Mailgun mailbox creation failed: {}", body));
    }

    Ok(format!("{}@{}", local_part, domain))
}

async fn create_mailgun_route(
    base_url: &str,
    api_key: &str,
    domain: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let webhook_url = "https://coreswift.net/api/v1/webhooks/mailgun/inbound";

    let resp = client
        .post(format!("{}/v3/routes", base_url))
        .basic_auth("api", Some(api_key))
        .form(&[
            ("priority", "10"),
            ("description", &format!("CoreSwift inbound for {}", domain)),
            ("expression", &format!("match_recipient('.*@{}')", domain)),
            ("action", &format!("forward(\"{}\")", webhook_url)),
        ])
        .send()
        .await
        .map_err(|e| format!("Mailgun route creation error: {}", e))?;

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Mailgun route creation failed: {}", body));
    }

    Ok("route-created".into())
}
