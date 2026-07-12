use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use serde_json::{json, Value};
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use sqlx::PgPool;
use crate::auth::models::Claims;
use super::providers;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct OutboundMessage {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub channel: String,
    pub to_address: String,
    pub subject: Option<String>,
    pub body: String,
    pub status: String,
    pub sent_at: Option<chrono::DateTime<chrono::Utc>>,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct MessageTemplate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub channel: String,
    pub subject: Option<String>,
    pub body: String,
    pub variables: serde_json::Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct SendRequest {
    pub channel: String,
    pub to: String,
    pub subject: Option<String>,
    pub body: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub channel: String,
    pub subject: Option<String>,
    pub body: String,
    pub variables: Option<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdateTemplateRequest {
    pub subject: Option<String>,
    pub body: Option<String>,
    pub variables: Option<serde_json::Value>,
}

/// GET /api/comms/messages — List outbound messages
pub async fn list_messages(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let channel = p.get("channel").and_then(|v| v.as_str()).unwrap_or("");

    let (msgs, total) = if !channel.is_empty() {
        let m = sqlx::query_as::<_, OutboundMessage>(
            "SELECT * FROM outbound_messages WHERE tenant_id = $1 AND channel = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ).bind(tid).bind(channel).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let t: i64 = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM outbound_messages WHERE tenant_id = $1 AND channel = $2"
        ).bind(tid).bind(channel).fetch_one(&s.db).await?.unwrap_or(0);
        (m, t)
    } else {
        let m = sqlx::query_as::<_, OutboundMessage>(
            "SELECT * FROM outbound_messages WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let t: i64 = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM outbound_messages WHERE tenant_id = $1"
        ).bind(tid).fetch_one(&s.db).await?.unwrap_or(0);
        (m, t)
    };

    Ok(Json(json!({"messages": msgs, "total": total, "page": page, "per_page": per_page})))
}

/// POST /api/comms/messages — Send a message immediately
pub async fn send(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<SendRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if !["email", "sms"].contains(&r.channel.as_str()) {
        return Err(AppError::Validation("channel must be 'email' or 'sms'".to_string()));
    }
    if r.to.is_empty() || r.body.is_empty() {
        return Err(AppError::Validation("to and body are required".to_string()));
    }

    let msg = sqlx::query_as::<_, OutboundMessage>(
        r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status)
           VALUES ($1, $2, $3, $4, $5, $6, 'queued') RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(&r.channel).bind(&r.to)
    .bind(&r.subject).bind(&r.body)
    .fetch_one(&s.db).await?;

    // Fire-and-forget delivery attempt via configured provider
    let db_clone = s.db.clone();
    let msg_id = msg.id;
    let tid = msg.tenant_id;
    let ch = r.channel.clone();
    let to = r.to.clone();
    let subj = r.subject.clone();
    let body = r.body.clone();
    tokio::spawn(async move {
        deliver_message(&db_clone, msg_id, tid, &ch, &to, subj, &body).await;
    });

    Ok((StatusCode::CREATED, Json(json!(msg))))
}

/// GET /api/comms/messages/{id} — Get message delivery status
pub async fn get_message(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let msg = sqlx::query_as::<_, OutboundMessage>("SELECT * FROM outbound_messages WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("Message not found".to_string()))?;
    Ok(Json(json!(msg)))
}

/// GET /api/comms/templates — List message templates
pub async fn list_templates(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let templates = sqlx::query_as::<_, MessageTemplate>(
        "SELECT * FROM message_templates WHERE tenant_id = $1 ORDER BY name ASC LIMIT $2 OFFSET $3"
    ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;

    Ok(Json(json!({"templates": templates, "page": page, "per_page": per_page})))
}

/// POST /api/comms/templates — Create message template
pub async fn create_template(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateTemplateRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.body.is_empty() {
        return Err(AppError::Validation("name and body are required".to_string()));
    }
    if !["email", "sms"].contains(&r.channel.as_str()) {
        return Err(AppError::Validation("channel must be 'email' or 'sms'".to_string()));
    }

    let tmpl = sqlx::query_as::<_, MessageTemplate>(
        r#"INSERT INTO message_templates (id, tenant_id, name, channel, subject, body, variables)
           VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(&r.name).bind(&r.channel)
    .bind(&r.subject).bind(&r.body).bind(r.variables.unwrap_or(json!({})))
    .fetch_one(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(tmpl))))
}

/// PATCH /api/comms/templates/{id} — Update template
pub async fn update_template(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateTemplateRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let tmpl = sqlx::query_as::<_, MessageTemplate>(
        r#"UPDATE message_templates SET
            subject = COALESCE($1, subject),
            body = COALESCE($2, body),
            variables = COALESCE($3, variables),
            updated_at = NOW()
           WHERE id = $4 AND tenant_id = $5 RETURNING *"#
    )
    .bind(&r.subject).bind(&r.body).bind(&r.variables).bind(id).bind(tid)
    .fetch_one(&s.db).await?;
    Ok(Json(json!(tmpl)))
}

/// DELETE /api/comms/templates/{id} — Delete template
pub async fn delete_template(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM message_templates WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Template deleted"})))
}

/// GET /api/comms/providers — Get communication provider config
pub async fn get_providers(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let config = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT settings->'communications' FROM tenants WHERE id = $1"
    ).bind(tid).fetch_optional(&s.db).await?;
    let merged = config.flatten().unwrap_or(json!({
        "email_provider": "mailgun",
        "sms_provider": "telnyx",
        "from_email": null,
        "from_name": null,
        "ai": {
            "providers": {
                "deepseek": null,
                "openai": null,
                "anthropic": null
            }
        }
    }));
    Ok(Json(json!(merged)))
}

/// PATCH /api/comms/providers — Update communication provider config
pub async fn update_providers(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(settings): Json<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let _ = sqlx::query(
        r#"UPDATE tenants SET settings = jsonb_set(COALESCE(settings, '{}'::jsonb), '{communications}', $1::jsonb), updated_at = NOW() WHERE id = $2"#
    )
    .bind(&settings).bind(tid)
    .execute(&s.db).await?;
    Ok(Json(json!({"message": "Provider settings updated", "settings": settings})))
}

/// Deliver a message via the configured provider (Mailgun, SMTP.com, or Telnyx).
/// Loads provider config from tenant settings, calls the API, and updates the message status.
async fn deliver_message(db: &PgPool, msg_id: Uuid, tenant_id: Uuid, channel: &str, to: &str, subject: Option<String>, body: &str) {
    tracing::info!(msg = %msg_id, channel = %channel, to = %to, "Delivering message via provider");

    // Load tenant's provider config and deliver
    let cfg = providers::load_delivery_config(
        db, msg_id, tenant_id, channel, to, subject, body
    ).await;

    let (success, error) = providers::deliver(&cfg).await;

    let (status, error_msg) = if success {
        ("sent".to_string(), None)
    } else {
        ("failed".to_string(), error.or(Some("Delivery failed — unknown error".to_string())))
    };

    let _ = sqlx::query(
        "UPDATE outbound_messages SET status = $1, sent_at = NOW(), error_message = $2 WHERE id = $3"
    )
    .bind(&status).bind(&error_msg).bind(msg_id)
    .execute(db).await;
}
