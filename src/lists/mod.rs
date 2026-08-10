pub mod evaluator;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .route("/:id/members", axum::routing::get(handlers::list_members))
        .route("/:id/members", axum::routing::post(handlers::add_member))
        .route(
            "/:id/members/:contact_id",
            axum::routing::delete(handlers::remove_member),
        )
        .route(
            "/:id/evaluate",
            axum::routing::post(handlers::evaluate_list),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
