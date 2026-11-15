use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

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
    // A message filed from the PUBLIC contact page has no workspace (2026-11-06), so no tenant owns it
    // and the workspace-scoped read below could never show it to the one person who can answer. A
    // platform admin's queue therefore ALSO carries those rows; a tenant still sees strictly its own,
    // so nothing widens across workspaces.
    let is_platform = crate::auth::platform_admin::is_platform_admin(&s.db, &c.sub).await?;
    // who "mine" means: the caller, from the session rather than from a query parameter, so a request
    // cannot ask for somebody else's view by naming them.
    let assignee = Uuid::parse_str(&c.sub).ok();

    let tickets = sqlx::query_as::<_, Ticket>(
        r#"SELECT id, tenant_id, subject, description, status, priority, assigned_to, contact_id,
                  number, ai_state, first_response_due, first_responded_at, resolution_due, solved_at,
                  created_at, updated_at, source, contact_email,
                  CASE
                    WHEN tenant_id IS NULL THEN COALESCE(NULLIF(contact_name, ''), 'Website visitor')
                    ELSE contact_name
                  END AS contact_name
           FROM tickets
           WHERE (tenant_id = $1 OR ($7 AND tenant_id IS NULL))
             AND ($2::text IS NULL OR status = $2)
             AND ($3::text IS NULL OR priority = $3)
             AND ($4::uuid IS NULL OR contact_id = $4)
             AND ($10::text IS NULL OR lower(contact_email) = lower($10))
             -- ── SEARCH (David, 2026-11-14) ────────────────────────────────────────────────
             -- One box for the two questions a desk is actually asked: "where is ticket 42?" and "what has
             -- this person sent?". The number arm uses a CASE so the cast only happens when the text really
             -- is a number — an OR chain does not promise which side is evaluated, and a cast of "dana"
             -- would be a 500 instead of an empty result.
             AND (
               $11::text IS NULL
               OR number = CASE WHEN $11 ~ '^[#]?[0-9]+$' THEN replace($11, '#', '')::int ELSE NULL END
               OR ('cs-' || coalesce(number::text, '')) = lower($11)
               OR lower(coalesce(contact_email, '')) LIKE '%' || lower($11) || '%'
               OR lower(coalesce(contact_name, '')) LIKE '%' || lower($11) || '%'
               OR lower(subject) LIKE '%' || lower($11) || '%'
             )
             -- ── THE VIEWS AN AGENT ACTUALLY WORKS FROM (David, 2026-11-14) ────────────────
             -- One predicate list rather than a query per view, so every view is scoped by the same
             -- tenant clause above it: a new view cannot accidentally forget the account.
             AND (
               $8::text IS NULL OR $8::text = 'all'
               -- WHO OWNS THIS is one question with two places to answer it: the ticket's own owner
               -- (the only place a hand-raised ticket can hold one) and the shared inbox's assignment
               -- on the conversation behind it, which is where assignment actually happens today. Read
               -- through both, or "My open" would be empty for every ticket assigned in the inbox —
               -- the exact drift the one-conversation-two-views rule exists to prevent.
               OR ($8::text = 'mine' AND (
                     assigned_to = $9::uuid
                     OR EXISTS (SELECT 1 FROM private_email_messages m
                                 WHERE m.ticket_id = tickets.id AND m.assigned_user_id = $9::uuid)
                   ))
               OR ($8::text = 'unassigned' AND assigned_to IS NULL AND NOT EXISTS (
                     SELECT 1 FROM private_email_messages m
                      WHERE m.ticket_id = tickets.id AND m.assigned_user_id IS NOT NULL
                   ))
               OR ($8::text = 'waiting'    AND status = 'pending')
               OR ($8::text = 'open'       AND status IN ('new', 'open', 'on_hold'))
               OR ($8::text = 'ai'         AND ai_state = 'handed_off')
               -- "Due soon" is the first-response clock: unanswered, still in the queue, and inside the
               -- next four hours. A ticket already breached is included, because work does not stop
               -- being late the moment it is late.
               OR ($8::text = 'due'        AND first_responded_at IS NULL
                                           AND first_response_due IS NOT NULL
                                           AND status IN ('new', 'open', 'pending', 'on_hold')
                                           AND first_response_due <= NOW() + INTERVAL '4 hours')
             )
           ORDER BY
             -- the due view is a work queue: the deadline decides the order, not the priority word
             CASE WHEN $8::text = 'due' THEN first_response_due END ASC NULLS LAST,
             CASE priority
               WHEN 'urgent' THEN 0
               WHEN 'high' THEN 1
               WHEN 'medium' THEN 2
               WHEN 'low' THEN 3
             END,
             created_at DESC
           LIMIT $5 OFFSET $6"#,
    )
    .bind(tid)
    .bind(&q.status)
    .bind(&q.priority)
    .bind(&q.contact_id)
    .bind(limit)
    .bind(offset)
    .bind(is_platform)
    .bind(&q.view)
    .bind(assignee)
    .bind(&q.contact_email)
    .bind(q.q.as_deref().map(str::trim).filter(|v| !v.is_empty()))
    .fetch_all(&s.db)
    .await?;

    // ── ONE CONVERSATION, TWO VIEWS (kanban t_5f314e9a) ──────────────────────────────────────────
    // The assignment lives on the CONVERSATION (`private_email_messages.assigned_user_id`) — the
    // same object the shared inbox writes — and a ticket that came from a message carries no second
    // copy of it. This attaches who is working each ticket by reading that ONE column through the
    // link (`private_email_messages.ticket_id`), so the inbox and the work queue can never disagree
    // about who owns a thread. A ticket raised by hand (no conversation behind it) has no member
    // assignment to show, and says so with `null` rather than an invented name.
    let ids: Vec<Uuid> = tickets.iter().map(|t| t.id).collect();
    let assigned = assigned_members_for(&s.db, &ids).await?;
    let tickets_json: Vec<serde_json::Value> = tickets
        .into_iter()
        .map(|t| {
            let mut v = serde_json::to_value(&t).unwrap_or(serde_json::Value::Null);
            v["assigned_member"] = assigned
                .get(&t.id)
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            v
        })
        .collect();

    Ok(Json(json!({
        "tickets": tickets_json,
        "count": tickets_json.len(),
        // CS-22 — the customer-facing portal URL, derived on the server from the tenant on the
        // token so no front end has to hardcode an account id. The customer signs in there with
        // their address plus this ticket's reference (the first 8 characters of its id).
        "portal_url": format!("/s/{tid}/support"),
        "portal_reference_hint": "the first 8 characters of a ticket id"
    })))
}

