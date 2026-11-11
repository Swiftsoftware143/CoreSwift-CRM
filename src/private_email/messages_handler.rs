//! The inbox's own records: every message that arrived, where it arrived, and what happened to it.
//!
//! WHY THIS MODULE EXISTS (kanban B204/B205/B211)
//! Until it did, receiving had no record of its own. The inbound webhook created a contact and
//! then, only if the mailbox was ticked, a ticket — and the inbox screen read TICKETS. So a message
//! to an address that does not create tickets (info@, david@) was accepted, acknowledged with 200
//! and then invisible: nothing in the product could show it, because nothing had stored it. Mail
//! that arrived and mail a human could see were two different things wearing one name.
//!
//! A message is now its own row, written for EVERY authenticated delivery whatever the ticket
//! toggle says. It carries:
//!   * `address`   — the address it ARRIVED ON, so one catch-all inbox can serve many addresses
//!                   and the client can filter by address, with MISC for addresses nobody created;
//!   * `is_ticket` — whether it ALSO became a ticket. A ticketed message is in the work queue AND
//!                   in the inbox; an unticketed one is in the inbox awaiting a decision, and any
//!                   message can be turned into a ticket with one call.
//!
//! ACCOUNT SCOPE, deliberately: every query is keyed on the account id from the session token
//! (`claims.aid`). There is no workspace column and no workspace filter — mail belongs to the
//! business, not to a workspace (B211, David 2026-11-11).

