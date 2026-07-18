//! Booking calendar module — slot inventory, date-range booking, file upload, payment-first flow.
//!
//! Routes:
//! - GET    /api/bookings/calendars              — list calendars (tenant-scoped)
//! - POST   /api/bookings/calendars              — create calendar
//! - GET    /api/bookings/calendars/:slug/slots  — list slot types + availability
//! - POST   /api/bookings/calendars/:slug/slots  — create slot type
//! - PATCH  /api/bookings/calendars/:slug/slots/:id — update slot config
//! - GET    /api/bookings/calendars/:slug/questions — return Q&A form fields
//! - GET    /api/bookings/calendars/:slug/available?start=&end= — time-slot availability
//! - POST   /api/bookings/checkout               — create booking + return payment URL
//! - POST   /api/bookings/verify-payment          — called by Stripe webhook, activate booking
//! - GET    /api/bookings/bookings               — list bookings (admin)
//! - POST   /api/bookings/bookings/:id/cancel    — cancel booking
//! - PATCH  /api/bookings/bookings/:id/adjust-slots — admin adjust slot count

pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/calendars", axum::routing::get(handlers::list_calendars))
        .route("/calendars", axum::routing::post(handlers::create_calendar))
        .route("/calendars/:slug", axum::routing::get(handlers::get_calendar))
        .route("/calendars/:slug", axum::routing::patch(handlers::update_calendar))
        .route("/calendars/:slug/slots", axum::routing::get(handlers::list_slots))
        .route("/calendars/:slug/slots", axum::routing::post(handlers::create_slot))
        .route("/calendars/:slug/slots/:slot_id", axum::routing::patch(handlers::update_slot))
        .route("/calendars/:slug/slots/:slot_id", axum::routing::delete(handlers::delete_slot))
        .route("/calendars/:slug/questions", axum::routing::get(handlers::get_questions))
        .route("/calendars/:slug/available", axum::routing::get(handlers::get_available))
        .route("/checkout", axum::routing::post(handlers::create_checkout))

        .route("/bookings", axum::routing::get(handlers::list_bookings))
        .route("/bookings/:id", axum::routing::get(handlers::get_booking))
        .route("/bookings/:id/cancel", axum::routing::post(handlers::cancel_booking))
        .route("/bookings/:id/adjust-slots", axum::routing::patch(handlers::adjust_slot_config))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}

/// Internal (x-internal-key auth) endpoints for cross-app calendar provisioning
pub fn internal_router() -> Router<AppState> {
    Router::new()
        .route("/calendars", axum::routing::post(handlers::internal_create_calendar))
        .route("/slots/default", axum::routing::post(handlers::internal_create_default_slot))
}

/// Public (no-auth) endpoints for booking portal access
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/public/slots/questions", axum::routing::get(handlers::public_questions))
        .route("/public/slots/available/:tenant_id", axum::routing::get(handlers::public_available))
        .route("/public/checkout", axum::routing::post(handlers::public_create_booking))

}