/// Who is working each of these tickets, read from the CONVERSATION they came from.
///
/// One query for a whole page of tickets: `DISTINCT ON` picks the newest linked message per ticket,
/// which is the conversation's current owner (a thread that was assigned and reassigned keeps its
/// latest answer). A removed teammate is not reported — their assignment is cleared on removal, and
/// an inactive row would otherwise still paint as "worked by somebody who cannot sign in".
async fn assigned_members_for(
    db: &sqlx::PgPool,
    ticket_ids: &[Uuid],
) -> Result<std::collections::HashMap<Uuid, serde_json::Value>, AppError> {
    let mut out = std::collections::HashMap::new();
    if ticket_ids.is_empty() {
        return Ok(out);
    }
    let rows = sqlx::query(
        r#"SELECT DISTINCT ON (m.ticket_id)
                  m.ticket_id, u.id AS user_id, u.name, u.email
             FROM private_email_messages m
             JOIN users u ON u.id = m.assigned_user_id
            WHERE m.ticket_id = ANY($1)
              AND u.is_active = true
            ORDER BY m.ticket_id, m.received_at DESC"#,
    )
    .bind(ticket_ids)
    .fetch_all(db)
    .await?;

    for r in rows {
        let ticket_id: Uuid = sqlx::Row::get(&r, "ticket_id");
        let name: String = sqlx::Row::try_get(&r, "name").unwrap_or_default();
        out.insert(
            ticket_id,
            json!({
                "id": sqlx::Row::try_get::<Uuid, _>(&r, "user_id").ok(),
                "name": name,
                "email": sqlx::Row::try_get::<Option<String>, _>(&r, "email").ok().flatten(),
            }),
        );
    }
    Ok(out)
}

/// GET /api/tickets/:id
pub async fn get_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let ticket =
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
            .bind(ticket_id)
            .bind(tid)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| AppError::NotFound("Ticket not found".into()))?;

    let messages = sqlx::query_as::<_, TicketMessage>(
        "SELECT * FROM ticket_messages WHERE ticket_id = $1 ORDER BY created_at ASC",
    )
    .bind(ticket_id)
    .fetch_all(&s.db)
    .await?;

    // The SAME assignment the shared inbox writes (kanban t_5f314e9a): one conversation, two views.
    let assigned = assigned_members_for(&s.db, &[ticket_id])
        .await?
        .get(&ticket_id)
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    Ok(Json(json!({
        "ticket": ticket,
        "messages": messages,
        "assigned_member": assigned
    })))
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
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, assigned_to, contact_id, source, contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', $4, $5, $6, COALESCE($7, 'manual'), $8, $9)
           RETURNING *"#
    )
    .bind(tid)
    .bind(&body.subject)
    .bind(&description)
    .bind(&priority)
    .bind(&body.assigned_to)
    .bind(&body.contact_id)
    .bind(&body.source)
    .bind(&body.contact_email)
    .bind(&body.contact_name)
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

    let ticket =
        sqlx::query_as::<_, Ticket>("SELECT * FROM tickets WHERE id = $1 AND tenant_id = $2")
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

    // `solved_at` is the ticket's own record of when it was closed, and the resolution clock reads it.
    // It is cleared when a ticket goes back into the queue, so a reopened ticket cannot carry a solve
    // time from the last time it was closed and report a negative resolution.
    let updated = sqlx::query_as::<_, Ticket>(
        r#"UPDATE tickets
           SET subject = $1, description = $2, status = $3, priority = $4,
               assigned_to = $5, updated_at = NOW(),
               solved_at = CASE
                   WHEN $3 IN ('solved', 'closed') THEN COALESCE(solved_at, NOW())
                   ELSE NULL
               END
           WHERE id = $6 AND tenant_id = $7
           RETURNING *"#,
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

/// POST /api/tickets/:id/merge — fold another ticket's conversation into this one.
///
/// The body names the SURVIVOR (`into`), so the caller is explicit about which number the customer keeps.
#[derive(Debug, serde::Deserialize)]
pub struct MergeBody {
    pub into: uuid::Uuid,
}

pub async fn merge_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
    Json(body): Json<MergeBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let outcome = crate::helpdesk::merge::merge_tickets(&s.db, tid, ticket_id, body.into).await?;
    Ok(Json(serde_json::json!({
        "survivor": outcome.survivor,
        "survivor_number": outcome.survivor_number,
        "merged": outcome.merged,
        "merged_number": outcome.merged_number,
        "messages_moved": outcome.messages_moved,
    })))
}

