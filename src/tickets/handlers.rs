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

    let tickets = sqlx::query_as::<_, Ticket>(
        r#"SELECT id, tenant_id, subject, description, status, priority, assigned_to, contact_id,
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
           ORDER BY
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
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({
        "tickets": tickets,
        "count": tickets.len(),
        // CS-22 — the customer-facing portal URL, derived on the server from the tenant on the
        // token so no front end has to hardcode an account id. The customer signs in there with
        // their address plus this ticket's reference (the first 8 characters of its id).
        "portal_url": format!("/s/{tid}/support"),
        "portal_reference_hint": "the first 8 characters of a ticket id"
    })))
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

    let updated = sqlx::query_as::<_, Ticket>(
        r#"UPDATE tickets
           SET subject = $1, description = $2, status = $3, priority = $4,
               assigned_to = $5, updated_at = NOW()
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
    let msg = sqlx::query_as::<_, TicketMessage>(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, message)
           VALUES ($1, $2, $3) RETURNING *"#,
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

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "open",
            "ticket_id": ticket_id,
            "message": "Your message is with our team. We will reply to the address you gave."
        })),
    ))
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
        "SELECT sender_type, message, created_at::text FROM ticket_messages \
          WHERE ticket_id = $1 ORDER BY created_at ASC",
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
        Json(json!({"status": "received", "ticket_id": ticket.id})),
    ))
}

// ── Public: Embed script (no auth) ────────────────────────────────────

/// GET /s/:tid/widget.js — returns the embeddable widget
pub async fn support_embed_script(
    State(_s): State<AppState>,
    Path(tenant_id): Path<uuid::Uuid>,
) -> impl IntoResponse {
    let js = format!(
        r#"(function(){{
  var d=document;
  if(d.getElementById('crm-support-root'))return;
  var tid='{tid}';
  // The widget is served by whichever host the customer embedded it from, so the API base is taken
  // from this script's own URL. It used to be hardcoded to coreswiftcrm.com — a host whose vhost had
  // no `/s/` proxy at all — so the advertised one-line embed could not post a ticket from anywhere.
  var me=d.currentScript;
  var BASE=(me&&me.src?me.src:'').replace(/\/s\/[^\/]+\/widget\.js.*$/,'')||'https://coreswiftcrm.com';

  // Styles
  var s=d.createElement('style');
  s.textContent='.crm-support-btn{{position:fixed;bottom:20px;right:20px;z-index:9999;background:#0ea5e9;color:#fff;border:none;border-radius:50px;padding:14px 24px;font-size:15px;font-family:-apple-system,system-ui,sans-serif;cursor:pointer;box-shadow:0 4px 20px rgba(14,165,233,0.35);transition:transform .2s,box-shadow .2s}}.crm-support-btn:hover{{transform:scale(1.05);box-shadow:0 6px 28px rgba(14,165,233,0.5)}}.crm-support-panel{{position:fixed;bottom:90px;right:20px;z-index:9998;width:360px;max-width:calc(100vw-40px);background:#fff;border-radius:16px;box-shadow:0 8px 40px rgba(0,0,0,0.15);display:none;overflow:hidden;font-family:-apple-system,system-ui,sans-serif;color:#1e293b}}.crm-support-panel.open{{display:block}}.crm-support-header{{background:#0ea5e9;color:#fff;padding:20px;font-weight:600;font-size:16px}}.crm-support-body{{padding:20px}}.crm-support-field{{margin-bottom:14px}}.crm-support-field label{{display:block;font-size:13px;font-weight:500;margin-bottom:4px;color:#475569}}.crm-support-field input,.crm-support-field textarea,.crm-support-field select{{width:100%;padding:10px 12px;border:1px solid #e2e8f0;border-radius:8px;font-size:14px;font-family:inherit;box-sizing:border-box;outline:none;transition:border .2s}}.crm-support-field input:focus,.crm-support-field textarea:focus,.crm-support-field select:focus{{border-color:#0ea5e9;box-shadow:0 0 0 3px rgba(14,165,233,0.15)}}.crm-support-field textarea{{resize:vertical;min-height:90px}}.crm-support-submit{{width:100%;background:#0ea5e9;color:#fff;border:none;border-radius:8px;padding:12px;font-size:14px;font-weight:600;cursor:pointer;transition:background .2s}}.crm-support-submit:hover{{background:#0284c7}}.crm-support-submit:disabled{{opacity:0.6;cursor:default}}.crm-support-thanks{{display:none;text-align:center;padding:40px 20px}}.crm-support-thanks.show{{display:block}}.crm-support-thanks h3{{color:#0ea5e9;margin:0 0 8px}}';
  d.head.appendChild(s);

  // Button
  var b=d.createElement('button');
  b.className='crm-support-btn';
  b.textContent='💬 Support';
  b.onclick=function(){{
    var p=d.getElementById('crm-support-panel');
    if(!p){{renderPanel();return}}
    p.classList.toggle('open');
  }};
  d.body.appendChild(b);

  function renderPanel(){{
    var p=d.createElement('div');
    p.id='crm-support-panel';
    p.className='crm-support-panel open';
    p.innerHTML='<div class="crm-support-header">How can we help?</div><div class="crm-support-body" id="crm-support-form"><div class="crm-support-field"><label>Your name</label><input id="cs-name" placeholder="Jane Smith"></div><div class="crm-support-field"><label>Email</label><input id="cs-email" type="email" placeholder="you@example.com"></div><div class="crm-support-field"><label>What do you need help with?</label><input id="cs-subject" placeholder="Brief summary"></div><div class="crm-support-field"><label>Details</label><textarea id="cs-message" placeholder="Describe your issue or question..."></textarea></div><div class="crm-support-field"><label>Priority</label><select id="cs-priority"><option value="medium">Medium — normal response</option><option value="low">Low — not urgent</option><option value="high">High — need help soon</option><option value="urgent">Urgent — critical issue</option></select></div><button class="crm-support-submit" id="cs-submit" onclick="submitSupport()">Send</button></div><div class="crm-support-thanks" id="crm-support-thanks"><h3>✓ Received!</h3><p>We will get back to you shortly.</p></div>';
    d.body.appendChild(p);
  }}

  window.submitSupport=function(){{
    var btn=d.getElementById('cs-submit');
    var subject=d.getElementById('cs-subject').value;
    if(!subject){{alert('Please enter a subject');return}}
    btn.disabled=true;btn.textContent='Sending...';
    var payload={{subject:subject,message:d.getElementById('cs-message').value||'',name:d.getElementById('cs-name').value||null,email:d.getElementById('cs-email').value||null,priority:d.getElementById('cs-priority').value||'medium'}};
    fetch(BASE+'/s/'+tid+'/ticket',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify(payload)}})
      .then(function(r){{return r.json()}})
      .then(function(){{d.getElementById('crm-support-form').style.display='none';d.getElementById('crm-support-thanks').className='crm-support-thanks show'}})
      .catch(function(e){{alert('Error: '+e.message);btn.disabled=false;btn.textContent='Send'}});
  }};
}})();"#,
        tid = tenant_id
    );

    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        js,
    )
}
