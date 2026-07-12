use axum::{extract::{State, Path, Json, Extension, Query}, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct Notification {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub message: String,
    pub read: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// GET /api/notifications
pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let read_filter = p.get("read").and_then(|v| v.as_str());

    let (notifications, total) = match read_filter {
        Some("true") => {
            let n = sqlx::query_as::<_, Notification>(
                "SELECT * FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read=true ORDER BY created_at DESC LIMIT $3 OFFSET $4"
            ).bind(tid).bind(uid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
            let t: i64 = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read=true")
                .bind(tid).bind(uid).fetch_one(&s.db).await?.unwrap_or(0);
            (n, t)
        }
        Some("false") => {
            let n = sqlx::query_as::<_, Notification>(
                "SELECT * FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read=false ORDER BY created_at DESC LIMIT $3 OFFSET $4"
            ).bind(tid).bind(uid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
            let t: i64 = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read=false")
                .bind(tid).bind(uid).fetch_one(&s.db).await?.unwrap_or(0);
            (n, t)
        }
        _ => {
            let n = sqlx::query_as::<_, Notification>(
                "SELECT * FROM notifications WHERE tenant_id=$1 AND user_id=$2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
            ).bind(tid).bind(uid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
            let t: i64 = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM notifications WHERE tenant_id=$1 AND user_id=$2")
                .bind(tid).bind(uid).fetch_one(&s.db).await?.unwrap_or(0);
            (n, t)
        }
    };

    Ok(Json(json!({"notifications": notifications, "total": total, "page": page, "per_page": per_page})))
}

/// POST /api/notifications/{id}/read
pub async fn mark_read(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("UPDATE notifications SET read=true WHERE id=$1 AND tenant_id=$2 AND user_id=$3")
        .bind(id).bind(tid).bind(uid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Marked as read"})))
}

/// POST /api/notifications/read-all
pub async fn mark_all_read(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("UPDATE notifications SET read=true WHERE tenant_id=$1 AND user_id=$2 AND read=false")
        .bind(tid).bind(uid).execute(&s.db).await?;
    Ok(Json(json!({"message": "All marked as read"})))
}

/// GET /api/notifications/unread-count
pub async fn unread_count(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let count: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read=false"
    ).bind(tid).bind(uid).fetch_one(&s.db).await?.unwrap_or(0);
    Ok(Json(json!({"unread_count": count})))
}
