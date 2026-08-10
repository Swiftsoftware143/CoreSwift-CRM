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
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
