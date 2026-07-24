use axum::{extract::{Path, State}, Extension, Json};
use serde_json::json;
use uuid::Uuid;

use super::encryption;
use super::feature_gate;
use super::models::*;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn add_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddDomainRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    feature_gate::check_domain_limit(&state.db, account_id).await?;

    // Resolve API key: from existing saved key, or encrypt a new one
    let (encrypted_key, api_key_id): (String, Option<Uuid>) = if let Some(kid) = req.api_key_id {
        // Use existing saved key
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT api_key_encrypted FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2"
        )
        .bind(kid)
        .bind(account_id)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?
        .ok_or_else(|| AppError::NotFound("Saved API key not found".into()))?;
        (row.0, Some(kid))
    } else if let Some(ref raw_key) = req.mailgun_api_key {
        // Encrypt and optionally save as named key
        let encrypted = encryption::encrypt_api_key(account_id, raw_key)
            .map_err(AppError::Internal)?;
        let label = req.label.clone().unwrap_or_else(|| req.domain.clone());
        // Save as a named key for future reuse
        let kid = sqlx::query_as::<_, (Uuid,)>(
            r#"
            INSERT INTO private_email_api_keys (tenant_id, label, provider, api_key_encrypted)
            VALUES ($1, $2, 'mailgun', $3)
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(account_id)
        .bind(&label)
        .bind(&encrypted)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?;
        (encrypted, kid.map(|(id,)| id))
    } else {
        return Err(AppError::BadRequest("Either mailgun_api_key or api_key_id is required".into()));
    };

    // Decrypt to validate
    let raw_key = encryption::decrypt_api_key(account_id, &encrypted_key)
        .map_err(AppError::Internal)?;

    // Validate Mailgun API key by checking the domain exists
    let key_valid = validate_mailgun_domain(&raw_key, &req.domain, &req.mailgun_region).await;
    if !key_valid {
        return Err(AppError::BadRequest("Invalid Mailgun API key or domain not configured in Mailgun".into()));
    }

    let label = req.label.unwrap_or_else(|| req.domain.clone());
    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        INSERT INTO private_email_domains (tenant_id, domain, mailgun_api_key, mailgun_region, label, api_key_id)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(&req.domain)
    .bind(&encrypted_key)
    .bind(&req.mailgun_region)
    .bind(&label)
    .bind(api_key_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}

pub async fn list_domains(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let domains = sqlx::query_as::<_, PrivateEmailDomain>(
        "SELECT * FROM private_email_domains WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&domains).unwrap()))
}

pub async fn delete_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "DELETE FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(domain_id)
    .bind(account_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Domain not found".into()));
    }

    Ok(Json(json!({"deleted": true})))
}

pub async fn update_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
    Json(req): Json<UpdateDomainRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    if let Some(catch_all) = req.catch_all_enabled {
        if catch_all {
            let allowed = feature_gate::can_enable_catch_all(&state.db, account_id).await?;
            if !allowed {
                return Err(AppError::BadRequest("Catch-all requires Pro plan or higher".into()));
            }
        }
    }

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        UPDATE private_email_domains
        SET catch_all_enabled = COALESCE($3, catch_all_enabled),
            updated_at = NOW()
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(domain_id)
    .bind(account_id)
    .bind(req.catch_all_enabled)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(domain) => Ok(Json(serde_json::to_value(&domain).unwrap())),
        None => Err(AppError::NotFound("Domain not found".into())),
    }
}

/// Validate a Mailgun API key by calling GET /v3/domains/{domain}
async fn validate_mailgun_domain(api_key: &str, domain: &str, region: &str) -> bool {
    let base_url = if region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };

    let client = reqwest::Client::new();
    match client
        .get(format!("{}/v3/domains/{}", base_url, domain))
        .basic_auth("api", Some(api_key))
        .send()
        .await
    {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}
