//! The agent door: one credential-agnostic write surface for an AI agent working inside an account.
//!
//! David, 2026-11-25: "are they all read and write?" — they were not. The MCP could read 150 entities and
//! change four payment settings, and the reason it could not do more was measured, not assumed: the
//! console routes refuse an issued API key with `this route needs a console session — an API key cannot be
//! used here`. That refusal is DELIBERATE (auth/boundary.rs, plan §6.2/§8): a key must never reach a session
//! door such as the key-reveal route.
//!
//! So this module does what `custom_objects` already does for the same problem — it resolves EITHER
//! credential itself (`Bearer <csk_…>` or a console session) to exactly one account, and only the routes
//! listed here accept a key. The console's own routes stay session-only; nothing is widened.
//!
//! EVERY handler below is a thin wrapper: it builds the same `Claims` the console would have, then calls
//! the SAME handler the console calls. That is the point — tenancy, validation and audit exist once. A
//! wrapper that re-implemented any of it would be a second rule that can disagree with the first.
//!
//! What is deliberately absent: no deletes, and nothing that marks a booking paid. Money arriving is an
//! authenticated fact proven by the payment provider's key plus a matching amount, and a delete an agent
//! can perform is a delete nobody can undo.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    routing::{patch, post},
    Extension, Json, Router,
};
use serde_json::Value;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};

/// Who is calling, and may they write?
///
/// A `read_only` issued key is refused here, in the same words the REST boundary uses, so the two doors
/// agree about what that role means.
pub struct Agent {
    pub claims: Claims,
    pub tenant: Uuid,
    pub label: String,
}

/// Resolve either credential to one account.
async fn authenticate(state: &crate::AppState, headers: &HeaderMap) -> Result<Agent, AppError> {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(AppError::Unauthorized)?;

    if bearer.starts_with("csk_") {
        // The SAME resolver the rest of the issued-key surface uses (`/api/external/**`): it decides
        // whether the key is live, whether its OWNER may still act (an issued key whose user was
        // deactivated or closed is not a credential), stamps `last_used_at` and meters the call
        // against the account's daily ceiling. A second hand-rolled key lookup here would be a
        // second rule that can disagree with the first.
        let (tenant, key_id) = crate::external_api::resolve_key(state, headers).await?;
        let role = crate::external_api::issued_key_role(state, bearer)
            .await
            .unwrap_or_default();
        if role != crate::personal_api_keys::ROLE_WORKSPACE {
            // The boundary's own sentence, so both doors say the same thing about the same role.
            return Err(AppError::ForbiddenReason(
                crate::auth::boundary::READ_ONLY_REFUSAL.to_string(),
            ));
        }
        let key_id = key_id.to_string();
        // ACT AS A REAL USER OF THE ACCOUNT (measured 2026-11-25). Several writers put `claims.sub` into
        // a column with a foreign key to `users(id)` — `opportunity_stage_history.moved_by` is one — so a
        // key's own id produced 500 "Database error" on the first deal an agent created. The key
        // authenticates the caller; the record is attributed to a real person of that account, which is
        // also what an audit reads months later.
        let actor: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE tenant_id = $1
              ORDER BY (role = 'account_owner') DESC, created_at LIMIT 1",
        )
        .bind(tenant)
        .fetch_optional(&state.db)
        .await?;
        let Some(actor) = actor else {
            return Err(AppError::Forbidden);
        };
        return Ok(Agent {
            claims: Claims {
                sub: actor.to_string(),
                aid: tenant.to_string(),
                // A key is not a person: it holds no admin role, so every handler's own role check
                // (an agency_admin door, for instance) refuses it exactly as it would a member.
                role: "api_key".to_string(),
                wsid: None,
                exp: 0,
                iat: 0,
                aud: None,
                iss: None,
            },
            tenant,
            label: format!("agent key {key_id} → acting as {actor}"),
        });
    }

    // Otherwise it is a console session, verified exactly as every other module does.
    let claims = crate::auth::middleware::verify_token(bearer, &state.config.jwt_secret)?;
    let tenant = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Agent {
        label: claims.sub.clone(),
        claims,
        tenant,
    })
}

pub fn router(_state: crate::AppState) -> Router<crate::AppState> {
    Router::new()
        .route("/contacts", post(contact_create))
        .route("/contacts/:id", patch(contact_update))
        .route("/deals", post(deal_create))
        .route("/deals/:id", patch(deal_update))
        .route("/deals/:id/stage", post(deal_move_stage))
        .route("/deals/:id/tasks", post(deal_add_task))
        .route("/lists/:id/members", post(list_add_contact))
        .route("/tags/assign", post(tag_assign))
        .route("/calendars/:id", patch(calendar_update))
        .route("/calendars/:id/payment-checkout", patch(calendar_checkout))
        .route("/resources/:id/payment-checkout", patch(resource_checkout))
        .route(
            "/calendars/:id/payment-key/rotate",
            post(rotate_payment_key),
        )
        // Where an owned address sends a visitor — the console route is session-only, so an agent needs
        // this door for the same rule to be reachable with a key.
        .route("/redirects", axum::routing::put(domain_redirect))
}

/// The booking module keeps its own error type. Map it into this module's at the boundary: the wrapper
/// must not leak another module's error shape, and the message a customer sees is the module's own.
fn booking_err(e: crate::booking::error::BookingError) -> AppError {
    use crate::booking::error::BookingError as B;
    match e {
        B::NotFound(_) => AppError::NotFound("that booking setting is not on this account".into()),
        B::Invalid(m) => AppError::BadRequest(m.to_string()),
        B::Conflict(m) => AppError::BadRequest(m.to_string()),
        other => AppError::BadRequest(other.to_string()),
    }
}

