use axum::{extract::{Path, State}, Extension, Json};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use super::encryption;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct ApiKeyRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub label: String,
    pub provider: String,
    pub api_key_encrypted: String,
}

#[derive(Debug, Deserialize)]
pub struct AddApiKeyRequest {
    pub label: String,
    pub api_key: String,
    #[serde(default = "default_provider")]
    pub provider: String,
}

fn default_provider() -> String { "mailgun".into() }

pub async fn list_api_keys(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query_as::<_, ApiKeyRow>(
        "SELECT id, tenant_id, label, provider, api_key_encrypted FROM private_email_api_keys WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Return without the encrypted key — just metadata
    let safe: Vec<serde_json::Value> = rows.iter().map(|r| serde_json::json!({
        "id": r.id,
        "label": r.label,
        "provider": r.provider,
    })).collect();

    Ok(Json(serde_json::json!(safe)))
}

pub async fn add_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddApiKeyRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let encrypted = encryption::encrypt_api_key(account_id, &req.api_key)
        .map_err(AppError::Internal)?;

    let row = sqlx::query_as::<_, ApiKeyRow>(
        r#"
        INSERT INTO private_email_api_keys (tenant_id, label, provider, api_key_encrypted)
        VALUES ($1, $2, $3, $4)
        RETURNING id, tenant_id, label, provider, api_key_encrypted
        "#,
    )
    .bind(account_id)
    .bind(&req.label)
    .bind(&req.provider)
    .bind(&encrypted)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::json!({
        "id": row.id,
        "label": row.label,
        "provider": row.provider,
        "created": true,
    })))
}

pub async fn delete_api_key(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(key_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "DELETE FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2"
    )
    .bind(key_id)
    .bind(account_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("API key not found".into()));
    }

    Ok(Json(serde_json::json!({"deleted": true})))
}