use axum::{
    extract::{Path, Query, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The stored folder a message sits in. The eight surfaces in the sidebar are VIEWS over this
/// column plus `is_read` / `is_starred`; only a real store is ever written here. A custom
/// subfolder is a path under its parent ('inbox/Clients'), which is what makes nesting real
/// server-side state rather than a client-side label (B205).
const SYSTEM_FOLDERS: [&str; 6] = ["inbox", "archive", "spam", "trash", "sent", "drafts"];

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// inbox | unread | favorites | drafts | sent | archive | spam | trash | all
    #[serde(default)]
    pub folder: Option<String>,
    /// An exact address, "misc" for catch-all arrivals with no mailbox row, or "all".
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
    #[serde(default)]
    pub offset: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMessageRequest {
    pub is_read: Option<bool>,
    pub is_starred: Option<bool>,
    /// Move to a real store: inbox | archive | spam | trash | drafts, or a subfolder path.
    pub folder: Option<String>,
    pub assigned_user_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct ReplyRequest {
    pub from_address: String,
    pub body: String,
    #[serde(default)]
    pub subject: Option<String>,
    /// reply (default) | reply_all | forward
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FolderActionRequest {
    pub folder: String,
    /// read | unread | empty
    pub action: String,
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// Reading
// ────────────────────────────────────────────────────────────────────────────────────────────────

/// GET /api/private-email/messages
///
/// One page of the inbox. Every row is a message that was RECEIVED or SENT by this account — never
/// a ticket-shaped guess — with the address it arrived on and whether it also became a ticket.
pub async fn list_messages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let view = q.folder.as_deref().unwrap_or("inbox").to_string();
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset = q.offset.unwrap_or(0).max(0);

    // The address filter's value, normalised: "all" (everything), "misc" (catch-all arrivals with no
    // mailbox row) or one exact address.
    let address_filter = q.address.as_deref().unwrap_or("all").trim().to_lowercase();
    let search =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("")
            .to_string();

    // ONE statement, every filter a BIND. The view -> predicate mapping lives inside a CASE over
    // $2 and the optional filters are written as `$n = 'sentinel' OR <predicate>`, so no caller
    // value is ever concatenated into SQL text: the fleet's rule against run-time-built SQL, and
    // what keeps an address like `x' OR 1=1 --` a value rather than a statement.
    let rows = sqlx::query(
        r#"
        SELECT m.id, m.mailbox_id, m.address, m.folder, m.direction,
               m.from_address, m.from_name, m.to_address, m.subject, m.snippet,
               m.is_read, m.is_starred, m.ticket_id, m.is_ticket, m.assigned_user_id,
               m.has_attachments, m.size_bytes, m.received_at, m.message_id, m.cc_address
          FROM private_email_messages m
         WHERE m.tenant_id = $1
           AND CASE $2
                 WHEN 'unread'    THEN m.is_read = false AND m.folder NOT IN ('trash','spam','drafts','sent')
                 WHEN 'favorites' THEN m.is_starred = true AND m.folder <> 'trash'
                 WHEN 'drafts'    THEN m.folder = 'drafts'
                 WHEN 'sent'      THEN m.folder = 'sent'
                 WHEN 'archive'   THEN m.folder = 'archive'
                 WHEN 'spam'      THEN m.folder = 'spam'
                 WHEN 'trash'     THEN m.folder = 'trash'
                 WHEN 'all'       THEN m.folder NOT IN ('trash','spam')
                 ELSE (m.folder = 'inbox' OR m.folder LIKE 'inbox/%')
               END
           AND ( $3 = 'all'
                 OR ($3 = 'misc' AND m.mailbox_id IS NULL)
                 OR m.address = $3 )
           AND ( $4 = ''
                 OR m.subject ILIKE '%' || $4 || '%'
                 OR m.from_address ILIKE '%' || $4 || '%'
                 OR m.snippet ILIKE '%' || $4 || '%' )
         ORDER BY m.received_at DESC
         LIMIT $5 OFFSET $6
        "#,
    )
    .bind(tenant_id)
    .bind(&view)
    .bind(&address_filter)
    .bind(&search)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let messages: Vec<Value> = rows.iter().map(message_json).collect();

    Ok(Json(json!({
        "messages": messages,
        "folder": view,
        "address": address_filter,
        "limit": limit,
        "offset": offset,
    })))
}

/// GET /api/private-email/messages/counts
///
/// The badge numbers the sidebar needs, plus the storage figure the retention banner states. All
/// computed from the same table, so a count can never disagree with the list it labels.
pub async fn message_counts(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    let row = sqlx::query(
        r#"
        SELECT
          count(*) FILTER (WHERE folder = 'inbox' OR folder LIKE 'inbox/%')                   AS inbox,
          count(*) FILTER (WHERE (folder = 'inbox' OR folder LIKE 'inbox/%') AND is_read = false) AS unread,
          count(*) FILTER (WHERE is_starred AND folder <> 'trash')                            AS favorites,
          count(*) FILTER (WHERE folder = 'drafts')                                           AS drafts,
          count(*) FILTER (WHERE folder = 'sent')                                             AS sent,
          count(*) FILTER (WHERE folder = 'archive')                                          AS archive,
          count(*) FILTER (WHERE folder = 'spam')                                             AS spam,
          count(*) FILTER (WHERE folder = 'trash')                                            AS trash,
          count(*) FILTER (WHERE is_ticket)                                                   AS tickets,
          coalesce(sum(size_bytes), 0)                                                        AS used_bytes
        FROM private_email_messages
        WHERE tenant_id = $1
        "#,
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    let get = |k: &str| row.try_get::<i64, _>(k).unwrap_or(0);
    let used: i64 = row.try_get::<i64, _>("used_bytes").unwrap_or(0);

    // Storage is stated, not enforced on a clock (B206). The plan's storage_gb is the ceiling; the
    // inbox never refuses inbound mail, so this number only informs.
    let storage_gb = plan_storage_gb(&state, tenant_id).await.unwrap_or(2.0);
    let limit_bytes = (storage_gb * 1024.0 * 1024.0 * 1024.0) as i64;

    Ok(Json(json!({
        "inbox": get("inbox"),
        "unread": get("unread"),
        "favorites": get("favorites"),
        "drafts": get("drafts"),
        "sent": get("sent"),
        "archive": get("archive"),
        "spam": get("spam"),
        "trash": get("trash"),
        "tickets": get("tickets"),
        "used_bytes": used,
        "limit_bytes": limit_bytes,
        "storage_gb": storage_gb,
    })))
}

/// GET /api/private-email/addresses
///
/// The address filter (B211): every address this account has RECEIVED mail on, plus MISC for
/// arrivals at addresses nobody created (catch-all), plus ALL. Counts come with them so the
/// filter can show how much mail is behind each one instead of an empty picker.
pub async fn list_addresses(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    let rows = sqlx::query(
        r#"
        SELECT address, count(*) AS total, count(*) FILTER (WHERE is_read = false) AS unread
          FROM private_email_messages
         WHERE tenant_id = $1 AND folder <> 'trash'
         GROUP BY address
         ORDER BY lower(address)
        "#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let created = sqlx::query(
        "SELECT email_address FROM private_email_boxes WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;
    let created_addresses: Vec<String> = created
        .iter()
        .filter_map(|r| r.try_get::<String, _>("email_address").ok())
        .map(|s| s.to_lowercase())
        .collect();

    let mut total: i64 = 0;
    let mut unread: i64 = 0;
    let mut misc_total: i64 = 0;
    let mut by_address = Vec::new();
    for r in &rows {
        let addr: String = r.try_get::<String, _>("address").unwrap_or_default();
        let n: i64 = r.try_get::<i64, _>("total").unwrap_or(0);
        let u: i64 = r.try_get::<i64, _>("unread").unwrap_or(0);
        total += n;
        unread += u;
        if !created_addresses.is_empty() && !created_addresses.contains(&addr) && !addr.is_empty() {
            misc_total += n;
        }
        by_address.push(json!({
            "address": addr,
            "total": n,
            "unread": u,
            "created": created_addresses.contains(&addr),
        }));
    }

    Ok(Json(json!({
        "addresses": by_address,
        "all": { "address": "all", "total": total, "unread": unread },
        "misc": { "address": "misc", "total": misc_total },
    })))
}

/// GET /api/private-email/messages/:id
///
/// The opened message. Marks it read on the way out (that IS the user opening it); `?peek=1` reads
/// without touching the flag, which is what a prefetch or a summary call needs.
pub async fn get_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let peek = q
        .get("peek")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);

    if !peek {
        sqlx::query(
            "UPDATE private_email_messages SET is_read = true, updated_at = NOW()
              WHERE id = $1 AND tenant_id = $2 AND is_read = false",
        )
        .bind(message_id)
        .bind(tenant_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    let row = sqlx::query(
        r#"
        SELECT id, mailbox_id, address, folder, direction, from_address, from_name,
               to_address, cc_address, subject, body_plain, body_html, snippet,
               message_id, in_reply_to, provider, is_read, is_starred, size_bytes,
               has_attachments, ticket_id, is_ticket, assigned_user_id, received_at
          FROM private_email_messages
         WHERE id = $1 AND tenant_id = $2
        "#,
    )
    .bind(message_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Message not found".into()))?;

    let mut body = message_json(&row);
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "body_plain".into(),
            json!(row.try_get::<String, _>("body_plain").unwrap_or_default()),
        );
        obj.insert(
            "body_html".into(),
            json!(row
                .try_get::<Option<String>, _>("body_html")
                .unwrap_or(None)),
        );
        obj.insert(
            "labels".into(),
            message_labels(&state, tenant_id, message_id).await?,
        );
        obj.insert("is_read".into(), json!(true));
    }
    Ok(Json(body))
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// Writing
// ────────────────────────────────────────────────────────────────────────────────────────────────

/// PATCH /api/private-email/messages/:id — star, read/unread, move, assign.
pub async fn update_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
    Json(req): Json<UpdateMessageRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    if let Some(folder) = req.folder.as_deref() {
        let normalized = folder.trim().to_lowercase();
        let is_system = SYSTEM_FOLDERS.contains(&normalized.as_str());
        let is_subfolder = SYSTEM_FOLDERS
            .iter()
            .any(|f| normalized.starts_with(&format!("{}/", f)))
            && normalized.len() > 8
            && !normalized.contains("//");
        if !is_system && !is_subfolder {
            return Err(AppError::BadRequest(format!(
                "'{}' is not a folder this account has. Use one of inbox, archive, spam, trash, \
                 drafts, or a subfolder such as inbox/Clients.",
                folder
            )));
        }
    }

    // Assigning is the same object as ticket assignment (B204): the person must be in this account.
    if let Some(user_id) = req.assigned_user_id {
        let member: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM users WHERE id = $1 AND tenant_id = $2")
                .bind(user_id)
                .bind(tenant_id)
                .fetch_optional(&state.db)
                .await
                .map_err(AppError::Database)?;
        if member.is_none() {
            return Err(AppError::Validation(
                "That person is not a member of this account.".into(),
            ));
        }
    }

    let row = sqlx::query(
        r#"
        UPDATE private_email_messages
           SET is_read          = COALESCE($3, is_read),
               is_starred       = COALESCE($4, is_starred),
               folder           = COALESCE($5, folder),
               assigned_user_id = COALESCE($6, assigned_user_id),
               updated_at       = NOW()
         WHERE id = $1 AND tenant_id = $2
        RETURNING id, mailbox_id, address, folder, direction, from_address, from_name,
                  to_address, subject, snippet, is_read, is_starred, ticket_id, is_ticket,
                  assigned_user_id, has_attachments, size_bytes, received_at, message_id, cc_address
        "#,
    )
    .bind(message_id)
    .bind(tenant_id)
    .bind(req.is_read)
    .bind(req.is_starred)
    .bind(req.folder.as_deref().map(|f| f.trim().to_lowercase()))
    .bind(req.assigned_user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Message not found".into()))?;

    Ok(Json(message_json(&row)))
}

/// DELETE /api/private-email/messages/:id
///
/// Normal folders: the message moves to Trash and stays restorable.
/// Trash: it is destroyed — emptying a holding pen must actually empty it.
/// Spam: destroyed too. Sending spam to Trash would hide it in a SECOND holding pen (David
/// 2026-11-11: "deleting from Spam is a DELETE FOREVER, never to Trash").
pub async fn delete_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    let folder: Option<(String,)> = sqlx::query_as(
        "SELECT folder FROM private_email_messages WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let folder = folder
        .ok_or_else(|| AppError::NotFound("Message not found".into()))?
        .0;

    if folder == "trash" || folder == "spam" {
        // Ticket rows are the WORK RECORD and are not destroyed by a mailbox action; the message
        // row goes, the ticket it created stays where the team can still see it.
        sqlx::query("DELETE FROM private_email_messages WHERE id = $1 AND tenant_id = $2")
            .bind(message_id)
            .bind(tenant_id)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?;
        return Ok(Json(json!({ "destroyed": true, "folder": folder })));
    }

    sqlx::query(
        "UPDATE private_email_messages SET folder = 'trash', updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({ "trashed": true, "restorable": true })))
}

/// POST /api/private-email/messages/:id/ticket — turn a received message into a work item.
///
/// This is the one-click conversion B204 requires: mail that did not create a ticket (because its
/// mailbox is not ticked) can still become one. The message and the ticket then point at each
/// other, so the inbox can show "ticket" and the ticket can show its mail.
pub async fn create_ticket_from_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    let row = sqlx::query(
        "SELECT subject, body_plain, from_address, from_name, ticket_id
           FROM private_email_messages WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Message not found".into()))?;

    // Already a ticket: answer with it instead of stacking a second work item on the same mail.
    if let Some(existing) = row.try_get::<Option<Uuid>, _>("ticket_id").unwrap_or(None) {
        return Ok(Json(json!({ "ticket_id": existing, "created": false })));
    }

    let subject: String = row
        .try_get::<String, _>("subject")
        .unwrap_or_else(|_| "(no subject)".into());
    let body: String = row.try_get::<String, _>("body_plain").unwrap_or_default();
    let from: String = row.try_get::<String, _>("from_address").unwrap_or_default();
    let name: Option<String> = row
        .try_get::<Option<String>, _>("from_name")
        .unwrap_or(None);

    let ticket_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source,
                                contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', 'medium', 'email', $4, $5)
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(if subject.trim().is_empty() {
        "(no subject)"
    } else {
        &subject
    })
    .bind(&body)
    .bind(&from)
    .bind(
        name.as_deref()
            .unwrap_or(from.split('@').next().unwrap_or("")),
    )
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    sqlx::query(
        "UPDATE private_email_messages
            SET ticket_id = $3, is_ticket = true, updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .bind(ticket_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({ "ticket_id": ticket_id, "created": true })))
}

/// POST /api/private-email/messages/:id/reply — answer the mail from one of the account's
/// addresses, and record the answer in Sent so the thread is not one-sided.
///
/// The send itself goes through the SAME provider path as /send (one sender, one credential), so a
/// reply can never leave by a route the account's own sending does not use.
pub async fn reply_to_message(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(message_id): Path<Uuid>,
    Json(req): Json<ReplyRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;

    let original = sqlx::query(
        "SELECT from_address, from_name, to_address, cc_address, subject
           FROM private_email_messages WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Message not found".into()))?;

    let from_original: String = original
        .try_get::<String, _>("from_address")
        .unwrap_or_default();
    let subject_original: String = original.try_get::<String, _>("subject").unwrap_or_default();

    let mode = req.mode.as_deref().unwrap_or("reply");
    let subject = req.subject.clone().unwrap_or_else(|| {
        if subject_original.to_lowercase().starts_with("re:") {
            subject_original.clone()
        } else if mode == "forward" {
            format!("Fwd: {}", subject_original)
        } else {
            format!("Re: {}", subject_original)
        }
    });

    let to = match req.to.clone() {
        Some(t) if !t.trim().is_empty() => t,
        _ => from_original.clone(),
    };

    // The mailbox must belong to this account — a From selector is not permission to send as
    // somebody else's address.
    let mailbox = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM private_email_boxes
          WHERE tenant_id = $1 AND email_address = $2 AND status = 'active'",
    )
    .bind(tenant_id)
    .bind(&req.from_address)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    if mailbox.is_none() {
        return Err(AppError::Validation(
            "That address is not one of this account's mailboxes.".into(),
        ));
    }

    // Reuse the one send path.
    crate::private_email::send_handler::deliver(
        &state.db,
        tenant_id,
        &req.from_address,
        &to,
        &subject,
        &req.body,
        None,
    )
    .await?;

    let snippet: String = req.body.chars().take(500).collect();
    let sent_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO private_email_messages (
              tenant_id, mailbox_id, address, folder, direction, from_address, to_address,
              subject, body_plain, snippet, in_reply_to, size_bytes, is_read, received_at
           ) VALUES ($1, $2, $3, 'sent', 'outbound', $4, $5, $6, $7, $8, $9, $10, true, NOW())
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(mailbox.map(|m| m.0))
    .bind(&req.from_address)
    .bind(&req.from_address)
    .bind(&to)
    .bind(&subject)
    .bind(&req.body)
    .bind(&snippet)
    .bind(&subject_original)
    .bind(req.body.len() as i32)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({ "sent": true, "message_id": sent_id })))
}

/// POST /api/private-email/folders/action — mark all read / unread, or empty a folder.
///
/// `empty` is destructive by design and reports how many messages it destroyed, so a caller can
/// confirm with a real number instead of a promise (B205).
pub async fn folder_action(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<FolderActionRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let folder = req.folder.trim().to_lowercase();

    match req.action.as_str() {
        "read" | "unread" => {
            let want = req.action == "read";
            let n = sqlx::query(
                "UPDATE private_email_messages SET is_read = $3, updated_at = NOW()
                  WHERE tenant_id = $1
                    AND (folder = $2 OR folder LIKE $2 || '/%')",
            )
            .bind(tenant_id)
            .bind(&folder)
            .bind(want)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?
            .rows_affected();
            Ok(Json(json!({ "updated": n, "is_read": want })))
        }
        "empty" => {
            // Only a holding pen can be emptied. Emptying Inbox would be a bulk delete wearing a
            // different name, and it is not offered anywhere.
            if folder != "trash" && folder != "spam" {
                return Err(AppError::BadRequest(
                    "Only Spam and Trash can be emptied. Messages elsewhere are moved to Trash \
                     first, so nothing is destroyed by one click."
                        .into(),
                ));
            }
            let n = sqlx::query(
                "DELETE FROM private_email_messages WHERE tenant_id = $1 AND folder = $2",
            )
            .bind(tenant_id)
            .bind(&folder)
            .execute(&state.db)
            .await
            .map_err(AppError::Database)?
            .rows_affected();
            Ok(Json(json!({ "destroyed": n, "folder": folder })))
        }
        other => Err(AppError::BadRequest(format!(
            "'{}' is not a folder action. Use read, unread or empty.",
            other
        ))),
    }
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// Labels (B205) — name + colour, per account, applied to one or many messages.
// ────────────────────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LabelRequest {
    pub name: Option<String>,
    pub color: Option<String>,
    #[serde(default)]
    pub message_ids: Option<Vec<Uuid>>,
}

pub async fn list_labels(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let rows = sqlx::query(
        r#"
        SELECT l.id, l.name, l.color, count(ml.message_id) AS used
          FROM private_email_labels l
          LEFT JOIN private_email_message_labels ml ON ml.label_id = l.id
         WHERE l.tenant_id = $1
         GROUP BY l.id, l.name, l.color
         ORDER BY lower(l.name)
        "#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let labels: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.try_get::<Uuid, _>("id").ok(),
                "name": r.try_get::<String, _>("name").unwrap_or_default(),
                "color": r.try_get::<String, _>("color").unwrap_or_default(),
                "used": r.try_get::<i64, _>("used").unwrap_or(0),
            })
        })
        .collect();
    Ok(Json(json!({ "labels": labels })))
}

pub async fn create_label(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<LabelRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let name = req
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| AppError::Validation("A label needs a name.".into()))?;
    if name.chars().count() > 64 {
        return Err(AppError::Validation(
            "A label name is at most 64 characters.".into(),
        ));
    }
    let color = req.color.as_deref().unwrap_or("#64748b");

    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM private_email_labels WHERE tenant_id = $1 AND lower(name) = lower($2)",
    )
    .bind(tenant_id)
    .bind(name)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    if exists.is_some() {
        return Err(AppError::Duplicate(format!(
            "A label called '{}' already exists.",
            name
        )));
    }

    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO private_email_labels (tenant_id, name, color) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(color)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({ "id": id, "name": name, "color": color })))
}