/// POST /api/tickets/:id/split — pull one message out into a new ticket.
#[derive(Debug, serde::Deserialize)]
pub struct SplitBody {
    pub message_id: uuid::Uuid,
    pub subject: Option<String>,
}

pub async fn split_ticket(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(ticket_id): Path<Uuid>,
    Json(body): Json<SplitBody>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let outcome =
        crate::helpdesk::merge::split_message(&s.db, tid, ticket_id, body.message_id, body.subject)
            .await?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "ticket_id": outcome.ticket_id,
            "number": outcome.number,
            "from_number": outcome.from_number,
            "moved_message": outcome.moved_message,
        })),
    ))
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

    // Verify the ticket is reachable by this caller: their own workspace's, or — for a platform admin
    // — a message filed from the public contact page, which belongs to no workspace (2026-11-06).
    // Without the second arm the operator could see such a row in the queue and still answer 404 to
    // their own reply.
    let is_platform = crate::auth::platform_admin::is_platform_admin(&s.db, &c.sub).await?;
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM tickets WHERE id = $1 AND (tenant_id = $2 OR ($3 AND tenant_id IS NULL)))",
    )
    .bind(ticket_id)
    .bind(tid)
    .bind(is_platform)
    .fetch_one(&s.db)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Ticket not found".into()));
    }

    let sender = body.sender_type.unwrap_or_else(|| "agent".to_string());

    // A NOTE IS NOT A REPLY (David, 2026-11-14). The helpdesk's most valuable writing is the kind a
    // customer never sees: "rang them, invoice is already paid", "this is the third time he has asked".
    // Until the database grew a `kind` there was nowhere to put that, and the outbound mail below fired
    // on the AUTHOR — so any attempt at a private note would have been emailed to the customer. The gate
    // is now the kind, and an unknown kind is refused rather than silently treated as a reply.
    let kind = match body.kind.as_deref().map(str::trim) {
        None | Some("") | Some("reply") => "reply",
        Some("note") => "note",
        Some(other) => {
            return Err(AppError::BadRequest(format!(
                "a ticket message is either a reply or a note, not `{other}`"
            )))
        }
    };

    let msg = sqlx::query_as::<_, TicketMessage>(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, message, kind)
           VALUES ($1, $2, $3, $4) RETURNING *"#,
    )
    .bind(ticket_id)
    .bind(&sender)
    .bind(&body.message)
    .bind(kind)
    .fetch_one(&s.db)
    .await?;

    // Touch the ticket, and record the first time a person answered — the first-response clock the
    // SLA work needs. A note does not answer the customer, so it deliberately does not set it.
    sqlx::query(
        "UPDATE tickets
            SET updated_at = NOW(),
                first_responded_at = CASE
                    WHEN $2 = 'reply' AND $3 = 'agent'
                        THEN COALESCE(first_responded_at, NOW())
                    ELSE first_responded_at
                END
          WHERE id = $1",
    )
    .bind(ticket_id)
    .bind(kind)
    .bind(&sender)
    .execute(&s.db)
    .await?;

    // TELL THE ASKER (2026-11-13). The rows above are the thread and the queue; this is the only
    // thing the person who asked actually receives. A public requester (the buyer who pressed
    // "Request this plan", the customer who cannot sign in) has no account and no portal login, so
    // before this the operator's answer existed only inside the console. Best effort by design: the
    // reply is already recorded, so a mail failure is logged and the 201 kept.
    if sender == "agent" && kind == "reply" {
        let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<i32>)>(
            "SELECT subject, contact_email, contact_name, number FROM tickets WHERE id = $1",
        )
        .bind(ticket_id)
        .fetch_optional(&s.db)
        .await;
        match row {
            Ok(Some((subject, Some(email), name, number))) if !email.trim().is_empty() => {
                if let Err(e) = crate::email::send_support_reply_to_requester(
                    &s.db,
                    &email,
                    name.as_deref().unwrap_or(""),
                    &subject,
                    &body.message,
                    number,
                )
                .await
                {
                    tracing::warn!(ticket = %ticket_id, "support reply notice was not sent: {e}");
                }
            }
            Ok(_) => {}
            Err(e) => {
                tracing::warn!(ticket = %ticket_id, error = %e, "could not read the ticket for the reply notice");
            }
        }
    }

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
    struct StatRow {
        status: String,
        count: Option<i64>,
    }

    let rows = sqlx::query_as::<_, StatRow>(
        r#"SELECT status, COUNT(*) as count
           FROM tickets WHERE tenant_id = $1 GROUP BY status"#,
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let total = rows.iter().fold(0i64, |acc, r| acc + r.count.unwrap_or(0));
    let open = rows
        .iter()
        .find(|r| r.status == "open")
        .and_then(|r| r.count)
        .unwrap_or(0);
    let in_progress = rows
        .iter()
        .find(|r| r.status == "in_progress")
        .and_then(|r| r.count)
        .unwrap_or(0);
    let resolved = rows
        .iter()
        .find(|r| r.status == "resolved")
        .and_then(|r| r.count)
        .unwrap_or(0);
    let closed = rows
        .iter()
        .find(|r| r.status == "closed")
        .and_then(|r| r.count)
        .unwrap_or(0);

    Ok(Json(json!({
        "total": total,
        "open": open,
        "in_progress": in_progress,
        "resolved": resolved,
        "closed": closed
    })))
}

