use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use serde_json::{json, Value};
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct NotificationRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub trigger_event: String,
    pub action: String,
    pub template_id: Option<Uuid>,
    pub target_entity: Option<String>,
    pub config: serde_json::Value,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateRuleRequest {
    pub trigger_event: String,
    pub action: String,
    pub template_id: Option<Uuid>,
    pub target_entity: Option<String>,
    pub config: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UpdateRuleRequest {
    pub trigger_event: Option<String>,
    pub action: Option<String>,
    pub template_id: Option<Uuid>,
    pub target_entity: Option<String>,
    pub config: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}

/// GET /api/notifications
pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
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

// ── Notification Rules CRUD ──────────────────────────────────────────────

/// GET /api/notifications/rules — List notification rules
pub async fn list_rules(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rules = sqlx::query_as::<_, NotificationRule>(
        "SELECT * FROM notification_rules WHERE tenant_id = $1 ORDER BY created_at DESC"
    ).bind(tid).fetch_all(&s.db).await?;
    Ok(Json(json!({"rules": rules})))
}

/// POST /api/notifications/rules — Create a notification rule
pub async fn create_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if r.trigger_event.is_empty() || r.action.is_empty() {
        return Err(AppError::Validation("trigger_event and action are required".to_string()));
    }

    let valid_actions = ["send_email", "send_sms", "send_whatsapp", "in_app"];
    if !valid_actions.contains(&r.action.as_str()) {
        return Err(AppError::Validation("action must be one of: send_email, send_sms, send_whatsapp, in_app".to_string()));
    }

    let rule = sqlx::query_as::<_, NotificationRule>(
        r#"INSERT INTO notification_rules (id, tenant_id, trigger_event, action, template_id, target_entity, config, is_active)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(tid)
    .bind(&r.trigger_event)
    .bind(&r.action)
    .bind(r.template_id)
    .bind(&r.target_entity)
    .bind(r.config.unwrap_or(json!({})))
    .bind(r.is_active.unwrap_or(true))
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(rule))))
}

/// PATCH /api/notifications/rules/{id} — Update a rule
pub async fn update_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let rule = sqlx::query_as::<_, NotificationRule>(
        r#"UPDATE notification_rules SET
            trigger_event = COALESCE($1, trigger_event),
            action = COALESCE($2, action),
            template_id = COALESCE($3, template_id),
            target_entity = COALESCE($4, target_entity),
            config = COALESCE($5, config),
            is_active = COALESCE($6, is_active),
            updated_at = NOW()
           WHERE id = $7 AND tenant_id = $8 RETURNING *"#
    )
    .bind(&r.trigger_event)
    .bind(&r.action)
    .bind(r.template_id)
    .bind(&r.target_entity)
    .bind(r.config)
    .bind(r.is_active)
    .bind(id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(rule)))
}

/// DELETE /api/notifications/rules/{id} — Delete a rule
pub async fn delete_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM notification_rules WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Rule deleted"})))
}

/// GET /api/notifications/queue — List notification queue items
pub async fn list_queue(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let status_filter = p.get("status").and_then(|v| v.as_str()).unwrap_or("");

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
    struct NQueueItem {
        id: Uuid,
        tenant_id: Uuid,
        rule_id: Option<Uuid>,
        channel: String,
        to_address: Option<String>,
        subject: Option<String>,
        body: String,
        status: String,
        error_message: Option<String>,
        sent_at: Option<chrono::DateTime<chrono::Utc>>,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    let items: Vec<serde_json::Value> = if !status_filter.is_empty() {
        sqlx::query_as::<_, NQueueItem>(
            "SELECT * FROM notification_queue WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ).bind(tid).bind(status_filter).bind(per_page).bind(offset).fetch_all(&s.db).await?
            .into_iter().map(|i| serde_json::to_value(i).unwrap_or_default()).collect()
    } else {
        sqlx::query_as::<_, NQueueItem>(
            "SELECT * FROM notification_queue WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?
            .into_iter().map(|i| serde_json::to_value(i).unwrap_or_default()).collect()
    };

    Ok(Json(json!({"items": items, "page": page, "per_page": per_page})))
}

/// POST /api/notifications/queue — Queue a notification to be sent
pub async fn enqueue_notification(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(body): Json<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let channel = body.get("channel").and_then(|v| v.as_str()).unwrap_or("email");
    let to_address = body.get("to").and_then(|v| v.as_str()).unwrap_or("");
    let subject = body.get("subject").and_then(|v| v.as_str());
    let body_text = body.get("body").and_then(|v| v.as_str()).unwrap_or("");

    if to_address.is_empty() || body_text.is_empty() {
        return Err(AppError::Validation("to and body are required".to_string()));
    }

    let item = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO notification_queue (id, tenant_id, channel, to_address, subject, body, status)
           VALUES ($1, $2, $3, $4, $5, $6, 'queued') RETURNING id"#
    )
    .bind(Uuid::new_v4())
    .bind(tid)
    .bind(channel)
    .bind(to_address)
    .bind(subject)
    .bind(body_text)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({"id": item, "status": "queued"}))))
}