/// PATCH /api/private-email/labels/:id — rename and/or recolour. Never touches messages.
pub async fn update_label(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(label_id): Path<Uuid>,
    Json(req): Json<LabelRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let row = sqlx::query(
        "UPDATE private_email_labels
            SET name = COALESCE($3, name), color = COALESCE($4, color)
          WHERE id = $1 AND tenant_id = $2
        RETURNING id, name, color",
    )
    .bind(label_id)
    .bind(tenant_id)
    .bind(req.name.as_deref().map(str::trim).filter(|n| !n.is_empty()))
    .bind(req.color.as_deref())
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Label not found".into()))?;

    Ok(Json(json!({
        "id": row.try_get::<Uuid, _>("id").ok(),
        "name": row.try_get::<String, _>("name").unwrap_or_default(),
        "color": row.try_get::<String, _>("color").unwrap_or_default(),
    })))
}

/// DELETE /api/private-email/labels/:id — removes the label only. Messages are untouched.
pub async fn delete_label(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(label_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let n = sqlx::query("DELETE FROM private_email_labels WHERE id = $1 AND tenant_id = $2")
        .bind(label_id)
        .bind(tenant_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?
        .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound("Label not found".into()));
    }
    Ok(Json(json!({ "deleted": true })))
}

/// POST /api/private-email/labels/:id/apply — attach the label to one or many messages.
/// DELETE /api/private-email/labels/:id/apply — detach it from one or many.
pub async fn apply_label(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(label_id): Path<Uuid>,
    Json(req): Json<LabelRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let ids = req.message_ids.unwrap_or_default();
    if ids.is_empty() {
        return Err(AppError::Validation(
            "Choose at least one message to label.".into(),
        ));
    }

    let owns: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM private_email_labels WHERE id = $1 AND tenant_id = $2")
            .bind(label_id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;
    if owns.is_none() {
        return Err(AppError::NotFound("Label not found".into()));
    }

    let applied = sqlx::query(
        r#"
        INSERT INTO private_email_message_labels (message_id, label_id)
        SELECT m.id, $3 FROM private_email_messages m
         WHERE m.tenant_id = $1 AND m.id = ANY($2)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(tenant_id)
    .bind(&ids)
    .bind(label_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?
    .rows_affected();

    Ok(Json(json!({ "labeled": applied })))
}

pub async fn remove_label(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(label_id): Path<Uuid>,
    Json(req): Json<LabelRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = account_id(&claims)?;
    let ids = req.message_ids.unwrap_or_default();
    if ids.is_empty() {
        return Err(AppError::Validation(
            "Choose at least one message to unlabel.".into(),
        ));
    }
    let removed = sqlx::query(
        r#"
        DELETE FROM private_email_message_labels ml
         USING private_email_messages m
         WHERE ml.message_id = m.id
           AND ml.label_id = $3
           AND m.tenant_id = $1
           AND m.id = ANY($2)
        "#,
    )
    .bind(tenant_id)
    .bind(&ids)
    .bind(label_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?
    .rows_affected();
    Ok(Json(json!({ "unlabeled": removed })))
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// Helpers
// ────────────────────────────────────────────────────────────────────────────────────────────────

fn account_id(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// One list row. `body_plain` / `body_html` are deliberately absent: a list of 50 messages must not
/// carry 50 full bodies over the wire, and the snippet is what the list actually shows.
fn message_json(row: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": row.try_get::<Uuid, _>("id").ok(),
        "mailbox_id": row.try_get::<Option<Uuid>, _>("mailbox_id").unwrap_or(None),
        "address": row.try_get::<String, _>("address").ok(),
        "folder": row.try_get::<String, _>("folder").ok(),
        "direction": row.try_get::<String, _>("direction").unwrap_or_else(|_| "inbound".into()),
        "from_address": row.try_get::<String, _>("from_address").unwrap_or_default(),
        "from_name": row.try_get::<Option<String>, _>("from_name").unwrap_or(None),
        "to_address": row.try_get::<String, _>("to_address").unwrap_or_default(),
        "cc_address": row.try_get::<Option<String>, _>("cc_address").unwrap_or(None),
        "subject": row.try_get::<String, _>("subject").unwrap_or_default(),
        "snippet": row.try_get::<String, _>("snippet").unwrap_or_default(),
        "is_read": row.try_get::<bool, _>("is_read").unwrap_or(false),
        "is_starred": row.try_get::<bool, _>("is_starred").unwrap_or(false),
        "is_ticket": row.try_get::<bool, _>("is_ticket").unwrap_or(false),
        "ticket_id": row.try_get::<Option<Uuid>, _>("ticket_id").unwrap_or(None),
        "assigned_user_id": row.try_get::<Option<Uuid>, _>("assigned_user_id").unwrap_or(None),
        "has_attachments": row.try_get::<bool, _>("has_attachments").unwrap_or(false),
        "size_bytes": row.try_get::<i32, _>("size_bytes").unwrap_or(0),
        "received_at": row.try_get::<DateTime<Utc>, _>("received_at").ok(),
    })
}

async fn message_labels(
    state: &AppState,
    tenant_id: Uuid,
    message_id: Uuid,
) -> Result<Value, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT l.id, l.name, l.color
          FROM private_email_message_labels ml
          JOIN private_email_labels l ON l.id = ml.label_id
         WHERE ml.message_id = $1 AND l.tenant_id = $2
         ORDER BY lower(l.name)
        "#,
    )
    .bind(message_id)
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(json!(rows
        .iter()
        .map(|r| json!({
            "id": r.try_get::<Uuid, _>("id").ok(),
            "name": r.try_get::<String, _>("name").unwrap_or_default(),
            "color": r.try_get::<String, _>("color").unwrap_or_default(),
        }))
        .collect::<Vec<Value>>()))
}

/// The account's plan storage in GB. 2 GB is the stated default (B206), and a plan that names a
/// size wins. A failed read is the default, never an error: the banner must not hide mail.
async fn plan_storage_gb(state: &AppState, tenant_id: Uuid) -> Option<f64> {
    let row = sqlx::query(
        r#"
        SELECT p.features
          FROM tenant_plans tp
          JOIN plans p ON p.id = tp.plan_id
         WHERE tp.tenant_id = $1 AND tp.status = 'active'
         LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()??;
    let features: Value = row.try_get("features").ok()?;
    let gb = features.get("storage_gb").and_then(|v| v.as_f64())?;
    // A plan naming less than the sold 2 GB minimum is read as the minimum: the inbox exists to
    // receive, and a 0.1 GB free tier would make the meter meaningless.
    Some(gb.max(2.0))
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// The inbound writer — ONE place that stores a received message.
// ────────────────────────────────────────────────────────────────────────────────────────────────

/// Everything the webhook knows about a delivery, already authenticated.
pub struct InboundMessage<'a> {
    pub tenant_id: Uuid,
    pub mailbox_id: Option<Uuid>,
    pub address: &'a str,
    pub from_address: &'a str,
    pub from_name: Option<&'a str>,
    pub to_address: &'a str,
    pub cc_address: Option<&'a str>,
    pub subject: &'a str,
    pub body_plain: &'a str,
    pub body_html: Option<&'a str>,
    pub message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub provider: &'a str,
    pub provider_message_id: Option<&'a str>,
    pub is_spam: bool,
    pub has_attachments: bool,
}

/// Store one received message. Returns (message id, whether this call created it).
///
/// Idempotent on (tenant, Message-Id): a provider retry finds the row already there, updates nothing
/// and gets the SAME id back, so a retry can never become a second copy of the same mail.
pub async fn store_inbound(
    db: &sqlx::PgPool,
    msg: &InboundMessage<'_>,
) -> Result<(Uuid, bool), AppError> {
    let snippet: String = msg.body_plain.chars().take(500).collect();
    let size = (msg.body_plain.len() + msg.body_html.map(str::len).unwrap_or(0)) as i32;
    let folder = if msg.is_spam { "spam" } else { "inbox" };

    let row = sqlx::query(
        r#"
        INSERT INTO private_email_messages (
            tenant_id, mailbox_id, address, folder, direction,
            from_address, from_name, to_address, cc_address, subject,
            body_plain, body_html, snippet, message_id, in_reply_to,
            provider, provider_message_id, size_bytes, has_attachments, received_at
        ) VALUES ($1,$2,$3,$4,'inbound',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,NOW())
        ON CONFLICT (tenant_id, message_id) WHERE message_id IS NOT NULL AND message_id <> ''
        DO NOTHING
        RETURNING id
        "#,
    )
    .bind(msg.tenant_id)
    .bind(msg.mailbox_id)
    .bind(msg.address.to_lowercase())
    .bind(folder)
    .bind(msg.from_address)
    .bind(msg.from_name)
    .bind(msg.to_address)
    .bind(msg.cc_address)
    .bind(msg.subject)
    .bind(msg.body_plain)
    .bind(msg.body_html)
    .bind(&snippet)
    .bind(msg.message_id)
    .bind(msg.in_reply_to)
    .bind(msg.provider)
    .bind(msg.provider_message_id)
    .bind(size)
    .bind(msg.has_attachments)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(r) => Ok((r.try_get::<Uuid, _>("id").unwrap_or_default(), true)),
        None => {
            // Either a retry of a delivery already stored, or no Message-Id at all (the ON CONFLICT
            // clause cannot fire for NULL/empty). Fall back to the newest matching row so the caller
            // still gets an id to attach the ticket to.
            let existing: Option<Uuid> = sqlx::query_scalar(
                r#"
                SELECT id FROM private_email_messages
                 WHERE tenant_id = $1
                   AND address = $2
                   AND from_address = $3
                   AND subject = $4
                   AND received_at > NOW() - INTERVAL '1 day'
                 ORDER BY received_at DESC
                 LIMIT 1
                "#,
            )
            .bind(msg.tenant_id)
            .bind(msg.address.to_lowercase())
            .bind(msg.from_address)
            .bind(msg.subject)
            .fetch_optional(db)
            .await
            .map_err(AppError::Database)?;

            Ok((existing.unwrap_or_default(), false))
        }
    }
}

/// Attach the ticket a message created (or was converted into). Keeps the flag and the link in one
/// statement so a message can never claim to be a ticket while pointing at none.
pub async fn link_ticket(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    ticket_id: Uuid,
) -> Result<(), AppError> {
    if message_id == Uuid::nil() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE private_email_messages
            SET ticket_id = $3, is_ticket = true, updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(message_id)
    .bind(tenant_id)
    .bind(ticket_id)
    .execute(db)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}
