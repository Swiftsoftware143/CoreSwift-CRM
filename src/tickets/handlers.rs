use axum::{
    extract::{Path, Query, State, Extension},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};

use super::models::*;

// ── Ticket CRUD ──────────────────────────────────────────────────────────

/// GET /api/tickets
pub async fn list_tickets(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(q): Query<TicketListQuery>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let limit = q.limit.unwrap_or(50).min(100);
    let offset = q.offset.unwrap_or(0);

    let tickets = sqlx::query_as::<_, Ticket>(
        r#"SELECT id, tenant_id, subject, description, status, priority, assigned_to, contact_id, created_at, updated_at
           FROM tickets
           WHERE tenant_id = $1
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR priority = $3)
             AND ($4::uuid IS NULL OR contact_id = $4)
           ORDER BY
             CASE priority
               WHEN 'urgent' THEN 0
               WHEN 'high' THEN 1
               WHEN 'medium' THEN 2
               WHEN 'low' THEN 3
             END,
             created_at DESC
           LIMIT $5 OFFSET $6"#
    )
    .bind(tid)
    .bind(&q.status)
    .bind(&q.priority)
    .bind(&q.contact_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "tickets": tickets, "count": tickets.len() })))
}

/// GET /api/tickets/:id
pub async fn get_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let ticket = sqlx::query_as::<_, Ticket>(
        "SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2"
    )
    .bind(ticket_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;

    let messages = sqlx::query_as::<_, TicketMessage>(
        "SELECT * FROM ticket_messages WHERE ticket_id = $1 ORDER BY created_at ASC"
    )
    .bind(ticket_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "ticket": ticket, "messages": messages })))
}

/// POST /api/tickets
pub async fn create_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CreateTicketRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let priority = body.priority.unwrap_or_else(|| "medium".to_string());
    let description = body.description.unwrap_or_default();

    let ticket = sqlx::query_as::<_, Ticket>(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, assigned_to, contact_id)
           VALUES ($1, $2, $3, 'open', $4, $5, $6)
           RETURNING *"#
    )
    .bind(tid)
    .bind(&body.subject)
    .bind(&description)
    .bind(&priority)
    .bind(&body.assigned_to)
    .bind(&body.contact_id)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(ticket))))
}

/// PATCH /api/tickets/:id
pub async fn update_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
    Json(body): Json<UpdateTicketRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let ticket = sqlx::query_as::<_, Ticket>(
        "SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2"
    )
    .bind(ticket_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;

    let new_subject = body.subject.unwrap_or(ticket.subject);
    let new_description = body.description.unwrap_or(ticket.description);
    let new_status = body.status.unwrap_or(ticket.status);
    let new_priority = body.priority.unwrap_or(ticket.priority);
    let new_assigned_to = body.assigned_to.or(ticket.assigned_to);

    let updated = sqlx::query_as::<_, Ticket>(
        r#"UPDATE tickets
           SET subject = $1, description = $2, status = $3, priority = $4,
               assigned_to = $5, updated_at = NOW()
           WHERE id = $6 AND tenant_id = $7
           RETURNING *"#
    )
    .bind(&new_subject)
    .bind(&new_description)
    .bind(&new_status)
    .bind(&new_priority)
    .bind(&new_assigned_to)
    .bind(ticket_id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(updated)))
}

/// DELETE /api/tickets/:id
pub async fn delete_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let result = sqlx::query("DELETE FROM tickets WHERE id = $1 AND tenant_id = $2")
        .bind(ticket_id)
        .bind(tid)
        .execute(&s.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Ticket not found".into()));
    }
    Ok(Json(json!({ "status": "deleted" })))
}

// ── Messages ─────────────────────────────────────────────────────────────

/// POST /api/tickets/:id/messages
pub async fn add_message(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
    Json(body): Json<AddMessageRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Verify ticket belongs to tenant
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM tickets WHERE id = $1 AND tenant_id = $2)"
    )
    .bind(ticket_id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Ticket not found".into()));
    }

    let sender = body.sender_type.unwrap_or_else(|| "agent".to_string());
    let msg = sqlx::query_as::<_, TicketMessage>(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, message)
           VALUES ($1, $2, $3) RETURNING *"#
    )
    .bind(ticket_id)
    .bind(&sender)
    .bind(&body.message)
    .fetch_one(&s.db)
    .await?;

    // Touch ticket updated_at
    sqlx::query("UPDATE tickets SET updated_at = NOW() WHERE id = $1")
        .bind(ticket_id)
        .execute(&s.db)
        .await?;

    Ok((StatusCode::CREATED, Json(json!(msg))))
}

// ── Quick Stats ───────────────────────────────────────────────────────────

/// GET /api/tickets/stats
pub async fn ticket_stats(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    #[derive(FromRow, Serialize)]
    struct StatRow { status: String, count: Option<i64> }

    let rows = sqlx::query_as::<_, StatRow>(
        r#"SELECT status, COUNT(*) as count
           FROM tickets WHERE tenant_id = $1 GROUP BY status"#
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let total = rows.iter().fold(0i64, |acc, r| acc + r.count.unwrap_or(0));
    let open = rows.iter().find(|r| r.status == "open").and_then(|r| r.count).unwrap_or(0);
    let in_progress = rows.iter().find(|r| r.status == "in_progress").and_then(|r| r.count).unwrap_or(0);
    let resolved = rows.iter().find(|r| r.status == "resolved").and_then(|r| r.count).unwrap_or(0);
    let closed = rows.iter().find(|r| r.status == "closed").and_then(|r| r.count).unwrap_or(0);

    Ok(Json(json!({
        "total": total,
        "open": open,
        "in_progress": in_progress,
        "resolved": resolved,
        "closed": closed
    })))
}
