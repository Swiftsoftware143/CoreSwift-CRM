//! Provider Keys module — API key management for external providers.
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    let public = Router::new()
        .route("/available-providers", axum::routing::get(handlers::list_available_providers));

    let protected = Router::new()
        .route("/provider-keys", axum::routing::get(handlers::list_provider_keys).post(handlers::upsert_provider_key))
        .route("/provider-keys/:provider", axum::routing::get(handlers::get_provider_key).delete(handlers::delete_provider_key))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware));

    Router::new().merge(public).merge(protected)
}
