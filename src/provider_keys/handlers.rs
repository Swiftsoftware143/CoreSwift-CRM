//! Provider Keys handlers — CRUD for provider_key records, read-only for available_providers.
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

fn mask_key(key: &str) -> String {
    if key.len() <= 6 {
        return "***".to_string();
    }
    let prefix = &key[..3];
    let suffix = &key[key.len() - 3..];
    format!("{}...{}", prefix, suffix)
}

pub async fn list_available_providers(State(s): State<AppState>) -> ApiResult<impl IntoResponse> {
    let rows = sqlx::query(
        "SELECT key, name, description, requires_base_url, requires_metadata, icon FROM available_providers ORDER BY name"
    ).fetch_all(&s.db).await?;
    let providers: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "key": row.get::<String,_>("key"),
                "name": row.get::<String,_>("name"),
                "description": row.get::<Option<String>,_>("description"),
                "requires_base_url": row.get::<bool,_>("requires_base_url"),
                "requires_metadata": row.get::<Value,_>("requires_metadata"),
                "icon": row.get::<Option<String>,_>("icon"),
            })
        })
        .collect();
    Ok(Json(
        json!({ "count": providers.len(), "items": providers }),
    ))
}

pub async fn list_provider_keys(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        "SELECT pk.id, pk.tenant_id, pk.provider, pk.api_key, pk.base_url, pk.metadata, pk.is_active, pk.scope, pk.created_at, pk.updated_at, ap.name AS provider_name, ap.icon AS provider_icon FROM provider_keys pk LEFT JOIN available_providers ap ON ap.key = pk.provider WHERE pk.tenant_id = $1 ORDER BY pk.provider"
    ).bind(tenant_id).fetch_all(&s.db).await?;
    let keys: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid,_>("id"),
                "tenant_id": row.get::<Uuid,_>("tenant_id"),
                "provider": row.get::<String,_>("provider"),
                "api_key_masked": mask_key(&crate::secret_box::open(row.get::<Uuid,_>("tenant_id"), &row.get::<String,_>("api_key"))),
                "base_url": row.get::<Option<String>,_>("base_url"),
                "metadata": row.get::<Value,_>("metadata"),
                "is_active": row.get::<bool,_>("is_active"),
                "scope": row.get::<String,_>("scope"),
                "provider_name": row.get::<Option<String>,_>("provider_name"),
                "provider_icon": row.get::<Option<String>,_>("provider_icon"),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "count": keys.len(), "items": keys })))
}

pub async fn get_provider_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query(
        "SELECT pk.id, pk.tenant_id, pk.provider, pk.api_key, pk.base_url, pk.metadata, pk.is_active, pk.scope, pk.created_at, pk.updated_at, ap.name AS provider_name, ap.icon AS provider_icon FROM provider_keys pk LEFT JOIN available_providers ap ON ap.key = pk.provider WHERE pk.tenant_id = $1 AND pk.provider = $2"
    ).bind(tenant_id).bind(&provider).fetch_optional(&s.db).await?.ok_or_else(|| AppError::NotFound(format!("Provider key '{}' not found", provider)))?;
    Ok(Json(json!({
        "id": row.get::<Uuid,_>("id"),
        "tenant_id": row.get::<Uuid,_>("tenant_id"),
        "provider": row.get::<String,_>("provider"),
        "api_key_masked": mask_key(&crate::secret_box::open(row.get::<Uuid,_>("tenant_id"), &row.get::<String,_>("api_key"))),
        "base_url": row.get::<Option<String>,_>("base_url"),
        "metadata": row.get::<Value,_>("metadata"),
        "is_active": row.get::<bool,_>("is_active"),
        "scope": row.get::<String,_>("scope"),
        "provider_name": row.get::<Option<String>,_>("provider_name"),
        "provider_icon": row.get::<Option<String>,_>("provider_icon"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
    })))
}

#[derive(serde::Deserialize, Debug)]
pub struct UpsertProviderKeyRequest {
    pub provider: String,
    pub api_key: String,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
    pub is_active: Option<bool>,
    pub scope: Option<String>,
}

