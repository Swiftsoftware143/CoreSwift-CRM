//! Dashboard module — aggregate stats for tenant home view

pub mod handlers;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/stats", axum::routing::get(handlers::stats))
        .route("/search/query", axum::routing::get(handlers::search_query))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
