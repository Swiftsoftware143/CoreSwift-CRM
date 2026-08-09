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

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/tickets", axum::routing::get(handlers::list_tickets).post(handlers::create_ticket))
        .route("/tickets/stats", axum::routing::get(handlers::ticket_stats))
        .route("/tickets/:id", axum::routing::get(handlers::get_ticket).patch(handlers::update_ticket).delete(handlers::delete_ticket))
        .route("/tickets/:id/messages", axum::routing::post(handlers::add_message))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
