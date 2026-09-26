pub mod account_health_handler;
pub mod engine;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/health", axum::routing::get(handlers::get_health))
        .route(
            "/health",
            axum::routing::post(handlers::update_health_signal),
        )
        .route("/thresholds", axum::routing::get(handlers::list_thresholds))
        .route(
            "/thresholds",
            axum::routing::post(handlers::create_threshold),
        )
        .route(
            "/thresholds/:id",
            axum::routing::patch(handlers::update_threshold),
        )
        .route(
            "/thresholds/:id",
            axum::routing::delete(handlers::delete_threshold),
        )
        // Account health trial monitor & churn prevention (Phase 4)
        .route(
            "/account-health/check",
            axum::routing::post(account_health_handler::run_health_check),
        )
        .route(
            "/account-health/milestone",
            axum::routing::post(account_health_handler::record_milestone),
        )
        .route(
            "/account-health/status/:profile_id",
            axum::routing::get(account_health_handler::profile_health_status),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "monitoring",
                "Monitoring & health",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
