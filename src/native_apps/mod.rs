//! Native App Connectors — CRM Swift's first-party integration layer
//!
//! Each app gets a connector that handles:
//! - OAuth / API key handshake
//! - Push contacts, lists, tags into CRM Swift
//! - Pull contacts, lists, tags from CRM Swift
//! - Trigger automations from app events
//!
//! Access model:
//! - Admin-only: AdaSwift (client viewing portal), CheatLayer
//! - Admin + Tenant: FunnelSwift, Palm Bay Pulse, ZaarHub, WorkflowSwift
//!
//! Ada campaign triggers (a CRM automation rule mapped to an AdaSwift campaign) were RETIRED
//! 2026-10-02 (kanban t_434b240b): nothing read the mapping, so it could never fire.

pub mod connectors;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // Admin manages available apps (FunnelSwift, AdaSwift, etc.)
        .route("/apps", axum::routing::get(handlers::list_available_apps))
        // Admin + Tenant: connect/disconnect their app instances
        .route(
            "/apps/:app_slug/connect",
            axum::routing::post(handlers::connect_app),
        )
        .route(
            "/apps/:app_slug/disconnect",
            axum::routing::post(handlers::disconnect_app),
        )
        .route(
            "/apps/:app_slug/status",
            axum::routing::get(handlers::app_status),
        )
        .route(
            "/apps/:app_slug/test",
            axum::routing::post(handlers::test_connection),
        )
        // Admin + Tenant: sync operations
        .route(
            "/apps/:app_slug/sync/pull",
            axum::routing::post(handlers::pull_from_app),
        )
        .route(
            "/apps/:app_slug/sync/push",
            axum::routing::post(handlers::push_to_app),
        )
        .route(
            "/apps/:app_slug/sync/history",
            axum::routing::get(handlers::sync_history),
        )
        // Admin only: global app config (e.g. AdaSwift base URL)
        .route(
            "/apps/admin/:app_slug",
            axum::routing::get(handlers::get_admin_config),
        )
        .route(
            "/apps/admin/:app_slug",
            axum::routing::patch(handlers::update_admin_config),
        )
        .route(
            "/apps/admin/configs",
            axum::routing::get(handlers::list_admin_configs),
        )
        // Ada campaign triggers (`/apps/ada-campaigns`) were RETIRED 2026-10-02 (kanban t_434b240b):
        // nothing ever read a trigger's `trigger_on`, so it could not fire a campaign.
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "native_apps",
                "Native app connectors",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
