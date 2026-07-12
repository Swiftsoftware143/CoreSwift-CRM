use axum::{extract::{State, Path, Json, Extension, Query}, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct AuditEntry {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Option<Uuid>,
    pub action: String,
    pub entity_type: String,
    pub entity_id: Option<Uuid>,
    pub changes: Option<serde_json::Value>,
    pub ip_address: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/audit — List audit logs for tenant (filterable)
pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(params): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.get("page").and_then(|v| v.as_i64()), params.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    // Build filter query
    let action = params.get("action").and_then(|v| v.as_str()).unwrap_or("");
    let entity_type = params.get("entity_type").and_then(|v| v.as_str()).unwrap_or("");
    let entity_id = params.get("entity_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    let user_id = params.get("user_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());

    // Use dynamic building based on filters
    if !action.is_empty() && !entity_type.is_empty() {
        let logs = sqlx::query_as::<_, AuditEntry>(
            "SELECT * FROM audit_logs WHERE tenant_id = $1 AND action = $2 AND entity_type = $3 ORDER BY created_at DESC LIMIT $4 OFFSET $5"
        )
        .bind(tid).bind(action).bind(entity_type).bind(per_page).bind(offset)
        .fetch_all(&s.db).await?;
        return Ok(Json(json!({"logs": logs, "page": page, "per_page": per_page})));
    }

    if let Some(uid) = user_id {
        let logs = sqlx::query_as::<_, AuditEntry>(
            "SELECT * FROM audit_logs WHERE tenant_id = $1 AND user_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        )
        .bind(tid).bind(uid).bind(per_page).bind(offset)
        .fetch_all(&s.db).await?;
        return Ok(Json(json!({"logs": logs, "page": page, "per_page": per_page})));
    }

    if let Some(eid) = entity_id {
        let logs = sqlx::query_as::<_, AuditEntry>(
            "SELECT * FROM audit_logs WHERE tenant_id = $1 AND entity_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        )
        .bind(tid).bind(eid).bind(per_page).bind(offset)
        .fetch_all(&s.db).await?;
        return Ok(Json(json!({"logs": logs, "page": page, "per_page": per_page})));
    }

    let logs = sqlx::query_as::<_, AuditEntry>(
        "SELECT * FROM audit_logs WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    )
    .bind(tid).bind(per_page).bind(offset)
    .fetch_all(&s.db).await?;

    Ok(Json(json!({"logs": logs, "page": page, "per_page": per_page})))
}

/// GET /api/audit/{id} — Get single audit log entry
pub async fn get(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let entry = sqlx::query_as::<_, AuditEntry>(
        "SELECT * FROM audit_logs WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id).bind(tid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Audit log entry not found".to_string()))?;

    Ok(Json(json!(entry)))
}
