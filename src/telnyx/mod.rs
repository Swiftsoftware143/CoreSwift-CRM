//! Telnyx SMS/Voice integration module.
//!
//! Provides:
//! - Outbound SMS sending
//! - Inbound webhook receiver (calls & SMS)
//! - Phone number management (list, purchase, release, search)
//! - Telnyx config management (API key, messaging profile)

pub mod handlers;
pub mod verify;

use crate::AppState;
use axum::{middleware, Router};

/// Build the Telnyx route tree.
/// Public routes — CREDENTIAL = the Telnyx Ed25519 signature on the delivery, verified by
/// `verify::verify()` before the handler reads the event (kanban t_fd5000e1):
///   POST /api/telnyx/webhook — Telnyx voice/call webhook receiver (unauthenticated; signature-verified)
///   POST /api/telnyx/sms-webhook — Telnyx SMS webhook receiver (unauthenticated; signature-verified)
/// Both stay on `auth::route_policy::PUBLIC_ROUTES` because Telnyx cannot present a JWT; the
/// signature is the credential, and a delivery that fails it is answered 401 (503 when this
/// deployment has no `TELNYX_PUBLIC_KEY` set at all).
/// Protected routes:
///   POST /api/telnyx/send-sms  — Send SMS
///   GET  /api/telnyx/numbers    — List purchased numbers
///   POST /api/telnyx/numbers    — Purchase/assign a number
///   DELETE /api/telnyx/numbers/:id — Release/unassign a number
///   GET  /api/telnyx/available   — Search available numbers
/// Admin routes:
///   GET  /api/telnyx/config      — Get global Telnyx config
///   PUT  /api/telnyx/config      — Save/update Telnyx config
pub fn router(state: AppState) -> Router<AppState> {
    // Public routes — Telnyx sends webhook callbacks here
    // Public webhook receivers — Telnyx calls these with no credential, so the credential is the
    // Ed25519 signature on the delivery itself (`src/telnyx/verify.rs`, kanban t_fd5000e1), checked
    // before either handler reads the event. Both read a JSON body, so the body-read deadline goes
    // on them (kanban t_59745689). Nothing is behind auth here, so there is no ordering question:
    // the deadline is simply the innermost layer of this chain.
    let public = Router::new()
        .route("/webhook", axum::routing::post(handlers::webhook))
        .route("/sms-webhook", axum::routing::post(handlers::sms_webhook))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ));

    // Protected routes — require auth
    let protected = Router::new()
        .route("/send-sms", axum::routing::post(handlers::send_sms))
        .route("/numbers", axum::routing::get(handlers::list_numbers))
        .route("/numbers", axum::routing::post(handlers::purchase_number))
        .route(
            "/numbers/:id",
            axum::routing::delete(handlers::delete_number),
        )
        .route(
            "/available",
            axum::routing::get(handlers::search_available_numbers),
        )
        .route("/config", axum::routing::get(handlers::get_config))
        .route("/config", axum::routing::put(handlers::update_config))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "telnyx", "SMS & voice (Telnyx)"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new().merge(public).merge(protected)
}
