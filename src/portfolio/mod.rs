/// Portfolio module — portfolio company management for multi-company tenants
pub mod sync;

pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/internal", axum::routing::post(handlers::internal_create))
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::put(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .route("/:id/targets", axum::routing::get(handlers::list_targets))
        .route("/:id/targets", axum::routing::post(handlers::create_target))
        // Plan gating — the admin controls this module per plan
        // (features::FEATURE_REGISTRY is the source of truth for the admin UI).
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "portfolio", "Portfolio sync"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
