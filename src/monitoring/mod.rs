pub mod models;
pub mod handlers;
pub mod engine;
pub mod account_health_handler;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/health", axum::routing::get(handlers::get_health))
        .route("/health", axum::routing::post(handlers::update_health_signal))
        .route("/thresholds", axum::routing::get(handlers::list_thresholds))
        .route("/thresholds", axum::routing::post(handlers::create_threshold))
        .route("/thresholds/:id", axum::routing::patch(handlers::update_threshold))
        .route("/thresholds/:id", axum::routing::delete(handlers::delete_threshold))
        // Account health trial monitor & churn prevention (Phase 4)
        .route("/account-health/check", axum::routing::post(account_health_handler::run_health_check))
        .route("/account-health/milestone", axum::routing::post(account_health_handler::record_milestone))
        .route("/account-health/status/:profile_id", axum::routing::get(account_health_handler::profile_health_status))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
