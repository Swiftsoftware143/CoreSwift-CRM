//! Event Bus — centralized webhook listener and event dispatcher.
//!
//! All external services (landing pages, directories, SaaS) send webhooks here.
//! The event bus normalizes them, stores them, and dispatches to:
//!   - Automation rules (trigger matching)
//!   - The "If-Not-Then" delay engine
//!   - External webhook endpoints

pub mod dispatcher;
pub mod handlers;
pub mod models;
pub mod prepopulate;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // External webhook intake — accepts events from any source
        .route("/ingest/:source", axum::routing::post(handlers::ingest))
        .route("/ingest/:source", axum::routing::get(handlers::ingest_get))
        // Event querying
        .route("/", axum::routing::get(handlers::list_events))
        .route("/:id", axum::routing::get(handlers::get_event))
        // Delayed action management (If-Not-Then)
        .route("/delayed", axum::routing::get(handlers::list_delayed))
        .route("/delayed", axum::routing::post(handlers::schedule_delayed))
        .route(
            "/delayed/:id",
            axum::routing::delete(handlers::cancel_delayed),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
