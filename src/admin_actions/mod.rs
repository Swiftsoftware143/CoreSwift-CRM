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
//! GET  /api/admin/users — list users across all tenants (admin)
//! POST /api/admin/portfolio-sync — cross-app sync
//! GET  /api/admin/email-config — platform mail transport, masked (admin)
//! PUT  /api/admin/email-config — save the platform provider + credential (admin)
//! DELETE /api/admin/email-config — drop the stored override, use the environment (admin)
//! POST /api/admin/email-config/test — send a real message through the platform transport (admin)
//! PUT  /api/admin/tenants/:id/plan — put a TARGET tenant on a plan (admin; kanban t_f1ffb865)
//! GET  /api/admin/provisioning-config — the tag → free account knobs (admin; kanban t_e968e9ad)
//! PUT  /api/admin/provisioning-config — save them (admin; kanban t_e968e9ad)

pub mod email_config;
pub mod handlers;
pub mod provisioning_config;
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
        // Bulk retire (kanban t_ac2fe688) — the panel's "Delete selected" control. Static segment,
        // and there is no bare `/tenants/:id` route on this router (only `:id/overrides`,
        // `:id/entitlements`, `:id/plan`), so it can never be read as a tenant id.
        .route(
            "/tenants/bulk-delete",
            axum::routing::post(handlers::bulk_delete_tenants),
        )
        // People across every tenant — the counterpart of /tenants. The admin shell's Users tab
        // called a bare /api/users that nothing registered; this is the route it now calls.
        .route("/users", axum::routing::get(handlers::list_all_users))
        .route(
            "/portfolio-sync",
            axum::routing::post(handlers::cross_app_sync),
        )
        .route(
            "/site",
            axum::routing::get(site_handler::get_site).put(site_handler::update_site),
        )
        // The PLATFORM mail transport (provider + credential) — the half that used to live only in
        // the container environment, so it could be neither seen nor rotated from the panel
        // (kanban t_6a330ed2). GET is masked; DELETE drops the override and returns to EMAIL_*.
        .route(
            "/email-config",
            axum::routing::get(email_config::get_config)
                .put(email_config::update_config)
                .delete(email_config::delete_config),
        )
        .route(
            "/email-config/test",
            axum::routing::post(email_config::test_config),
        )
        // The operator's half of the tag → free account contract: the master switch (ships ON)
        // and the entry-plan picker, read by `POST /api/v1/internal/provision-free-account`
        // (kanban t_e968e9ad). On this router, so it is covered by the platform-admin gate below —
        // a tenant `owner` is refused.
        .route(
            "/provisioning-config",
            axum::routing::get(provisioning_config::get_config)
                .put(provisioning_config::update_config),
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
        // The operator's PLAN-ASSIGNMENT instrument (kanban t_f1ffb865, the WS-15 analogue).
        // Without it no surface could put ANOTHER tenant on a plan: both writers of
        // /api/billing/subscription take the tenant from `Claims.aid`, so the platform could only
        // ever write its own workspace and every other tenant stayed on its signup plan.
        // The tenant is the PATH parameter here — that is the difference that makes this route
        // able to act on a target account at all.
        .route(
            "/tenants/:id/plan",
            axum::routing::put(crate::billing::handlers::assign_tenant_plan),
        )
        // TWO layers. `Router::layer` wraps outermost-last, so listing the platform-admin gate
        // FIRST makes `auth_middleware` run first and inject `Claims`; the gate then reads them.
        // The gate sits on the ROUTER, not in each handler, so a route added to this router cannot
        // ship ungated. Before it existed, 6 of the 8 protected admin routes answered an ordinary
        // tenant user (`role='owner'`, 38 of 47 production users): /site, /portfolio-sync,
        // /chat-action, /chat-action/intents, /stop-impersonation and the private-email admin
        // surfaces (kanban t_d5cf6cad).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
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