// ── Public: the served contact page (no auth, no workspace) ───────────

/// How many messages ONE address may file through the public contact page per hour.
///
/// The arm is anonymous, so without a bound one script could fill the operator's queue. Three is
/// enough for a person writing about a lost password, then a correction.
const PUBLIC_SUPPORT_HOURLY_LIMIT: i64 = 3;

/// Trim and cap a free-text field by CHARACTERS (not bytes): the cap is the client's to exceed and
/// must never split a multi-byte character.
fn cap_chars(raw: &str, max: usize) -> String {
    raw.trim()
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// POST /api/public/support-request — a visitor who cannot sign in reaches the team (2026-11-06).
///
/// MEASURED (dns.google and cloudflare-dns.com agree): MX = 0 for this app's domain and every
/// sibling's, and nothing on this host listens on port 25, so every "write to support@…" sentence this
/// app printed sent a customer's message into a bounce — and a locked-out customer, the person who
/// needs help most, had no way to reach anyone.
///
/// This arm files the message into the operator's EXISTING queue: one ticket with no workspace
/// (`tenant_id` NULL, migration 122), the address the visitor typed in `contact_email` (the column this
/// app already answers a form request on), source `form`. A platform admin's ticket queue shows those
/// rows labelled "Website visitor", so the person who can help sees them without a new screen.
///
/// Guards, all refusing BEFORE a row is written: a syntactically real address, a non-empty bounded
/// message, at most three messages from one address per hour (429), and idempotence on an already open
/// ticket with the same address and subject.
pub async fn public_support_request(
    State(s): State<AppState>,
    Json(body): Json<super::models::PublicSupportRequest>,
) -> ApiResult<impl IntoResponse> {
    let email = crate::security::email_addr::normalize(&body.email)
        .map_err(|e| AppError::BadRequest(format!("{e} — we need an address we can reply to")))?;
    let name = cap_chars(body.name.as_deref().unwrap_or(""), 120);
    let message = cap_chars(
        body.body
            .as_deref()
            .or(body.message.as_deref())
            .unwrap_or(""),
        4000,
    );
    if message.is_empty() {
        return Err(AppError::Validation(
            "Write your message so the team knows what you need.".into(),
        ));
    }
    let subject = cap_chars(&body.subject, 200);
    if subject.is_empty() {
        return Err(AppError::Validation(
            "Give your message a short subject so the team knows what it is about.".into(),
        ));
    }

    let recent: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM tickets \
          WHERE tenant_id IS NULL AND lower(contact_email) = $1 \
            AND created_at > now() - interval '1 hour'",
    )
    .bind(&email)
    .fetch_one(&s.db)
    .await?;
    if recent >= PUBLIC_SUPPORT_HOURLY_LIMIT {
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({
                "status": 429,
                "error": "You have sent us several messages in the last hour. Please wait a little, or reply to the email we already have from you.",
                "retry_after_seconds": 3600
            })),
        ));
    }

    // A second identical message is not a second request: the form stays pressable after a send, so a
    // visitor who pressed twice must not mint a twin the operator has to work twice.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tickets \
          WHERE tenant_id IS NULL AND lower(contact_email) = $1 \
            AND lower(subject) = lower($2) AND status IN ('open','in_progress') \
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(&email)
    .bind(&subject)
    .fetch_optional(&s.db)
    .await?;
    if let Some(id) = existing {
        return Ok((
            StatusCode::OK,
            Json(json!({
                "status": "existing",
                "ticket_id": id,
                "message": "We already have your message with that subject — the team will reply to the address you gave."
            })),
        ));
    }

    // `source = 'form'` is the value the schema's own check constraint allows for a form submission;
    // the no-workspace marker is `tenant_id IS NULL`, which is what the queue labels on.
    let ticket_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           VALUES (NULL, $1, $2, 'open', 'medium', 'form', $3, $4)
           RETURNING id"#,
    )
    .bind(&subject)
    .bind(&message)
    .bind(&email)
    .bind(if name.is_empty() { None } else { Some(name.as_str()) })
    .fetch_one(&s.db)
    .await?;

    // TELL THE OPERATOR (2026-11-09). The row above is the record the queue reads; this is the
    // notification that makes anyone read it. Until it existed a locked-out customer's only way back
    // in was a console nobody had been told to open. Best effort by design: the ticket is already
    // written, so a mail failure is logged and the 201 kept (never lose a filed request to a mailer).
    if let Err(e) =
        crate::email::send_support_request_operator_alert(&s.db, &name, &email, &subject, &message)
            .await
    {
        tracing::warn!(
            ticket = %ticket_id,
            "public support request operator alert was not sent: {e}"
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "open",
            "ticket_id": ticket_id,
            "message": "Your message is with our team. We will reply to the address you gave."
        })),
    ))
}

