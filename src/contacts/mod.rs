//! Contacts management module.
//!
//! Full CRUD with tenant-scoped queries, search, and pagination.

pub mod models;
pub mod handlers;
pub mod internal_handler;

use axum::{Router, middleware};
use crate::AppState;

/// Build the contacts router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/search", axum::routing::get(handlers::search))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
