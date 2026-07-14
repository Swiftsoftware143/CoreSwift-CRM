//! Telnyx SMS/Voice integration module.
//!
//! Provides:
//! - Outbound SMS sending
//! - Inbound webhook receiver (calls & SMS)
//! - Phone number management (list, purchase, release, search)
//! - Telnyx config management (API key, messaging profile)

pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

/// Build the Telnyx route tree.
///
/// Public routes (no auth):
///   POST /api/telnyx/webhook — Telnyx webhook receiver (unauthenticated)
///
/// Protected routes:
///   POST /api/telnyx/send-sms  — Send SMS
///   GET  /api/telnyx/numbers    — List purchased numbers
///   POST /api/telnyx/numbers    — Purchase/assign a number
///   DELETE /api/telnyx/numbers/:id — Release/unassign a number
///   GET  /api/telnyx/available   — Search available numbers
///
/// Admin routes:
///   GET  /api/telnyx/config      — Get global Telnyx config
///   PUT  /api/telnyx/config      — Save/update Telnyx config
pub fn router(state: AppState) -> Router<AppState> {
    // Public routes — Telnyx sends webhook callbacks here
    let public = Router::new()
        .route("/webhook", axum::routing::post(handlers::webhook))
        .route("/sms-webhook", axum::routing::post(handlers::sms_webhook));

    // Protected routes — require auth
    let protected = Router::new()
        .route("/send-sms", axum::routing::post(handlers::send_sms))
        .route("/numbers", axum::routing::get(handlers::list_numbers))
        .route("/numbers", axum::routing::post(handlers::purchase_number))
        .route("/numbers/:id", axum::routing::delete(handlers::delete_number))
        .route("/available", axum::routing::get(handlers::search_available_numbers))
        .route("/config", axum::routing::get(handlers::get_config))
        .route("/config", axum::routing::put(handlers::update_config))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware));

    Router::new()
        .merge(public)
        .merge(protected)
}
