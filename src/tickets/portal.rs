//! CS-22 — the customer-facing "💬 My Support" portal.
//!
//! The guide promises the tenant's *end customers* a portal that shows only their own requests
//! ("Your end customers log in and see 💬 My Support"). Nothing in the app implemented it, and a
//! passwordless login that depends on sending mail cannot be built or proven on this deployment —
//! transactional email is blocked on a missing credential (card t_b28d3432). So the login is a
//! **capability proof that needs no email at all**: the customer types their address plus the
//! reference of a ticket they already have, and the server returns a stateless grant scoped to
//! (tenant, address).
//!
//! Design notes, all deliberate:
//! - The grant is `base64url(address) "." hmac_sha256(secret, tenant:address)`. It carries no
//!   server state, so there is no session table to clean up, and it is **scoped to one tenant**:
//!   the tenant id is inside the MAC, so a grant minted for tenant A is rejected by tenant B.
//! - It travels in the `X-Support-Token` header, never in the URL, so a customer credential can
//!   never land in an access log or a referrer — the same rule as t_a724790f.
//! - Every data statement is filtered by the SAME owner predicate (tenant + the authenticated
//!   address, including tickets linked through `contacts`). A ticket id alone gets a 404.
//!
//! Routes (public — the app is reached through the nginx-proxied `/s/` prefix):
//! - `GET  /s/:tenant_id/support`                     — the portal page
//! - `POST /s/:tenant_id/support/login`               — address + ticket reference -> grant
//! - `GET  /s/:tenant_id/support/tickets`             — the caller's own requests
//! - `GET  /s/:tenant_id/support/tickets/:id`         — one request + its thread
//! - `POST /s/:tenant_id/support/tickets`             — submit a new request
//! - `POST /s/:tenant_id/support/tickets/:id/messages` — reply on the thread

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

use super::models::{Ticket, TicketMessage};

type HmacSha256 = Hmac<Sha256>;

/// The portal page, embedded in the binary at build time. It is a static document — the tenant id
/// is the only server-side substitution — so there is no template engine and no file to publish.
const PORTAL_HTML: &str = include_str!("../../public/support-portal.html");

/// The customer credential travels in this header. Never a query parameter: the access log keeps
/// the query string, so `?token=` would leak the credential into `access.log` (t_a724790f).
const TOKEN_HEADER: &str = "x-support-token";

/// Every customer-visible statement is filtered by this predicate, with `$1` = the tenant and `$2`
/// = the authenticated address. It selects a ticket when it was raised with this address as the
/// contact address, or when it is linked to a contact of this tenant with that address.
const OWNER_PREDICATE: &str = "(lower(coalesce(t.contact_email, '')) = $2 \
     OR t.contact_id IN (SELECT c.id FROM contacts c \
                         WHERE c.tenant_id = t.tenant_id AND lower(c.email) = $2))";

// ── Credential ───────────────────────────────────────────────────────────

/// Domain-separated from the session JWT: the same secret is reused, but neither token can be
/// presented as the other.
fn grant_key(state: &AppState) -> String {
    format!("coreswift-support-portal-v1:{}", state.config.jwt_secret)
}

fn grant_mac(state: &AppState, tenant_id: Uuid, email: &str) -> Result<HmacSha256, AppError> {
    let mut mac = HmacSha256::new_from_slice(grant_key(state).as_bytes())
        .map_err(|e| AppError::Internal(format!("portal grant key: {e}")))?;
    mac.update(format!("{tenant_id}:{email}").as_bytes());
    Ok(mac)
}

fn mint_grant(state: &AppState, tenant_id: Uuid, email: &str) -> Result<String, AppError> {
    let mac = grant_mac(state, tenant_id, email)?;
    let sig = mac.finalize().into_bytes();
    Ok(format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(email),
        hex::encode(sig.as_slice())
    ))
}

