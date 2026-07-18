pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list_tracked_links))
        .route("/", axum::routing::post(handlers::create_tracked_link))
        .route("/:id", axum::routing::delete(handlers::delete_tracked_link))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}

/// Public (no auth) redirect router — mounted at /track
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/:slug", axum::routing::get(handlers::redirect_tracked_link))
}
