//! Internal tenant lookup — no JWT, validated by x-internal-key
use axum::{extract::State, http::HeaderMap, Json};
use axum::response::IntoResponse;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{ApiResult, AppError};

pub async fn internal_lookup_tenant(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    let expected = s.config.internal_sync_key.clone();
    if key != expected {
        return Err(AppError::Unauthorized);
    }

    let slug = req.get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if slug.is_empty() {
        return Err(AppError::BadRequest("slug is required".into()));
    }

    let tenant = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, name, slug FROM tenants WHERE slug = $1"
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?;

    match tenant {
        Some((id, name, slug)) => Ok(Json(json!({
            "id": id.to_string(),
            "name": name,
            "slug": slug,
        }))),
        None => Err(AppError::NotFound(format!("Tenant with slug '{slug}' not found")))
    }
}

pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/lookup", post(internal_lookup_tenant))
}
