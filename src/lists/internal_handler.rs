use axum::{extract::{Path, State}, http::HeaderMap, Json};
use axum::response::{IntoResponse, StatusCode};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{ApiResult, AppError};
use crate::lists::models::ListMember;

/// Internal: add member to list — no JWT, validated by x-internal-key
pub async fn internal_add_member(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(list_id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key != s.config.internal_sync_key {
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

    let m = sqlx::query_as::<_, ListMember>(
        "INSERT INTO list_members(id, list_id, contact_id, tenant_id, added_manually) VALUES($1, $2, $3, $4, true) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(list_id)
    .bind(contact_id)
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref d) = e {
            if d.constraint() == Some("list_members_list_id_contact_id_key") {
                return AppError::Duplicate("Already a member".into());
            }
        }
        AppError::Database(e)
    })?;

    Ok((StatusCode::CREATED, Json(json!(m))))
}
