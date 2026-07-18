pub mod models;
pub mod handlers;
pub mod engine;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/rules", axum::routing::get(handlers::list_rules))
        .route("/rules", axum::routing::post(handlers::create_rule))
        .route("/rules/:id", axum::routing::patch(handlers::update_rule))
        .route("/rules/:id", axum::routing::delete(handlers::delete_rule))
        .route("/scores/:contact_id", axum::routing::get(handlers::get_score))
        .route("/calculate/:contact_id", axum::routing::post(handlers::calculate_score))
        .route("/history/:contact_id", axum::routing::get(handlers::get_score_history))
        .route("/distribution", axum::routing::get(handlers::score_distribution))
        .route("/thresholds", axum::routing::get(handlers::list_thresholds))
        .route("/thresholds", axum::routing::post(handlers::create_threshold))
        .route("/thresholds/:id", axum::routing::delete(handlers::delete_threshold))
        .route("/webhooks", axum::routing::get(handlers::list_webhooks))
        .route("/webhooks", axum::routing::post(handlers::create_webhook))
        .route("/webhooks/:id", axum::routing::delete(handlers::delete_webhook))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
