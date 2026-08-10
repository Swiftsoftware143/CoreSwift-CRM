pub mod handlers;
pub mod models;
pub mod opportunity;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list_pipelines))
        .route("/", axum::routing::post(handlers::create_pipeline))
        .route("/:id", axum::routing::get(handlers::get_pipeline))
        .route("/:id", axum::routing::patch(handlers::update_pipeline))
        .route("/:id", axum::routing::delete(handlers::delete_pipeline))
        .route(
            "/:pipeline_id/stages",
            axum::routing::get(handlers::list_stages),
        )
        .route(
            "/:pipeline_id/stages",
            axum::routing::post(handlers::create_stage),
        )
        .route(
            "/:pipeline_id/stages/:stage_id",
            axum::routing::patch(handlers::update_stage),
        )
        .route(
            "/:pipeline_id/stages/:stage_id",
            axum::routing::delete(handlers::delete_stage),
        )
        .route(
            "/:pipeline_id/stages/:stage_id/move/:opportunity_id",
            axum::routing::patch(handlers::move_opportunity),
        )
        .route(
            "/:pipeline_id/opportunities",
            axum::routing::get(opportunity::list),
        )
        .route(
            "/:pipeline_id/opportunities",
            axum::routing::post(opportunity::create),
        )
        .route(
            "/:pipeline_id/opportunities/:id",
            axum::routing::get(opportunity::get),
        )
        .route(
            "/:pipeline_id/opportunities/:id",
            axum::routing::patch(opportunity::update),
        )
        .route(
            "/:pipeline_id/opportunities/:id",
            axum::routing::delete(opportunity::delete),
        )
        .route(
            "/:pipeline_id/analytics",
            axum::routing::get(handlers::pipeline_analytics),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