async fn contact_create(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::contacts::models::CreateContactRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("contact fields: {e}")))?;
    let r = crate::contacts::handlers::create(State(state), Extension(agent.claims), Json(typed))
        .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn contact_update(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::contacts::models::UpdateContactRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("contact fields: {e}")))?;
    let r = crate::contacts::handlers::update(
        State(state),
        Extension(agent.claims),
        Path(id),
        Json(typed),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn deal_create(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::deals::models::DealUpsert = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("deal fields: {e}")))?;
    let r =
        crate::deals::handlers::create(State(state), Extension(agent.claims), Json(typed)).await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn deal_update(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::deals::models::DealUpsert = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("deal fields: {e}")))?;
    let r = crate::deals::handlers::update(
        State(state),
        Extension(agent.claims),
        Path(id),
        Json(typed),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn deal_move_stage(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::deals::models::MoveDealRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("stage fields: {e}")))?;
    let r = crate::deals::handlers::move_deal(
        State(state),
        Extension(agent.claims),
        Path(id),
        Json(typed),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn deal_add_task(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::deals::models::NewTaskRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("task fields: {e}")))?;
    let r = crate::deals::handlers::add_task(
        State(state),
        Extension(agent.claims),
        Path(id),
        Json(typed),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn list_add_contact(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::lists::models::AddMemberRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("member fields: {e}")))?;
    let r = crate::lists::handlers::add_member(
        State(state),
        Extension(agent.claims),
        Path(id),
        Json(typed),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn tag_assign(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::tags::models::AssignTagRequest = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("tag fields: {e}")))?;
    let r = crate::tags::handlers::assign_tag(State(state), Extension(agent.claims), Json(typed))
        .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn calendar_update(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let typed: crate::booking::models::UpdateCalendar = serde_json::from_value(body.0)
        .map_err(|e| AppError::BadRequest(format!("calendar fields: {e}")))?;
    let r = crate::booking::handlers::update_calendar(
        State(state.db.clone()),
        Extension(crate::booking::models::TenantId(agent.tenant)),
        Path(id),
        Json(typed),
    )
    .await
    .map_err(booking_err)?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn calendar_checkout(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let checkout = body
        .get("checkout")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("checkout is required".into()))?;
    let typed = crate::booking::models::UpdateCalendar {
        name: None,
        slug: None,
        payment_mode: body
            .get("payment_mode")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        payment_embed: Some(checkout.to_string()),
        settings: None,
        reply_to_email: None,
        send_confirmation: None,
    };
    let r = crate::booking::handlers::update_calendar(
        State(state.db.clone()),
        Extension(crate::booking::models::TenantId(agent.tenant)),
        Path(id),
        Json(typed),
    )
    .await
    .map_err(booking_err)?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn resource_checkout(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let checkout = body
        .get("checkout")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("checkout is required".into()))?;
    // Read the current row first: the console's update writes the WHOLE position, so a checkout change
    // must carry the price, size and timing it already had rather than blanking them.
    let row: Option<(
        String,
        Option<String>,
        i64,
        Option<i64>,
        i32,
        i32,
        i32,
        i32,
        i32,
        i32,
        Option<String>,
        Option<String>,
        bool,
    )> = sqlx::query_as(
        "SELECT r.name, r.description, r.price_cents, r.compare_at_price_cents, r.duration_min,
                    r.slot_interval_min, r.buffer_min, r.min_notice_min, r.sort_order, r.capacity,
                    r.size, r.payment_mode, r.exclusive
               FROM booking_resources r JOIN booking_calendars c ON c.id = r.calendar_id
              WHERE r.id = $1 AND c.tenant_id = $2",
    )
    .bind(id)
    .bind(agent.tenant)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(|| AppError::NotFound("no such position on this account".into()))?;
    let typed = crate::booking::models::NewResource {
        name: row.0,
        description: row.1,
        capacity: Some(row.9),
        price_cents: Some(row.2),
        compare_at_price_cents: row.3,
        duration_min: Some(row.4),
        exclusive: Some(row.12),
        size: row.10,
        checkout_embed: Some(checkout.to_string()),
        payment_mode: body
            .get("payment_mode")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .or(row.11),
        slot_interval_min: Some(row.5),
        buffer_min: Some(row.6),
        min_notice_min: Some(row.7),
        sort_order: Some(row.8),
        metadata: None,
    };
    let r = crate::booking::handlers::update_resource(
        State(state.db.clone()),
        Extension(crate::booking::models::TenantId(agent.tenant)),
        Path(id),
        Json(typed),
    )
    .await
    .map_err(booking_err)?;
    Ok(axum::response::IntoResponse::into_response(r))
}

async fn rotate_payment_key(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let r = crate::booking::handlers::rotate_payment_key(
        State(state.db.clone()),
        Extension(crate::booking::models::TenantId(agent.tenant)),
        Path(id),
    )
    .await
    .map_err(booking_err)?;
    Ok(axum::response::IntoResponse::into_response(r))
}

/// Set where one of this account's own addresses sends a visitor. The rules live in
/// `domain_redirects::save_redirect` — this wrapper only supplies the identity it resolved.
async fn domain_redirect(
    State(state): State<crate::AppState>,
    headers: HeaderMap,
    body: Json<Value>,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let r = crate::domain_redirects::handlers::save_redirect(
        State(state),
        Extension(agent.claims),
        Json(body.0),
    )
    .await?;
    Ok(axum::response::IntoResponse::into_response(r))
}