/// Recover the authenticated address from a grant, or `None`. The MAC is verified in constant time,
/// and the tenant id is part of the signed message, so this cannot be replayed on another tenant.
fn open_grant(state: &AppState, tenant_id: Uuid, token: &str) -> Option<String> {
    let (email_b64, sig_hex) = token.split_once('.')?;
    let email = String::from_utf8(URL_SAFE_NO_PAD.decode(email_b64).ok()?).ok()?;
    if !email.contains('@') {
        return None;
    }
    let sig = hex::decode(sig_hex).ok()?;
    let mac = grant_mac(state, tenant_id, &email).ok()?;
    mac.verify_slice(&sig).ok()?;
    Some(email)
}

fn authenticated(
    state: &AppState,
    headers: &HeaderMap,
    tenant_id: Uuid,
) -> Result<String, AppError> {
    let raw = headers
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if raw.is_empty() {
        return Err(AppError::Unauthorized);
    }
    open_grant(state, tenant_id, raw).ok_or(AppError::Unauthorized)
}

fn normalise_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

// ── Page ─────────────────────────────────────────────────────────────────

/// `GET /s/:tenant_id/support` — the portal document. The tenant id is public (it is already in
/// every embed snippet), so it is safe to inline; the page stores the grant in `sessionStorage`
/// and sends it as a header, never as a URL.
pub async fn page(Path(tenant_id): Path<Uuid>) -> Response {
    let html = PORTAL_HTML.replace("__TENANT_ID__", &tenant_id.to_string());
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::HeaderName::from_static("x-robots-tag"), "noindex"),
        ],
        html,
    )
        .into_response()
}

// ── Login ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct PortalLogin {
    pub email: String,
    pub reference: String,
}

/// `POST /s/:tenant_id/support/login` — prove ownership of an address with a reference the customer
/// already has, and hand back the grant. The failure answer is a flat 401 for "no such ticket" as
/// well as "wrong address", so the endpoint cannot be used to enumerate tickets.
pub async fn login(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<PortalLogin>,
) -> ApiResult<impl IntoResponse> {
    let email = normalise_email(&body.email);
    let reference = body.reference.trim().to_lowercase();
    if !email.contains('@') {
        return Err(AppError::Validation(
            "Enter the email address you used when you contacted us.".into(),
        ));
    }
    if reference.len() < 8 {
        return Err(AppError::Validation(
            "Enter your request reference — at least the first 8 characters.".into(),
        ));
    }

    let owns: Option<(Uuid,)> = sqlx::query_as(&format!(
        "SELECT t.id FROM tickets t \
         WHERE t.tenant_id = $1 AND t.id::text LIKE $3 || '%' AND {OWNER_PREDICATE} \
         LIMIT 1"
    ))
    .bind(tenant_id)
    .bind(&email)
    .bind(&reference)
    .fetch_optional(&s.db)
    .await?;

    if owns.is_none() {
        return Err(AppError::Unauthorized);
    }

    let tenant_name: Option<String> = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&s.db)
        .await?;

    Ok(Json(json!({
        "token": mint_grant(&s, tenant_id, &email)?,
        "email": email,
        "tenant_name": tenant_name.unwrap_or_default(),
    })))
}

// ── Requests ─────────────────────────────────────────────────────────────

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct PortalTicket {
    pub id: Uuid,
    pub subject: String,
    pub status: String,
    pub priority: String,
    pub source: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub message_count: i64,
}

/// `GET /s/:tenant_id/support/tickets` — the customer's own requests, newest first.
pub async fn list(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let email = authenticated(&s, &headers, tenant_id)?;

    let tickets = sqlx::query_as::<_, PortalTicket>(&format!(
        "SELECT t.id, t.subject, t.status, t.priority, t.source, t.created_at, t.updated_at, \
                (SELECT count(*) FROM ticket_messages m WHERE m.ticket_id = t.id) AS message_count \
         FROM tickets t WHERE t.tenant_id = $1 AND {OWNER_PREDICATE} \
         ORDER BY t.created_at DESC LIMIT 100"
    ))
    .bind(tenant_id)
    .bind(&email)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(
        json!({ "tickets": tickets, "count": tickets.len(), "email": email }),
    ))
}

