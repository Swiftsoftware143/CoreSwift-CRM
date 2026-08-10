//! Message handlers: CRUD + webhook ingest with auto-routing.

use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde_json::json;
use uuid::Uuid;

use super::models::*;
use crate::auth::models::Claims;
use crate::errors::{validate_pagination, ApiResult, AppError};
use crate::AppState;

/// GET /api/messages — list inbox messages (tenant-scoped).
pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<MessageListParams>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let (page, per_page) = validate_pagination(params.page, params.limit);
    let offset = (page - 1) * per_page;

    let status_filter = params.status.unwrap_or_default();
    let search = params.search.unwrap_or_default();

    // Build query dynamically based on filters
    let messages: Vec<Message> = if !search.is_empty() {
        let pattern = format!("%{}%", search);
        sqlx::query_as::<_, Message>(
            r#"SELECT * FROM cs_messages
               WHERE tenant_id = $1
                 AND (sender_name ILIKE $2 OR subject ILIKE $2 OR body ILIKE $2 OR sender_email ILIKE $2)
               ORDER BY created_at DESC
               LIMIT $3 OFFSET $4"#,
        )
        .bind(tenant_id)
        .bind(&pattern)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?
    } else {
        match status_filter.as_str() {
            "unread" => sqlx::query_as::<_, Message>(
                r#"SELECT * FROM cs_messages
                       WHERE tenant_id = $1 AND is_read = false AND is_archived = false
                       ORDER BY created_at DESC
                       LIMIT $2 OFFSET $3"#,
            )
            .bind(tenant_id)
            .bind(per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?,
            "archived" => sqlx::query_as::<_, Message>(
                r#"SELECT * FROM cs_messages
                       WHERE tenant_id = $1 AND is_archived = true
                       ORDER BY created_at DESC
                       LIMIT $2 OFFSET $3"#,
            )
            .bind(tenant_id)
            .bind(per_page)
            .bind(offset)
            .fetch_all(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?,
            _ => {
                // "all" — exclude archived
                sqlx::query_as::<_, Message>(
                    r#"SELECT * FROM cs_messages
                       WHERE tenant_id = $1 AND is_archived = false
                       ORDER BY created_at DESC
                       LIMIT $2 OFFSET $3"#,
                )
                .bind(tenant_id)
                .bind(per_page)
                .bind(offset)
                .fetch_all(&state.db)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?
            }
        }
    };

    Ok(Json(json!({
        "messages": messages,
        "page": page,
        "per_page": per_page,
        "total": messages.len()
    })))
}

/// GET /api/messages/:id — get a single message.
pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let message =
        sqlx::query_as::<_, Message>("SELECT * FROM cs_messages WHERE id = $1 AND tenant_id = $2")
            .bind(message_id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| AppError::Internal(e.to_string()))?
            .ok_or(AppError::NotFound("Message not found".into()))?;

    Ok(Json(json!({ "message": message })))
}

/// POST /api/messages — create a message manually.
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<CreateMessageRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let message = sqlx::query_as::<_, Message>(
        r#"INSERT INTO cs_messages (tenant_id, contact_id, sender_name, sender_email,
           sender_phone, subject, body, source)
           VALUES ($1, $2, $3, $4, $5, $6, $7, 'manual')
           RETURNING *"#,
    )
    .bind(tenant_id)
    .bind(payload.contact_id)
    .bind(&payload.sender_name)
    .bind(&payload.sender_email)
    .bind(&payload.sender_phone)
    .bind(&payload.subject)
    .bind(&payload.body)
    .fetch_one(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    Ok((StatusCode::CREATED, Json(json!({ "message": message }))))
}

/// PATCH /api/messages/:id — update read/archive status.
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
    Json(payload): Json<UpdateMessageRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let message = sqlx::query_as::<_, Message>(
        r#"UPDATE cs_messages
           SET is_read = COALESCE($3, is_read),
               is_archived = COALESCE($4, is_archived),
               updated_at = NOW()
           WHERE id = $1 AND tenant_id = $2
           RETURNING *"#,
    )
    .bind(message_id)
    .bind(tenant_id)
    .bind(payload.is_read)
    .bind(payload.is_archived)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?
    .ok_or(AppError::NotFound("Message not found".into()))?;

    Ok(Json(json!({ "message": message })))
}

/// DELETE /api/messages/:id — delete a message.
pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM cs_messages WHERE id = $1 AND tenant_id = $2")
        .bind(message_id)
        .bind(tenant_id)
        .execute(&state.db)
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Message not found".into()));
    }

    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/internal/messages/webhook — receive messages from MD/IS (no auth).
pub async fn webhook_receive(
    State(state): State<AppState>,
    Json(payload): Json<WebhookMessagePayload>,
) -> impl IntoResponse {
    let source = payload
        .source
        .clone()
        .unwrap_or_else(|| "unknown".to_string());

    // Determine which tenant this message belongs to.
    // For now, use the sender_email to look up the tenant.
    // In production, this would be a more robust mapping.
    let tenant_id = if let Some(ref email) = payload.sender_email {
        // Try to find a contact with this email, use their tenant
        sqlx::query_scalar::<_, Uuid>(
            "SELECT tenant_id FROM contacts WHERE email = $1 AND is_active = true LIMIT 1",
        )
        .bind(email)
        .fetch_one(&state.db)
        .await
        .ok()
    } else {
        None
    };

    // If no tenant found by email, use the first non-admin tenant
    let tenant_id = match tenant_id {
        Some(id) => id,
        None => {
            match sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM tenants WHERE slug NOT IN ('admin','super_admin') LIMIT 1",
            )
            .fetch_one(&state.db)
            .await
            {
                Ok(id) => id,
                Err(_) => {
                    return Json(json!({
                        "ok": false,
                        "error": "No tenant found for webhook message"
                    }))
                    .into_response();
                }
            }
        }
    };

    // Try to find or create a contact for this sender
    let contact_id = if let Some(ref email) = payload.sender_email {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM contacts WHERE email = $1 AND tenant_id = $2 AND is_active = true LIMIT 1",
        )
        .bind(email)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await
        .ok()
    } else {
        None
    };

    // Insert the message
    let result = sqlx::query_as::<_, Message>(
        r#"INSERT INTO cs_messages (tenant_id, contact_id, sender_name, sender_email,
           sender_phone, subject, body, source, source_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           RETURNING *"#,
    )
    .bind(tenant_id)
    .bind(contact_id)
    .bind(&payload.sender_name)
    .bind(&payload.sender_email)
    .bind(&payload.sender_phone)
    .bind(&payload.subject)
    .bind(&payload.body)
    .bind(&source)
    .bind(&payload.source_id)
    .fetch_one(&state.db)
    .await;

    match result {
        Ok(msg) => Json(json!({ "ok": true, "message_id": msg.id })).into_response(),
        Err(e) => Json(json!({
            "ok": false,
            "error": format!("Failed to store message: {}", e)
        }))
        .into_response(),
    }
}
