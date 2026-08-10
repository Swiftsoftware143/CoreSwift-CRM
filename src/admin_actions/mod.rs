//! Admin module — chat actions + legacy admin API routes
//!
//! POST /api/admin/chat-action — run business actions from chat
//! POST /api/admin/impersonate — admin JWT tenant switch
//! GET  /api/admin/health — health check
//! GET  /api/admin/portfolio-companies — list all portfolio companies (admin)
//! GET  /api/admin/tenants — list all tenants (admin)
//! POST /api/admin/portfolio-sync — cross-app sync

pub mod handlers;

use crate::AppState;
use axum::{middleware, Router};

/// Admin API routes with auth middleware (except health check)
pub fn router(state: AppState) -> Router<AppState> {
    // Public admin routes (no auth needed)
    let public = Router::new().route("/health", axum::routing::get(handlers::health_check));

    // Protected admin routes
    let protected = Router::new()
        .route(
            "/chat-action",
            axum::routing::post(handlers::execute_chat_action),
        )
        .route(
            "/chat-action/intents",
            axum::routing::get(handlers::list_intents),
        )
        .route("/impersonate", axum::routing::post(handlers::impersonate))
        .route(
            "/stop-impersonation",
            axum::routing::post(handlers::stop_impersonation),
        )
        .route(
            "/portfolio-companies",
            axum::routing::get(handlers::list_all_portfolio_companies),
        )
        .route("/tenants", axum::routing::get(handlers::list_all_tenants))
        .route(
            "/portfolio-sync",
            axum::routing::post(handlers::cross_app_sync),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new().merge(public).merge(protected)
}
