//! Unified Inbox messages module.

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the messages router with auth middleware for protected routes only.
/// The webhook/webhook endpoint is registered separately in main.rs (no auth).
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::put(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
