pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // In-app notifications (existing)
        .route("/", axum::routing::get(handlers::list))
        .route("/:id/read", axum::routing::post(handlers::mark_read))
        .route("/read-all", axum::routing::post(handlers::mark_all_read))
        .route("/unread-count", axum::routing::get(handlers::unread_count))
        // Notification Rules CRUD
        .route("/rules", axum::routing::get(handlers::list_rules))
        .route("/rules", axum::routing::post(handlers::create_rule))
        .route("/rules/:id", axum::routing::patch(handlers::update_rule))
        .route("/rules/:id", axum::routing::delete(handlers::delete_rule))
        // Notification Queue
        .route("/queue", axum::routing::get(handlers::list_queue))
        .route("/queue", axum::routing::post(handlers::enqueue_notification))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
