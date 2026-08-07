//! Activities module — unified activity/event log for users.
//!
//! Provides a simple paginated endpoint to retrieve recent activity for the
//! current tenant, drawn from events, audit_logs, and opportunity_stage_history.

mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the activities router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list_activities))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
