//! The files a customer sent.
//!
//! Nothing here stores or serves bytes: the mail pipeline already keeps attachments in
//! `private_email_attachments` and already has a scoped download route. This is the far smaller piece that
//! was missing — asking "which files belong to this ticket?" so a timeline can say a screenshot arrived.
//!
//! It reaches the files through `ticket_messages.email_message_row_id`, the link written at ingest, rather
//! than through the Message-Id header: a header is not a key (a client may repeat it, and a missing one would
//! break the join silently), and a file on the wrong ticket is worse than no file at all.

use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Serialize;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct TicketAttachment {
    /// Which message on the timeline carried it, so the console can put the chip under the right bubble.
    pub ticket_message_id: Uuid,
    pub attachment_id: Uuid,
    /// The stored mail row — the download route is scoped by it.
    pub message_row_id: Uuid,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
}

pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // The ticket is read WITH the tenant, so a ticket id from another account answers the same as a missing
    // one: nothing about it leaks, including whether it exists.
    let owns: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(ticket_id)
            .bind(tid)
            .fetch_optional(&s.db)
            .await?;
    if owns.is_none() {
        return Err(AppError::NotFound("Ticket not found".into()));
    }

    let rows: Vec<TicketAttachment> = sqlx::query_as(
        r#"SELECT tm.id  AS ticket_message_id,
                  a.id   AS attachment_id,
                  pm.id  AS message_row_id,
                  a.filename,
                  a.content_type,
                  -- cast in the query: the column is integer and the response type is i64, and sqlx will not
                  -- widen it for you (it answers a 500 "Database error" that looks like anything but this)
                  a.size_bytes::bigint AS size_bytes
             FROM ticket_messages tm
             JOIN private_email_messages pm ON pm.id = tm.email_message_row_id
             JOIN private_email_attachments a ON a.message_id = pm.id
            WHERE tm.ticket_id = $1
              AND pm.tenant_id = $2
            ORDER BY tm.created_at, a.filename"#,
    )
    .bind(ticket_id)
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(serde_json::json!({
        "attachments": rows,
        "count": rows.len(),
    })))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    axum::Router::new().route("/tickets/:id/attachments", axum::routing::get(list))
}
