//! Plan / upgrade requests — one queue, one customer door (kanban t_42872c18).
//!
//! CoreSwift had no plan-request QUEUE. A plan ask was a `tickets` row whose ONLY marker was the
//! subject sentence ("Plan request at signup: {plan} plan"), written with `source = 'form'` — the
//! same value the public contact form writes — so the console could find one only by matching
//! prose (`/plan request/i`), no row recorded WHICH plan was asked for, and the operator's ordinary
//! ticket list could never show a request filed by a CUSTOMER's workspace (`list_tickets` scopes to
//! the caller's own tenant plus tenantless rows). A signed-in customer also had no door at all: the
//! only plan-request intake was the signup page.
//!
//! The fleet contract is `/opt/swift/docs/plan-request-queue-contract-2026-11-27.md`. Three states
//! everywhere — **open → contacted → closed** — with each app translating to its own stored
//! vocabulary. This app already stores ticket statuses `new/open/pending/on_hold/solved/closed`, so:
//!
//! | operator says | this app stores |
//! |---|---|
//! | `open` | `open` |
//! | `contacted` | `pending` |
//! | `closed` | `closed` |
//!
//! and on read `pending`/`contacted` → `contacted`, `solved`/`closed` → `closed`, anything else
//! unrecognised → `open` (so a row never hides from the queue). The raw stored word is returned
//! beside the canonical one.
//!
//! # Routes
//!
//! - `GET   /api/v1/admin/plan-requests`     — the cross-tenant operator queue
//! - `PATCH /api/v1/admin/plan-requests/:id` — one of the three canonical states
//! - `POST  /api/v1/plan-requests`           — the SIGNED-IN customer door (own workspace, slug
//!                                             validated against the plan catalogue before writing)
//!
//! # Who may read the queue
//!
//! `users.is_platform_admin = true`, OR a `role = 'admin'` seat inside the workspace that holds a
//! platform admin (i.e. the VA seat next to the operator). The CUSTOMER workspace roles —
//! `owner`, `account_owner`, `agency_admin` in their own tenant — grant NOTHING here: this list is
//! cross-tenant, so opening it to a tenant admin would expose every other workspace's requests.
//! The role is read from the DATABASE row, never from the token claim, exactly like
//! `auth::platform_admin`.

use axum::{
    extract::{Path, State},
    http::Request,
    middleware::Next,
    response::Response,
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The three states an operator may write, and the word this app stores for each.
pub fn stored_status(canonical: &str) -> Option<&'static str> {
    match canonical {
        "open" => Some("open"),
        "contacted" => Some("pending"),
        "closed" => Some("closed"),
        _ => None,
    }
}

/// The canonical state an operator reads for a stored ticket status. Total: anything not recognised
/// reads `open`, so a legacy row can never disappear from the queue.
pub fn queue_status(stored: &str) -> &'static str {
    match stored {
        "open" => "open",
        "pending" | "contacted" => "contacted",
        "solved" | "closed" => "closed",
        _ => "open",
    }
}

