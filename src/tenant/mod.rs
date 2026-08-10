//! Tenant module — user-facing endpoints for the current user's own tenant.
//!
//! GET  /api/tenants     — Get current user's tenant information
//! PUT  /api/tenants     — Update current user's tenant (name, settings, etc.)

pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the tenant router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::get_current_tenant))
        .route("/", axum::routing::put(handlers::update_current_tenant))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
