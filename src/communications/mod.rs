//! Communications module — multi-channel messaging orchestration.
//!
//! Handles queued outbound messages across Email (Mailgun / SMTP.com) and SMS (Telnyx).
//! The dispatcher writes to outbound_messages; this module picks up and sends.

pub mod handlers;
pub mod providers;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/messages", axum::routing::get(handlers::list_messages))
        .route("/messages", axum::routing::post(handlers::send))
        .route("/messages/:id", axum::routing::get(handlers::get_message))
        .route("/templates", axum::routing::get(handlers::list_templates))
        .route("/templates", axum::routing::post(handlers::create_template))
        .route(
            "/templates/:id",
            axum::routing::patch(handlers::update_template),
        )
        .route(
            "/templates/:id",
            axum::routing::delete(handlers::delete_template),
        )
        .route("/providers", axum::routing::get(handlers::get_providers))
        .route(
            "/providers",
            axum::routing::patch(handlers::update_providers),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "telnyx", "SMS & voice (Telnyx)"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