/// One row of the operator queue.
#[derive(Debug, Serialize, FromRow)]
pub struct PlanRequestRow {
    pub id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub tenant_name: Option<String>,
    pub requested_by: Option<String>,
    pub plan_slug: Option<String>,
    pub plan_name: Option<String>,
    pub note: String,
    /// The stored ticket status, verbatim — never rewritten.
    pub status: String,
    /// Filled from `queue_status` after the read; `#[sqlx(skip)]` keeps the query shape untouched.
    #[sqlx(skip)]
    pub queue_status: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// The whole queue. A literal statement, never assembled at run time (the release gate refuses
/// built SQL, and a reader must be able to see every byte sent to Postgres).
const LIST_SQL: &str = r#"
    SELECT t.id,
           t.tenant_id,
           tn.name AS tenant_name,
           COALESCE(NULLIF(t.contact_name, ''), NULLIF(t.contact_email, '')) AS requested_by,
           t.plan_slug,
           p.name AS plan_name,
           t.description AS note,
           t.status,
           t.created_at,
           t.updated_at
      FROM tickets t
      LEFT JOIN tenants tn ON tn.id = t.tenant_id
      LEFT JOIN plans p ON p.slug = t.plan_slug
     WHERE t.source = 'plan_request'
     ORDER BY t.created_at DESC
     LIMIT 500
"#;

/// One row of the queue, by id — the write answer and the read answer share one shape.
const ONE_SQL: &str = r#"
    SELECT t.id,
           t.tenant_id,
           tn.name AS tenant_name,
           COALESCE(NULLIF(t.contact_name, ''), NULLIF(t.contact_email, '')) AS requested_by,
           t.plan_slug,
           p.name AS plan_name,
           t.description AS note,
           t.status,
           t.created_at,
           t.updated_at
      FROM tickets t
      LEFT JOIN tenants tn ON tn.id = t.tenant_id
      LEFT JOIN plans p ON p.slug = t.plan_slug
     WHERE t.source = 'plan_request' AND t.id = $1
"#;

/// The operator gate: platform admin, or an `admin` seat in the platform workspace.
///
/// Mounted on the admin router, AFTER `auth_middleware` (which is what injects `Claims`).
pub async fn operator_gate(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, AppError> {
    let Some(claims) = req.extensions().get::<Claims>().cloned() else {
        tracing::warn!(path = %req.uri().path(), "plan-request gate reached without claims");
        return Err(AppError::Unauthorized);
    };
    if !crate::auth::platform_admin::is_platform_staff(&state.db, &claims.sub).await? {
        tracing::warn!(
            subject = %claims.sub,
            path = %req.uri().path(),
            "refused the cross-tenant plan-request queue for a non-operator"
        );
        return Err(AppError::Forbidden);
    }
    Ok(next.run(req).await)
}

/// `GET /api/v1/admin/plan-requests` — every workspace's plan asks, newest first.
pub async fn list_plan_requests(
    State(s): State<AppState>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let mut rows = sqlx::query_as::<_, PlanRequestRow>(LIST_SQL)
        .fetch_all(&s.db)
        .await?;
    for r in rows.iter_mut() {
        r.queue_status = queue_status(&r.status).to_string();
    }
    Ok(Json(json!({ "count": rows.len(), "items": rows })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateStatusBody {
    pub status: String,
}

/// `PATCH /api/v1/admin/plan-requests/:id` — write one of the three canonical states.
pub async fn update_plan_request(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStatusBody>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let canonical = body.status.trim().to_lowercase();
    let Some(stored) = stored_status(&canonical) else {
        return Err(AppError::Validation(format!(
            "\"{}\" is not a plan-request state; use one of: open, contacted, closed",
            body.status.trim()
        )));
    };

    // Same `solved_at` rule the ticket writer uses: closed stamps the solve time, reopening clears
    // it, so a row that comes back into the queue cannot carry a stale resolution time.
    let updated: Option<Uuid> = sqlx::query_scalar(
        r#"UPDATE tickets
              SET status = $1,
                  updated_at = NOW(),
                  solved_at = CASE
                      WHEN $1 = 'closed' THEN COALESCE(solved_at, NOW())
                      ELSE NULL
                  END
            WHERE id = $2 AND source = 'plan_request'
          RETURNING id"#,
    )
    .bind(stored)
    .bind(id)
    .fetch_optional(&s.db)
    .await?;
    let Some(row_id) = updated else {
        return Err(AppError::NotFound("Plan request not found".into()));
    };

    // The UPDATE cannot join for the display fields, so re-read through the SAME statement the list
    // uses — one row shape for the write answer and the read answer.
    let mut row = sqlx::query_as::<_, PlanRequestRow>(ONE_SQL)
        .bind(row_id)
        .fetch_one(&s.db)
        .await?;
    row.queue_status = queue_status(&row.status).to_string();
    Ok(Json(json!(row)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatePlanRequestBody {
    pub plan_slug: String,
    #[serde(default)]
    pub note: Option<String>,
}

/// `POST /api/v1/plan-requests` — a signed-in customer asks for a plan.
///
/// The slug is validated against the catalogue BEFORE anything is written (a typo files nothing),
/// the row carries the CALLER'S workspace, and the operator is told only after the row has
/// committed — so a mail failure can never lose a request that was really filed.
pub async fn create_plan_request(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CreatePlanRequestBody>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let slug = body.plan_slug.trim().to_lowercase();
    if slug.is_empty() || slug == "free" {
        return Err(AppError::Validation(
            "Name the plan you would like by its slug (for example \"professional\").".into(),
        ));
    }

    // A plan may be REQUESTED even when it is not on sale yet (the console shows the $1 Inbox door
    // with a request path on purpose); what a request must name is a REAL plan with a price. `free`
    // is not a purchase and is refused above.
    let plan: Option<(String, String)> = sqlx::query_as(
        "SELECT slug, name FROM plans \
          WHERE slug = $1 AND COALESCE(price_monthly, 0) > 0",
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?;
    let Some((plan_slug, plan_name)) = plan else {
        return Err(AppError::Validation(
            "That is not a plan this installation sells. Ask for one from the plan list in your console.".into(),
        ));
    };

    let (who, email): (String, String) = sqlx::query_as(
        "SELECT COALESCE(NULLIF(name, ''), email), email FROM users WHERE id = $1 AND is_active",
    )
    .bind(Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let note = body
        .note
        .as_deref()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if let Some(n) = &note {
        if n.chars().count() > 2000 {
            return Err(AppError::Validation(
                "The note is too long — keep it under 2000 characters.".into(),
            ));
        }
    }

    let subject = format!("Plan request: {plan_name} plan");
    let mut message = format!(
        "A signed-in workspace asked for the {plan_name} plan ({plan_slug}) from Plans & Billing. \
         Nothing was charged: self-serve checkout is not open on this installation, so the switch \
         has to be made by the CoreSwift team.\n\nPlan: {plan_name} ({plan_slug})\n\
         Workspace: {tenant_id}\nRequested by: {who} <{email}>"
    );
    if let Some(n) = &note {
        message.push_str(&format!("\n\nNote from the customer: {n}"));
    }

    // Idempotent while a request for the same plan is still open: a double press is one ask.
    let inserted: Option<Uuid> = sqlx::query_scalar(
        r#"INSERT INTO tickets
               (tenant_id, subject, description, status, priority, source, plan_slug,
                contact_email, contact_name)
           SELECT $1::uuid, $2, $3, 'open', 'medium', 'plan_request', $4, $5, $6
            WHERE NOT EXISTS (
                SELECT 1 FROM tickets
                 WHERE tenant_id = $1::uuid AND source = 'plan_request' AND plan_slug = $4
                   AND status IN ('open', 'pending'))
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(&subject)
    .bind(&message)
    .bind(&plan_slug)
    .bind(&email)
    .bind(&who)
    .fetch_optional(&s.db)
    .await?;

    let Some(ticket_id) = inserted else {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM tickets \
              WHERE tenant_id = $1 AND source = 'plan_request' AND plan_slug = $2 \
                AND status IN ('open', 'pending') \
              ORDER BY created_at DESC LIMIT 1",
        )
        .bind(tenant_id)
        .bind(&plan_slug)
        .fetch_optional(&s.db)
        .await?;
        tracing::info!(plan = %plan_slug, tenant = %tenant_id,
            "plan request already open for this workspace — not filed twice");
        return Ok((
            axum::http::StatusCode::OK,
            Json(json!({
                "status": "existing",
                "id": existing,
                "plan_slug": plan_slug,
                "plan_name": plan_name,
                "queue_status": "open"
            })),
        ));
    };

    // Tell the operator AFTER the row committed. Best effort: the row is the record.
    let (notified, notified_reason) = match crate::email::send_support_request_operator_alert(
        &s.db, &who, &email, &subject, &message,
    )
    .await
    {
        Ok(crate::email::SupportAlert::Sent) => (true, None),
        Ok(crate::email::SupportAlert::Withheld(reason)) => {
            tracing::info!(ticket = %ticket_id, reason, "plan-request operator alert withheld");
            (false, Some(reason.to_string()))
        }
        Err(e) => {
            tracing::warn!(ticket = %ticket_id,
                    "plan-request operator alert was not sent: {e}");
            (
                false,
                Some("no notification email could be sent".to_string()),
            )
        }
    };

    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "status": "filed",
            "id": ticket_id,
            "plan_slug": plan_slug,
            "plan_name": plan_name,
            "queue_status": "open",
            "notified": notified,
            "notified_reason": notified_reason
        })),
    ))
}

/// The operator queue — mounted at `/api/v1/admin/plan-requests`.
pub fn admin_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(list_plan_requests))
        .route("/:id", axum::routing::patch(update_plan_request))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            operator_gate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::middleware::auth_middleware,
        ))
}

/// The signed-in customer door — mounted at `/api/v1/plan-requests`.
pub fn customer_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::post(create_plan_request))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state,
            crate::auth::middleware::auth_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use super::{queue_status, stored_status};

    /// The mapping is total in both directions, and an unknown stored word never hides a row.
    #[test]
    fn the_three_states_map_both_ways() {
        assert_eq!(stored_status("open"), Some("open"));
        assert_eq!(stored_status("contacted"), Some("pending"));
        assert_eq!(stored_status("closed"), Some("closed"));
        assert_eq!(stored_status("Contacted "), None, "callers normalise first");
        assert_eq!(stored_status("solved"), None);

        assert_eq!(queue_status("open"), "open");
        assert_eq!(queue_status("pending"), "contacted");
        assert_eq!(queue_status("contacted"), "contacted");
        assert_eq!(queue_status("closed"), "closed");
        assert_eq!(queue_status("solved"), "closed");
        assert_eq!(queue_status("on_hold"), "open");
        assert_eq!(queue_status("anything-else"), "open");
    }

    /// Round trip: what the operator presses is what the queue reports back.
    #[test]
    fn a_pressed_state_reads_back_as_itself() {
        for canonical in ["open", "contacted", "closed"] {
            let stored = stored_status(canonical).expect("canonical state");
            assert_eq!(queue_status(stored), canonical);
        }
    }
}
