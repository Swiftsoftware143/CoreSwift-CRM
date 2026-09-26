//! Lightweight tickets module — in-house customer support ticketing.
//!
//! Routes:
//! - GET    /api/tickets          — list tickets (filters: status, priority, contact_id)
//! - GET    /api/tickets/stats    — quick counts by status
//! - POST   /api/tickets          — create ticket
//! - GET    /api/tickets/:id      — get ticket + messages
//! - PATCH  /api/tickets/:id      — update status, priority, assignment
//! - DELETE /api/tickets/:id      — delete ticket
//! - POST   /api/tickets/:id/messages — add message to ticket

pub mod handlers;
pub mod models;
pub mod portal;

use crate::AppState;
use axum::{middleware, Router};

pub fn public_router(_state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/public/contact",
            axum::routing::post(handlers::public_contact_form),
        )
        .route(
            "/s/:tenant_id/ticket",
            axum::routing::post(handlers::public_submit_ticket),
        )
        .route(
            "/s/:tenant_id/widget.js",
            axum::routing::get(handlers::support_embed_script),
        )
}

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/tickets",
            axum::routing::get(handlers::list_tickets).post(handlers::create_ticket),
        )
        .route("/tickets/stats", axum::routing::get(handlers::ticket_stats))
        .route(
            "/tickets/:id",
            axum::routing::get(handlers::get_ticket)
                .patch(handlers::update_ticket)
                .delete(handlers::delete_ticket),
        )
        .route(
            "/tickets/:id/messages",
            axum::routing::post(handlers::add_message),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "tickets", "Support tickets"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