/// File the tier a visitor picked on the price list, at the moment their account is minted
/// (kanban t_71a3a39f).
///
/// WHY IT IS THE PUBLIC INTAKE AND NOT A WORKSPACE TICKET: the console's own "Request this plan" card
/// files through `POST /api/public/support-request`, i.e. one `tickets` row with NO workspace
/// (`tenant_id IS NULL`) — the arm the operator's own queue carries (`list_tickets`: a platform admin
/// sees `tenant_id = own OR tenant_id IS NULL`, so a ticket owned by the CUSTOMER's workspace is
/// visible only to that customer). A brand-new Free workspace is refused 402 by `POST /api/tickets`
/// anyway, and it has no ticket permission to file with. So this is not a shortcut: it is the SAME
/// request the Upgrade card files, filed one step earlier — the operator reads it in the same queue,
/// labelled "Website visitor", with the signup address as the reply-to.
///
/// Nothing is charged and no plan changes here. An unknown slug, `free`, an inactive tier
/// (`inbox` ships inactive until a payment page exists) or a zero-price row files NOTHING and says so
/// in the log: the account is already real, so a bad slug must never turn a finished signup into an
/// error page.
pub async fn file_signup_plan_request(
    db: &sqlx::PgPool,
    email: &str,
    name: &str,
    raw_slug: &str,
) -> Result<Option<serde_json::Value>, AppError> {
    let slug = raw_slug.trim().to_lowercase();
    if slug.is_empty() || slug == "free" {
        return Ok(None);
    }
    let plan: Option<(String, String)> = sqlx::query_as(
        "SELECT slug, name FROM plans \
          WHERE slug = $1 AND COALESCE(is_active, true) = true AND COALESCE(price_monthly, 0) > 0",
    )
    .bind(&slug)
    .fetch_optional(db)
    .await?;
    let Some((plan_slug, plan_name)) = plan else {
        tracing::warn!(
            plan = %slug,
            "signup named a plan that is not on sale — no plan request filed"
        );
        return Ok(None);
    };

    let subject = format!("Plan request at signup: {plan_name} plan");
    let message = format!(
        "This workspace picked the {plan_name} plan on the price list and then created the account. \
         Self-serve checkout is not open on this installation, so the switch has to be made by the \
         CoreSwift team. Nothing was charged.\n\nPlan: {plan_name} ({plan_slug})\n\
         Signed up as: {email}"
    );
    // Idempotent on (address, subject, still open) — the same dedupe the public form uses. A dropped
    // connection on the signup page is retried by real people, and a second ask for the same tier is
    // noise the operator has to work twice.
    let filed: Option<Uuid> = sqlx::query_scalar(
        r#"INSERT INTO tickets
               (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           SELECT NULL, $1, $2, 'open', 'medium', 'form', $3, $4
            WHERE NOT EXISTS (
                SELECT 1 FROM tickets
                 WHERE tenant_id IS NULL AND lower(contact_email) = lower($3)
                   AND lower(subject) = lower($1) AND status IN ('open','in_progress'))
           RETURNING id"#,
    )
    .bind(&subject)
    .bind(&message)
    .bind(email)
    .bind(if name.trim().is_empty() {
        None
    } else {
        Some(name.trim())
    })
    .fetch_optional(db)
    .await?;

    let Some(ticket_id) = filed else {
        tracing::info!(
            plan = %plan_slug,
            address = %email,
            "signup plan request already on the queue — not filed twice"
        );
        return Ok(Some(json!({
            "plan_slug": plan_slug,
            "plan_name": plan_name,
            "status": "existing"
        })));
    };

    // Tell the OPERATOR, through the one implementation the contact page uses (best effort: the row
    // above is the record, so a mail failure must never undo a finished signup). A synthetic address
    // (a proof run) is withheld by that function itself, before any mail is queued.
    if let Err(e) =
        crate::email::send_support_request_operator_alert(db, name, email, &subject, &message).await
    {
        tracing::warn!(
            ticket = %ticket_id,
            "signup plan-request operator alert was not sent: {e}"
        );
    }

    Ok(Some(json!({
        "plan_slug": plan_slug,
        "plan_name": plan_name,
        "ticket_id": ticket_id,
        "status": "filed"
    })))
}

/// POST /api/public/support-answer — the person who wrote from the contact page reads our answer.
///
/// The filing arm above (2026-11-06) writes ONE ticket with no workspace and the address the visitor
/// typed. The operator answers it through the existing queue, which writes the words into
/// `ticket_messages` (`sender_type = 'agent'`) — a thread only a SIGNED-IN caller could load. The
/// person who filed from the contact page is, by definition, the one who cannot sign in, so they
/// hand-wrote their problem and then had no way to read the answer. This route is that way.
///
/// The capability is the pair: the ticket's own uuid (given to the visitor when the form was sent)
/// plus the address they filed with. Both must match the row, and every mismatch — unknown
/// reference, malformed reference, another person's reference — answers the SAME neutral sentence,
/// so the route cannot be used to test whether a reference exists.
pub async fn public_support_answer(
    State(s): State<AppState>,
    Json(body): Json<super::models::PublicSupportAnswer>,
) -> ApiResult<impl IntoResponse> {
    let missing = || {
        AppError::NotFound(
            "We have no message from that address with that reference. Check the reference and the \
             email address you filed with."
                .to_string(),
        )
    };
    let id = Uuid::parse_str(body.ticket_id.trim()).map_err(|_| missing())?;
    // The filing arm stores the address NORMALISED, so the read normalises the same way: a visitor
    // who types `Probe@Example.com ` must find the row filed as `probe@example.com`.
    let email = crate::security::email_addr::normalize(&body.email).map_err(|_| missing())?;

    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT subject, status FROM tickets \
          WHERE id = $1 AND tenant_id IS NULL AND lower(contact_email) = $2",
    )
    .bind(id)
    .bind(&email)
    .fetch_optional(&s.db)
    .await?;
    let Some((subject, status)) = row else {
        return Err(missing());
    };

    let messages: Vec<(String, String, String)> = sqlx::query_as(
        // public_support_answer is reachable with a ticket id and an email address: it is the
        // customer's own view, so an internal note must not be in it.
        "SELECT sender_type, message, created_at::text FROM ticket_messages \
          WHERE ticket_id = $1 AND kind NOT IN ('note', 'event') ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;

    // `from` is what the page prints in front of each bubble: our words vs the visitor's own. An
    // empty thread is honest (`answered: false`) rather than an empty green box.
    let answered = messages.iter().any(|(sender, _, _)| sender == "agent");
    let thread: Vec<serde_json::Value> = messages
        .into_iter()
        .map(|(sender, message, at)| {
            json!({
                "from": if sender == "agent" { "us" } else { "you" },
                "message": message,
                "at": at,
            })
        })
        .collect();

    Ok((
        StatusCode::OK,
        Json(json!({
            "id": id.to_string(),
            "subject": subject,
            "status": status,
            "answered": answered,
            "messages": thread,
        })),
    ))
}

// ── Public Contact Form (no auth) ─────────────────────────────────────

/// POST /api/public/contact — creates a ticket without auth
pub async fn public_contact_form(
    State(s): State<AppState>,
    Json(body): Json<super::models::ContactFormRequest>,
) -> ApiResult<impl IntoResponse> {
    let priority = body.priority.unwrap_or_else(|| "medium".to_string());
    let name = body.name.unwrap_or_else(|| "Website Visitor".to_string());
    let email = body.email.unwrap_or_default();

    let ticket = sqlx::query_as::<_, super::models::Ticket>(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', $4, 'form', $5, $6)
           RETURNING *"#
    )
    .bind(body.tenant_id)
    .bind(&body.subject)
    .bind(&body.message)
    .bind(&priority)
    .bind(&email)
    .bind(&name)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(ticket))))
}

