pub mod actions;
pub mod engine;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/rules", axum::routing::get(handlers::list_rules))
        .route("/rules", axum::routing::post(handlers::create_rule))
        .route("/rules/:id", axum::routing::get(handlers::get_rule))
        .route("/rules/:id", axum::routing::patch(handlers::update_rule))
        .route("/rules/:id", axum::routing::delete(handlers::delete_rule))
        // Plan gating — the admin controls this module per plan
        // (features::FEATURE_REGISTRY is the source of truth for the admin UI).
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "automation", "Automations"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
