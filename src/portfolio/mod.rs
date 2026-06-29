//! Portfolio module — portfolio company management for multi-company tenants

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

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
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
