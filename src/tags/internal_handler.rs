//! Internal tag operations — no JWT, validated by x-internal-key
use axum::{extract::State, http::HeaderMap, Json};
use axum::response::IntoResponse;
use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{ApiResult, AppError};

pub async fn internal_create_tag(
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

    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let color = req.get("color").and_then(|v| v.as_str()).unwrap_or("#6366f1").to_string();
    let category_id: Option<Uuid> = req.get("category_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());

    // Check if tag already exists
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM tags WHERE tenant_id = $1 AND name = $2"
    )
    .bind(tenant_id)
    .bind(&name)
    .fetch_optional(&s.db)
    .await?;

    if let Some((existing_id,)) = existing {
        // Update color
        let _ = sqlx::query("UPDATE tags SET color = $1, updated_at = NOW() WHERE id = $2")
            .bind(&color)
            .bind(existing_id)
            .execute(&s.db)
            .await;
        return Ok((StatusCode::OK, Json(json!({
            "status": "exists",
            "id": existing_id.to_string(),
            "name": name
        }))));
    }

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tags (id, tenant_id, category_id, name, color, is_active) VALUES ($1, $2, $3, $4, $5, true)"
    )
    .bind(id)
    .bind(tenant_id)
    .bind(category_id)
    .bind(&name)
    .bind(&color)
    .execute(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({
        "status": "created",
        "id": id.to_string(),
        "name": name,
        "color": color
    }))))
}

pub async fn internal_list_tags(
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

    let tags: Vec<serde_json::Value> = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>)>(
        "SELECT id, name, color, description FROM tags WHERE tenant_id = $1 AND is_active = true ORDER BY name"
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?
    .into_iter()
    .map(|(id, name, color, description)| {
        json!({"id": id.to_string(), "name": name, "color": color.unwrap_or_else(|| "#6366f1".into()), "description": description})
    })
    .collect();

    Ok(Json(json!({"tags": tags})))
}

pub async fn internal_assign_tag(
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

    let tag_id = req.get("tag_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tag_id required".into()))?;

    let entity_id = req.get("entity_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("entity_id required".into()))?;

    let entity_type = req.get("entity_type")
        .and_then(|v| v.as_str())
        .unwrap_or("contact")
        .to_string();

    // Verify tag exists in this tenant
    let tag_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tags WHERE id = $1 AND tenant_id = $2"
    )
    .bind(tag_id)
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or(0);

    if tag_exists == 0 {
        return Err(AppError::NotFound(format!("Tag {} not found in tenant", tag_id)));
    }

    // assigned_by is nullable FK to users — use None for system operations
    sqlx::query(
        "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id, assigned_by) VALUES ($1, $2, $3, $4, $5, NULL) ON CONFLICT (tag_id, entity_type, entity_id) DO NOTHING"
    )
    .bind(Uuid::new_v4())
    .bind(tag_id)
    .bind(&entity_type)
    .bind(entity_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "status": "assigned",
        "tag_id": tag_id.to_string(),
        "entity_type": entity_type,
        "entity_id": entity_id.to_string()
    })))
}

pub async fn internal_delete_tag(
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

    let tag_id = req.get("tag_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tag_id required".into()))?;

    // Verify tag exists
    let tag_exists = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tags WHERE id = $1 AND tenant_id = $2"
    )
    .bind(tag_id)
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or(0);

    if tag_exists == 0 {
        return Err(AppError::NotFound(format!("Tag {} not found", tag_id)));
    }

    // Cascade handled by FK: ON DELETE CASCADE
    let _ = sqlx::query("DELETE FROM tag_assignments WHERE tag_id = $1")
        .bind(tag_id)
        .execute(&s.db)
        .await;

    let result = sqlx::query("DELETE FROM tags WHERE id = $1 AND tenant_id = $2")
        .bind(tag_id)
        .bind(tenant_id)
        .execute(&s.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Tag {} not found", tag_id)));
    }

    Ok(Json(json!({"status": "deleted", "tag_id": tag_id.to_string()})))
}

pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/", post(internal_create_tag))
        .route("/list", post(internal_list_tags))
        .route("/assign", post(internal_assign_tag))
        .route("/delete", post(internal_delete_tag))
}
