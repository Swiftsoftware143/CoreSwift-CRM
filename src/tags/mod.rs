pub mod models;
pub mod handlers;
pub mod internal_handler;
pub mod triggers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/categories", axum::routing::get(handlers::list_categories))
        .route("/categories", axum::routing::post(handlers::create_category))
        .route("/categories/:id", axum::routing::patch(handlers::update_category))
        .route("/categories/:id", axum::routing::delete(handlers::delete_category))
        .route("/", axum::routing::get(handlers::list_tags))
        .route("/", axum::routing::post(handlers::create_tag))
        .route("/:id", axum::routing::get(handlers::get_tag))
        .route("/:id", axum::routing::patch(handlers::update_tag))
        .route("/:id", axum::routing::delete(handlers::delete_tag))
        .route("/assign", axum::routing::post(handlers::assign_tag))
        .route("/assign/:id", axum::routing::delete(handlers::unassign_tag))
        .route("/entity/:entity_type/:entity_id", axum::routing::get(handlers::get_entity_tags))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
