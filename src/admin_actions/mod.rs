//! Admin module — chat actions + legacy admin API routes
//!
//! PLATFORM-ONLY: every protected route here is gated on platform-admin authority
//! (`auth::platform_admin`), resolved from the database by token subject. Tenant users hold
//! `role='owner'`, which grants nothing platform-wide.
//!
//! POST /api/admin/chat-action — run business actions from chat
//! POST /api/admin/impersonate — admin JWT tenant switch
//! GET  /api/admin/health — health check
//! GET  /api/admin/portfolio-companies — list all portfolio companies (admin)
//! GET  /api/admin/tenants — list all tenants (admin)
//! POST /api/admin/portfolio-sync — cross-app sync

pub mod handlers;
pub mod site_handler;

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
        .route(
            "/site",
            axum::routing::get(site_handler::get_site).put(site_handler::update_site),
        )
        // Data-driven module & feature registry (CS-25..CS-27). The admin assigns MODULES and
        // individual FEATURES of each module to plans here; the catalogue itself lives in the
        // database, so a new module registers a row instead of a Rust const.
        .route(
            "/modules",
            axum::routing::get(crate::module_registry::handlers::list_modules),
        )
        .route(
            "/plans/:slug/modules",
            axum::routing::post(crate::module_registry::handlers::assign_module),
        )
        .route(
            "/plans/:slug/features",
            axum::routing::post(crate::module_registry::handlers::assign_feature),
        )
        .route(
            "/tenants/:id/overrides",
            axum::routing::post(crate::module_registry::handlers::set_override),
        )
        .route(
            "/tenants/:id/entitlements",
            axum::routing::get(crate::module_registry::handlers::tenant_entitlements),
        )
        // TWO layers. `Router::layer` wraps outermost-last, so listing the platform-admin gate
        // FIRST makes `auth_middleware` run first and inject `Claims`; the gate then reads them.
        // The gate sits on the ROUTER, not in each handler, so a route added to this router cannot
        // ship ungated. Before it existed, 6 of the 8 protected admin routes answered an ordinary
        // tenant user (`role='owner'`, 38 of 47 production users): /site, /portfolio-sync,
        // /chat-action, /chat-action/intents, /stop-impersonation and the private-email admin
        // surfaces (kanban t_d5cf6cad).
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::platform_admin::require_platform_admin_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new().merge(public).merge(protected)
}