// ── Public: Tenant-scoped ticket submit (no auth) ─────────────────────

/// POST /s/:tid/ticket — embedded form submission
pub async fn public_submit_ticket(
    State(s): State<AppState>,
    Path(tenant_id): Path<uuid::Uuid>,
    Json(body): Json<super::models::PublicTicketRequest>,
) -> ApiResult<impl IntoResponse> {
    let priority = body.priority.unwrap_or_else(|| "medium".to_string());
    let name = body.name.unwrap_or_else(|| "Website Visitor".to_string());
    let email = body.email.unwrap_or_default();

    let ticket = sqlx::query_as::<_, super::models::Ticket>(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', $4, 'form', $5, $6)
           RETURNING *"#
    )
    .bind(tenant_id)
    .bind(&body.subject)
    .bind(&body.message)
    .bind(&priority)
    .bind(&email)
    .bind(&name)
    .fetch_one(&s.db)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "received",
            "ticket_id": ticket.id,
            // the widget tells the visitor this, so they have something to quote in a reply
            "number": ticket.number,
        })),
    ))
}
pub async fn support_embed_script(
    State(_s): State<AppState>,
    Path(tenant_id): Path<uuid::Uuid>,
) -> impl IntoResponse {
    // A CONVERSATION, NOT A FORM (David, 2026-11-14). The widget used to open with three fields and a Send
    // button, which asks a stranger to do the work before they have been helped. It now opens with a greeting
    // and a message box: the visitor says what is wrong, we answer from the published articles if we can, and
    // the form appears only when a person is actually needed — with the whole conversation attached, so the
    // agent reads what was said instead of a single flattened line.
    let js = format!(
        r#"(function(){{
  var d=document;
  if(d.getElementById('crm-support-root'))return;
  var tid='{tid}';
  // The widget is served by whichever host the customer embedded it from, so the API base comes from this
  // script's own URL rather than a hardcoded host.
  var me=d.currentScript;
  var BASE=(me&&me.src?me.src:'').replace(/\/s\/[^\/]+\/widget\.js.*$/,'')||'https://coreswiftcrm.com';

  var S={{said:[], first:'', stage:'chat', answered:false}};

  var s=d.createElement('style');
  s.textContent='.csb-fab{{position:fixed;bottom:20px;right:20px;z-index:9999;display:flex;align-items:center;gap:8px;background:#0ea5e9;color:#fff;border:none;border-radius:50px;padding:14px 22px;font:600 15px -apple-system,system-ui,sans-serif;cursor:pointer;box-shadow:0 4px 20px rgba(14,165,233,.35)}}' +
    '.csb-fab:hover{{transform:scale(1.04)}}' +
    '.csb-fab-ico{{font-size:17px}}' +
    '.csb-panel{{position:fixed;bottom:88px;right:20px;z-index:9998;width:378px;max-width:calc(100vw - 32px);height:520px;max-height:calc(100vh - 120px);background:#fff;border-radius:16px;box-shadow:0 12px 44px rgba(15,23,42,.18);display:none;overflow:hidden;font:14px/1.5 -apple-system,system-ui,sans-serif;color:#0f172a}}' +
    '.csb-panel.on{{display:flex;flex-direction:column}}' +
    '.csb-head{{background:#0ea5e9;color:#fff;padding:14px 16px;display:flex;align-items:center;justify-content:space-between;flex:0 0 auto}}' +
    '.csb-title{{font-weight:600;font-size:15px}}' +
    '.csb-x{{background:none;border:none;color:#fff;font-size:22px;line-height:1;cursor:pointer;padding:0 4px}}' +
    '.csb-log{{flex:1 1 auto;overflow-y:auto;padding:14px;background:#f8fafc}}' +
    '.csb-b{{max-width:86%;padding:9px 12px;border-radius:14px;margin-bottom:9px;white-space:pre-wrap;word-wrap:break-word}}' +
    '.csb-you{{margin-left:auto;background:#0ea5e9;color:#fff;border-bottom-right-radius:4px}}' +
    '.csb-us{{background:#fff;border:1px solid #e2e8f0;border-bottom-left-radius:4px}}' +
    '.csb-art{{display:block;width:100%;text-align:left;background:#fff;border:1px solid #bae6fd;color:#0369a1;border-radius:10px;padding:8px 10px;margin:0 0 7px;font:inherit;cursor:pointer}}' +
    '.csb-art:hover{{background:#f0f9ff}}' +
    '.csb-chip{{display:inline-block;background:#fff;border:1px solid #cbd5e1;border-radius:20px;padding:7px 13px;margin:0 6px 8px 0;font:inherit;cursor:pointer;color:#0f172a}}' +
    '.csb-chip:hover{{border-color:#0ea5e9;color:#0369a1}}' +
    '.csb-form{{flex:0 0 auto;padding:0 14px;background:#f8fafc}}' +
    '.csb-form input{{width:100%;box-sizing:border-box;padding:9px 11px;margin-bottom:7px;border:1px solid #e2e8f0;border-radius:9px;font:inherit;outline:none}}' +
    '.csb-form input:focus{{border-color:#0ea5e9;box-shadow:0 0 0 3px rgba(14,165,233,.15)}}' +
    '.csb-go{{width:100%;background:#0ea5e9;color:#fff;border:none;border-radius:9px;padding:11px;font:600 14px inherit;cursor:pointer;margin-bottom:10px}}' +
    '.csb-go:disabled{{opacity:.6;cursor:default}}' +
    '.csb-row{{flex:0 0 auto;display:flex;gap:8px;padding:10px 14px 14px;border-top:1px solid #e2e8f0;background:#fff}}' +
    '.csb-in{{flex:1 1 auto;padding:10px 12px;border:1px solid #e2e8f0;border-radius:10px;font:inherit;outline:none;min-width:0}}' +
    '.csb-in:focus{{border-color:#0ea5e9;box-shadow:0 0 0 3px rgba(14,165,233,.15)}}' +
    '.csb-send{{background:#0ea5e9;color:#fff;border:none;border-radius:10px;padding:0 16px;font:600 14px inherit;cursor:pointer}}' +
    '.csb-send:disabled{{opacity:.6;cursor:default}}' +
    '.csb-note{{font-size:11px;color:#64748b;padding:0 2px 8px}}';
  d.head.appendChild(s);

  var fab=d.createElement('button');
  fab.type='button'; fab.className='csb-fab'; fab.setAttribute('aria-label','Open support chat');
  fab.innerHTML='<span class="csb-fab-ico">💬</span><span>Support</span>';
  d.body.appendChild(fab);

  var p=d.createElement('div');
  p.id='crm-support-root'; p.className='csb-panel'; p.setAttribute('role','dialog');
  p.setAttribute('aria-label','Support');
  p.innerHTML='<div class="csb-head"><div class="csb-title">Support</div>' +
    '<button class="csb-x" type="button" aria-label="Close support">×</button></div>' +
    '<div class="csb-log" id="csb-log" role="log" aria-live="polite"></div>' +
    '<div class="csb-form" id="csb-form"></div>' +
    '<div class="csb-row"><input class="csb-in" id="csb-in" placeholder="Write a message…" autocomplete="off" aria-label="Your message">' +
    '<button class="csb-send" id="csb-send" type="button">Send</button></div>';
  d.body.appendChild(p);

  var log=p.querySelector('#csb-log'), input=p.querySelector('#csb-in'),
      sendBtn=p.querySelector('#csb-send'), formSlot=p.querySelector('#csb-form');

  function esc(v){{return String(v==null?'':v).replace(/[&<>"']/g,function(c){{
    return {{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[c]}})}}

  function scroll(){{log.scrollTop=log.scrollHeight}}

  function bubble(who,text){{
    var el=d.createElement('div');
    el.className='csb-b '+(who==='you'?'csb-you':'csb-us');
    el.textContent=text;
    log.appendChild(el); scroll();
    if(who==='you'){{S.said.push(text)}}
    return el;
  }}

  function toggle(){{
    var open=p.classList.toggle('on');
    fab.style.display=open?'none':'flex';
    if(open){{
      if(!log.childNodes.length){{
        bubble('us','Hi 👋 What can we help you with today?');
      }}
      input.focus();
    }}
  }}

  fab.onclick=toggle;
  p.querySelector('.csb-x').onclick=toggle;

  // ── searching what we already know ─────────────────────────────────────────────────────
  function search(text){{
    fetch(BASE+'/api/public/helpdesk/kb?w='+encodeURIComponent(tid)+'&q='+encodeURIComponent(text))
      .then(function(r){{return r.json()}})
      .then(function(res){{
        var list=(res&&res.articles)||[];
        if(!list.length){{ offerHuman('I could not find an answer for that.'); return }}
        var el=bubble('us','A couple of answers that might help before a person does:');
        list.slice(0,3).forEach(function(a){{
          var b=d.createElement('button');
          b.type='button'; b.className='csb-art'; b.textContent=a.title;
          b.onclick=function(){{
            var body=d.createElement('div');
            body.className='csb-b csb-us';
            body.innerHTML='<strong>'+esc(a.title)+'</strong><div style="margin-top:6px">'+
              esc(a.body_md).replace(/\n/g,'<br>')+'</div>';
            log.appendChild(body); scroll();
          }};
          log.appendChild(b);
        }});
        scroll();
        chip('That answered it — thanks', function(){{ bubble('us','Glad it helped. If anything else comes up, just write here.'); }});
        chip('Talk to a person', function(){{ offerHuman('Of course.'); }});
      }})
      .catch(function(){{ offerHuman('') }});
  }}

  function chip(label,fn){{
    var b=d.createElement('button');
    b.type='button'; b.className='csb-chip'; b.textContent=label; b.onclick=fn;
    log.appendChild(b); scroll();
  }}

  // ── asking for a person ────────────────────────────────────────────────────────────────
  function offerHuman(prefix){{
    if(S.stage==='human')return;
    S.stage='human';
    if(prefix){{ bubble('us', prefix + ' Leave your name and email and a person will reply by email.'); }}
    else {{ bubble('us','Leave your name and email and a person will reply by email.'); }}
    var name=d.createElement('input'); name.id='csb-name'; name.placeholder='Your name';
    name.setAttribute('aria-label','Your name');
    var mail=d.createElement('input'); mail.id='csb-email'; mail.type='email';
    mail.placeholder='you@example.com'; mail.setAttribute('aria-label','Your email');
    var go=d.createElement('button'); go.type='button'; go.className='csb-go'; go.textContent='Send to a person';
    go.onclick=function(){{ submit(name.value, mail.value, go) }};
    formSlot.innerHTML='';
    formSlot.appendChild(name); formSlot.appendChild(mail); formSlot.appendChild(go);
    name.focus();
  }}

  function transcript(){{
    if(!S.said.length) return '(the visitor sent no messages)';
    return 'Conversation from the website widget:\n\n' +
      S.said.map(function(t){{return '- ' + t}}).join('\n');
  }}

  function submit(name, email, btn){{
    if(!email || email.indexOf('@')<1){{ alert('Please add an email address so we can reply.'); return }}
    btn.disabled=true; btn.textContent='Sending…';
    fetch(BASE+'/s/'+tid+'/ticket',{{
      method:'POST', headers:{{'Content-Type':'application/json'}},
      body:JSON.stringify({{
        subject:(S.first||'Support request').slice(0,110),
        message:transcript(),
        name:name||'Website Visitor',
        email:email,
        priority:'medium'
      }})
    }})
      .then(function(r){{return r.json()}})
      .then(function(res){{
        formSlot.innerHTML='';
        S.stage='done';
        var ref=res&&res.number?(' Your reference is CS-'+res.number+'.'):'';
        bubble('us','Thank you — we have it.'+ref+' We will reply to '+email+'.');
        input.disabled=true; sendBtn.disabled=true;
      }})
      .catch(function(e){{ btn.disabled=false; btn.textContent='Send to a person';
        alert('That did not send: '+e.message) }});
  }}

  // ── sending a message ──────────────────────────────────────────────────────────────────
  function say(){{
    var text=(input.value||'').trim();
    if(!text)return;
    input.value='';
    if(!S.first)S.first=text;
    bubble('you', text);
    if(S.stage==='chat'||S.stage==='human'){{
      // one search per message: the visitor's own words are the query, no tagging needed
      if(S.stage==='chat') search(text);
    }}
  }}

  sendBtn.onclick=say;
  input.addEventListener('keydown', function(e){{
    if(e.key==='Enter' && !e.shiftKey){{ e.preventDefault(); say() }}
  }});
}})();"#,
        tid = tenant_id
    );

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        js,
    )
}