/// `GET /s/:tenant_id/support/tickets/:id` — one request and its thread. A ticket belonging to
/// somebody else answers 404, never 403: the customer cannot learn that the id exists.
pub async fn detail(
    State(s): State<AppState>,
    Path((tenant_id, ticket_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let email = authenticated(&s, &headers, tenant_id)?;

    let ticket = sqlx::query_as::<_, Ticket>(&format!(
        "SELECT t.* FROM tickets t \
         WHERE t.id = $1 AND t.tenant_id = $3 AND {OWNER_PREDICATE}"
    ))
    .bind(ticket_id)
    .bind(&email)
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Request not found".into()))?;

    let messages = sqlx::query_as::<_, TicketMessage>(
        "SELECT * FROM ticket_messages WHERE ticket_id = $1 ORDER BY created_at ASC",
    )
    .bind(ticket_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "ticket": ticket, "messages": messages })))
}

#[derive(Debug, Deserialize)]
pub struct PortalNewTicket {
    pub subject: String,
    pub message: Option<String>,
    pub priority: Option<String>,
}

fn check_priority(raw: Option<&str>) -> Result<String, AppError> {
    let p = raw.unwrap_or("medium").trim().to_lowercase();
    if ["low", "medium", "high", "urgent"].contains(&p.as_str()) {
        Ok(p)
    } else {
        Err(AppError::Validation(
            "Priority must be low, medium, high or urgent.".into(),
        ))
    }
}

/// `POST /s/:tenant_id/support/tickets` — a new request from the portal. The contact address and
/// name come from the authenticated grant, never from the body, so a customer cannot file a request
/// under somebody else's address.
pub async fn create_ticket(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PortalNewTicket>,
) -> ApiResult<impl IntoResponse> {
    let email = authenticated(&s, &headers, tenant_id)?;
    let subject = body.subject.trim().to_string();
    if subject.is_empty() {
        return Err(AppError::Validation("A subject is required.".into()));
    }
    if subject.len() > 200 {
        return Err(AppError::Validation(
            "Keep the subject under 200 characters.".into(),
        ));
    }
    let priority = check_priority(body.priority.as_deref())?;
    let name = email.split('@').next().unwrap_or("Customer").to_string();

    let ticket = sqlx::query_as::<_, Ticket>(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', $4, 'portal', $5, $6)
           RETURNING *"#,
    )
    .bind(tenant_id)
    .bind(&subject)
    .bind(body.message.unwrap_or_default().trim())
    .bind(&priority)
    .bind(&email)
    .bind(&name)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({ "ticket": ticket }))))
}

#[derive(Debug, Deserialize)]
pub struct PortalReply {
    pub message: String,
}

/// `POST /s/:tenant_id/support/tickets/:id/messages` — the customer's side of the thread. A reply
/// on a resolved/closed request reopens it, which is what a support desk does everywhere else.
pub async fn reply(
    State(s): State<AppState>,
    Path((tenant_id, ticket_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<PortalReply>,
) -> ApiResult<impl IntoResponse> {
    let email = authenticated(&s, &headers, tenant_id)?;
    let message = body.message.trim().to_string();
    if message.is_empty() {
        return Err(AppError::Validation("Write a message first.".into()));
    }

    // Ownership first: 404 (not 403) for a ticket that is not this customer's.
    let owned: Option<(Uuid,)> = sqlx::query_as(&format!(
        "SELECT t.id FROM tickets t WHERE t.id = $1 AND t.tenant_id = $3 AND {OWNER_PREDICATE}"
    ))
    .bind(ticket_id)
    .bind(&email)
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?;
    if owned.is_none() {
        return Err(AppError::NotFound("Request not found".into()));
    }

    let msg = sqlx::query_as::<_, TicketMessage>(
        r#"INSERT INTO ticket_messages (ticket_id, sender_type, message)
           VALUES ($1, 'contact', $2)
           RETURNING *"#,
    )
    .bind(ticket_id)
    .bind(&message)
    .fetch_one(&s.db)
    .await?;

    sqlx::query(
        "UPDATE tickets SET updated_at = NOW(), \
         status = CASE WHEN status IN ('resolved', 'closed') THEN 'open' ELSE status END \
         WHERE id = $1",
    )
    .bind(ticket_id)
    .execute(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({ "message": msg }))))
}
