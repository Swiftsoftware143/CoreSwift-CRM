//! Industry Dashboards module — manage user industry dashboard selections.
//!
//! Industries map to template_categories from workflowswift and define
//! the default dashboard/automation templates a user sees. Each user can
//! activate multiple industry dashboards up to their plan's max_industries limit.
//!
//! Routes:
//! GET    /api/industries/available — list all available industries
//! GET    /api/industries          — list user's active industry dashboards
//! POST   /api/industries          — set/activate an industry dashboard
//! DELETE /api/industries/:slug    — deactivate an industry dashboard
//! GET    /api/industries/limit    — get plan industry limit & usage

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the industries router with auth middleware where needed.
pub fn router(state: AppState) -> Router<AppState> {
    // Public route — no auth needed for available list
    let public = Router::new()
        .route("/available", axum::routing::get(handlers::list_available));

    // Protected routes — require auth
    let protected = Router::new()
        .route("/", axum::routing::get(handlers::list_user_industries))
        .route("/", axum::routing::post(handlers::set_user_industry))
        .route("/limit", axum::routing::get(handlers::get_industry_limit))
        .route("/:slug", axum::routing::delete(handlers::remove_user_industry))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new().merge(public).merge(protected)
}
