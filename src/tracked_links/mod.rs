pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list_tracked_links))
        .route("/", axum::routing::post(handlers::create_tracked_link))
        .route("/:id", axum::routing::delete(handlers::delete_tracked_link))
        // Plan gating — the admin controls this module per plan
        // (features::FEATURE_REGISTRY is the source of truth for the admin UI).
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "tracked_links", "Tracked links"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// Public (no auth) redirect router — mounted at /track
pub fn public_router() -> Router<AppState> {
    Router::new().route(
        "/:slug",
        axum::routing::get(handlers::redirect_tracked_link),
    )
}
