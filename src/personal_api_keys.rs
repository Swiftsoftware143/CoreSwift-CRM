//! Personal API Keys — per-user keys for third-party apps (Zapier-style).
//! A CoreSwift user generates a key, pastes it into the third-party app
//! (e.g. IncentiveSwift), and that app pushes data into THIS user's tenant.

use axum::{
    extract::{Extension, Path, State},
    middleware,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

fn key_hash(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

fn generate_key() -> String {
    // Format: csk_<128-bit><128-bit> hex (CoreSwift Key), readable + high entropy
    format!("csk_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[derive(Deserialize)]
pub struct CreateKeyRequest {
    pub name: Option<String>,
}

/// POST /api/personal-api-keys — generate a new key
pub async fn create_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<CreateKeyRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&c.sub).ok();

    let full_key = generate_key();
    let hash = key_hash(&full_key);
    let prefix = full_key.get(0..12).unwrap_or("csk_").to_string();
    let name = req.name.unwrap_or_else(|| "default".to_string());

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO personal_api_keys (id, tenant_id, user_id, name, key_hash, key_prefix)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&name)
    .bind(&hash)
    .bind(&prefix)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;

    // Return the FULL key ONCE — it can never be retrieved again.
    Ok(Json(json!({
        "id": id.to_string(),
        "name": name,
        "key": full_key,
        "prefix": prefix,
        "note": "Store this key now — it is shown only once."
    })))
}

/// GET /api/personal-api-keys — list keys (masked)
pub async fn list_keys(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        "SELECT id, name, key_prefix, is_active, last_used_at, created_at
         FROM personal_api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "name": r.get::<String, _>("name"),
                "prefix": r.get::<String, _>("key_prefix"),
                "is_active": r.get::<bool, _>("is_active"),
                "last_used_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_used_at"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            })
        })
        .collect();

    Ok(Json(json!({ "keys": keys })))
}

/// DELETE /api/personal-api-keys/:id — revoke a key
pub async fn revoke_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM personal_api_keys WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&s.db)
        .await
        .map_err(AppError::Database)?;
    Ok(Json(json!({ "revoked": true })))
}

pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::routing::{delete, get};
    axum::Router::new()
        .route("/", get(list_keys).post(create_key))
        .route("/:id", delete(revoke_key))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