pub async fn upsert_provider_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<UpsertProviderKeyRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if req.api_key.is_empty() {
        return Err(AppError::Validation("api_key is required".into()));
    }
    let provider_exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM available_providers WHERE key = $1")
            .bind(&req.provider)
            .fetch_one(&s.db)
            .await?;
    if provider_exists == 0 {
        return Err(AppError::Validation(format!(
            "Unknown provider '{}'. Available providers can be listed at /available-providers",
            req.provider
        )));
    }
    let metadata = req.metadata.unwrap_or(json!({}));
    let scope = req.scope.unwrap_or_else(|| "tenant".to_string());

    // A client that round-trips the value it was shown would otherwise overwrite the real secret with
    // the MASK. If the submitted value is exactly the mask of what is already stored, keep the stored
    // secret and only update the rest of the row.
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT api_key FROM provider_keys WHERE tenant_id = $1 AND provider = $2",
    )
    .bind(tenant_id)
    .bind(&req.provider)
    .fetch_optional(&s.db)
    .await?;
    let submitted_is_mask = existing
        .as_deref()
        .map(|cur| {
            let cur_plain = crate::secret_box::open(tenant_id, cur);
            !cur_plain.is_empty() && mask_key(&cur_plain) == req.api_key
        })
        .unwrap_or(false);
    // Encrypt at rest (CS-21). Reads go through `secret_box::open`, which also understands a legacy
    // plaintext row, so this is safe on a database that has not been backfilled yet.
    let stored_secret = if submitted_is_mask {
        existing.clone().unwrap_or_default()
    } else {
        crate::secret_box::seal(tenant_id, &req.api_key)?
    };
    let row = sqlx::query(
        "INSERT INTO provider_keys (tenant_id, provider, api_key, base_url, metadata, is_active, scope) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tenant_id, provider) DO UPDATE SET api_key = EXCLUDED.api_key, base_url = COALESCE(EXCLUDED.base_url, provider_keys.base_url), metadata = CASE WHEN EXCLUDED.metadata = '{}'::jsonb THEN provider_keys.metadata ELSE EXCLUDED.metadata END, is_active = COALESCE(EXCLUDED.is_active, provider_keys.is_active), scope = EXCLUDED.scope, updated_at = NOW() RETURNING id, tenant_id, provider, api_key, base_url, metadata, is_active, scope, created_at, updated_at"
    ).bind(tenant_id).bind(&req.provider).bind(&stored_secret).bind(&req.base_url).bind(&metadata).bind(req.is_active.unwrap_or(true)).bind(&scope).fetch_one(&s.db).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": row.get::<Uuid,_>("id"),
            "tenant_id": row.get::<Uuid,_>("tenant_id"),
            "provider": row.get::<String,_>("provider"),
            "api_key_masked": mask_key(&crate::secret_box::open(row.get::<Uuid,_>("tenant_id"), &row.get::<String,_>("api_key"))),
            "base_url": row.get::<Option<String>,_>("base_url"),
            "metadata": row.get::<Value,_>("metadata"),
            "is_active": row.get::<bool,_>("is_active"),
            "scope": row.get::<String,_>("scope"),
            "created_at": row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            "updated_at": row.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
        })),
    ))
}

#[derive(serde::Deserialize, Debug)]
pub struct UpdateProviderKeyRequest {
    pub is_active: Option<bool>,
    pub base_url: Option<String>,
    pub metadata: Option<Value>,
}

/// PATCH /provider-keys/:provider — enable/disable (or re-point) a stored key WITHOUT
/// resending the secret.
///
/// GET only ever returns `api_key_masked`, so a UI toggle has no way to round-trip the
/// real secret, and the only other write path is the upsert — which rejects an empty
/// `api_key` by design. That left "toggle this connection" impossible to implement
/// from any client, so the Integration Center could show a key but never switch it off.
pub async fn update_provider_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(provider): Path<String>,
    Json(req): Json<UpdateProviderKeyRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let row = sqlx::query(
        "UPDATE provider_keys SET is_active = COALESCE($1, is_active), base_url = COALESCE($2, base_url), metadata = COALESCE($3, metadata), updated_at = NOW() WHERE tenant_id = $4 AND provider = $5 RETURNING id, tenant_id, provider, api_key, base_url, metadata, is_active, scope, created_at, updated_at"
    )
    .bind(req.is_active)
    .bind(req.base_url.as_deref())
    .bind(req.metadata.as_ref())
    .bind(tenant_id)
    .bind(&provider)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Provider key '{}' not found", provider)))?;

    Ok(Json(json!({
        "id": row.get::<Uuid,_>("id"),
        "tenant_id": row.get::<Uuid,_>("tenant_id"),
        "provider": row.get::<String,_>("provider"),
        "api_key_masked": mask_key(&crate::secret_box::open(row.get::<Uuid,_>("tenant_id"), &row.get::<String,_>("api_key"))),
        "base_url": row.get::<Option<String>,_>("base_url"),
        "metadata": row.get::<Value,_>("metadata"),
        "is_active": row.get::<bool,_>("is_active"),
        "scope": row.get::<String,_>("scope"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
    })))
}

pub async fn delete_provider_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(provider): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let result = sqlx::query("DELETE FROM provider_keys WHERE tenant_id = $1 AND provider = $2")
        .bind(tenant_id)
        .bind(&provider)
        .execute(&s.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Provider key '{}' not found",
            provider
        )));
    }
    Ok(Json(json!({ "deleted": true, "provider": provider })))
}
