//! Email Campaigns Module
//!
//! Groups multiple message templates into sequenced campaigns
//! with timed delays and tag-based triggers.
//!
//! Access: Tenant-scoped (all authenticated users)

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
        .route("/:id/activate", axum::routing::post(handlers::activate))
        .route("/:id/pause", axum::routing::post(handlers::pause))
        // Steps
        .route("/steps", axum::routing::post(handlers::add_step))
        .route(
            "/steps/:step_id",
            axum::routing::patch(handlers::update_step),
        )
        .route(
            "/steps/:step_id",
            axum::routing::delete(handlers::delete_step),
        )
        // Triggers (tag -> campaign)
        .route("/:id/triggers", axum::routing::get(handlers::list_triggers))
        .route("/:id/triggers", axum::routing::post(handlers::add_trigger))
        .route(
            "/triggers/:trigger_id",
            axum::routing::delete(handlers::remove_trigger),
        )
        // Enrollments
        .route(
            "/:id/enrollments",
            axum::routing::get(handlers::list_enrollments),
        )
        .route("/:id/enroll", axum::routing::post(handlers::enroll_contact))
        .route(
            "/enrollments/:enrollment_id",
            axum::routing::patch(handlers::update_enrollment),
        )
        // Action: build full campaign from template names
        .route("/build", axum::routing::post(handlers::build_campaign))
        // Sync a tag from FunnelSwift to CRM Swift
        .route(
            "/sync-tag",
            axum::routing::post(handlers::sync_funnelswift_tag),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
