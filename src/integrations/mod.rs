pub mod handlers;
pub mod models;
pub mod n8n;
pub mod webhook;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .route(
            "/:integration_id/mappings",
            axum::routing::get(handlers::list_mappings),
        )
        .route(
            "/:integration_id/mappings",
            axum::routing::post(handlers::create_mapping),
        )
        .route(
            "/mappings/:id",
            axum::routing::delete(handlers::delete_mapping),
        )
        .route("/webhooks", axum::routing::get(handlers::list_webhooks))
        .route("/webhooks", axum::routing::post(handlers::create_webhook))
        .route(
            "/webhooks/:id",
            axum::routing::patch(handlers::update_webhook),
        )
        .route(
            "/webhooks/:id",
            axum::routing::delete(handlers::delete_webhook),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "integrations", "Integrations"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
