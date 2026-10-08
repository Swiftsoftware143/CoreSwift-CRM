pub mod branding;
pub mod handlers;
pub mod models;
pub mod settings;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .route("/:id/settings", axum::routing::get(handlers::get_settings))
        .route(
            "/:id/settings",
            axum::routing::patch(handlers::update_settings),
        )
        // Per-tenant email branding (kanban t_feab8aff). The id comes from the JWT claim, NOT the
        // path, so there is nothing to spoof; these are the only writers of `logo_url`.
        .route(
            "/branding/logo",
            axum::routing::post(branding::upload_logo).delete(branding::delete_logo),
        )
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
