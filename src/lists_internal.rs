//! Internal list member operations — no JWT, validated by x-internal-key
use axum::{extract::{Path, State}, http::HeaderMap, Json};
use axum::response::IntoResponse;
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{ApiResult, AppError};

pub async fn internal_add_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(list_id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    let expected = s.config.internal_sync_key.clone();
    if key != expected {
        return Err(AppError::Unauthorized);
    }

    let tenant_id = req.get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tenant_id required".into()))?;

    let contact_id = req.get("contact_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("contact_id required".into()))?;

    sqlx::query(
        "INSERT INTO list_members(id, list_id, contact_id, tenant_id, added_manually) VALUES($1, $2, $3, $4, true) ON CONFLICT (list_id, contact_id) DO NOTHING"
    )
    .bind(Uuid::new_v4())
    .bind(list_id)
    .bind(contact_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;

    Ok((StatusCode::CREATED, Json(json!({"status": "added", "list_id": list_id.to_string(), "contact_id": contact_id.to_string()}))))
}

pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/:list_id/members", post(internal_add_member))
}
