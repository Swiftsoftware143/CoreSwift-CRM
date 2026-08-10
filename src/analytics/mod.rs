//! Analytics module.
//!
//! Pipeline analytics, score distribution, tag usage, and client usage stats.

pub mod handlers;

use crate::AppState;
use axum::{middleware, Router};

/// Build the analytics router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/pipelines", axum::routing::get(handlers::pipeline_stats))
        .route("/scores", axum::routing::get(handlers::score_distribution))
        .route("/tags", axum::routing::get(handlers::tag_usage))
        .route("/contacts", axum::routing::get(handlers::contact_stats))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
