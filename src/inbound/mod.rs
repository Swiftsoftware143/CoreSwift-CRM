//! Inbound webhook module — receive events from satellite apps via API key

pub mod handlers;

use axum::Router;
use crate::AppState;

/// Build router for public inbound webhook endpoints.
/// No auth middleware — authentication is via key_prefix in URL.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/:key_prefix/:event_type", axum::routing::post(handlers::receive))
        .route("/v2/:key_prefix/:event_type", axum::routing::post(handlers::receive_v2))
        .route("/v3/:key_prefix/:event_type", axum::routing::post(handlers::receive_v3_contact_sync))
}
