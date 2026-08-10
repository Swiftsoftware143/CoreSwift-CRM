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
//! Instead of sending through Mailgun/SMTP.com/Telnyx, automation rules
//! now trigger an Ada campaign for welcome emails + scan reports on
//! new client/account creation.

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
        // Map a CRM Swift automation rule to trigger an Ada campaign
        .route(
            "/apps/ada-campaigns",
            axum::routing::post(handlers::create_ada_campaign_trigger),
        )
        .route(
            "/apps/ada-campaigns",
            axum::routing::get(handlers::list_ada_campaign_triggers),
        )
        .route(
            "/apps/ada-campaigns/:id",
            axum::routing::delete(handlers::delete_ada_campaign_trigger),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
