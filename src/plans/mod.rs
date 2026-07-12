//! Plans module — Super Admin plan definitions and feature limits.
//!
//! Provides CRUD for plan tiers. All endpoints require `agency_admin` role.
//! Plans define pricing, contact/deal/user limits, and feature toggles.

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the plans router with auth middleware.
/// All routes require agency_admin role (enforced in handlers).
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
