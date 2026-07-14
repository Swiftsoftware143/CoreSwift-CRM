//! Internal contact creation — no JWT, validated by x-internal-key
use axum::{extract::State, http::HeaderMap, Json};
use axum::response::IntoResponse;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{ApiResult, AppError};

pub async fn internal_create(
    State(s): State<AppState>,
    headers: HeaderMap,
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

    let first_name = req.get("first_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let last_name = req.get("last_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let email = req.get("email").and_then(|v| v.as_str()).map(|s| s.to_string());
    let phone = req.get("phone").and_then(|v| v.as_str()).map(|s| s.to_string());
    let company_id = req.get("company_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let notes = req.get("notes").and_then(|v| v.as_str()).map(|s| s.to_string());
    let title = req.get("title").and_then(|v| v.as_str()).map(|s| s.to_string());

    if first_name.is_empty() {
        return Err(AppError::BadRequest("first_name is required".into()));
    }

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, company_id, notes, title) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&first_name)
    .bind(&last_name)
    .bind(&email)
    .bind(&phone)
    .bind(company_id)
    .bind(&notes)
    .bind(&title)
    .execute(&s.db)
    .await?;

    Ok(Json(serde_json::json!({"id": id.to_string(), "first_name": first_name, "last_name": last_name})))
}

/// Router for internal contact endpoints (no auth middleware)
pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/", post(internal_create))
}
