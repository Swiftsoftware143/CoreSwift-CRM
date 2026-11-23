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
//! - (retired) POST /api/bookings/checkout        — nothing can leave a booking held to confirm
//! - POST   /api/bookings/verify-payment          — called by Stripe webhook, activate booking
//! - GET    /api/bookings/bookings               — list bookings (admin)
//! - POST   /api/bookings/bookings/:id/cancel    — cancel booking
//! - PATCH  /api/bookings/bookings/:id/adjust-slots — admin adjust slot count

pub mod clone;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/calendars", axum::routing::get(handlers::list_calendars))
        .route("/calendars", axum::routing::post(handlers::create_calendar))
        .route(
            "/calendars/:slug",
            axum::routing::get(handlers::get_calendar),
        )
        .route(
            "/calendars/:slug",
            axum::routing::patch(handlers::update_calendar),
        )
        .route(
            "/calendars/:slug/subdomain",
            axum::routing::put(handlers::set_calendar_subdomain),
        )
        .route(
            "/calendars/:slug/subdomain",
            axum::routing::delete(handlers::clear_calendar_subdomain),
        )
        .route(
            "/calendars/:slug/slots",
            axum::routing::get(handlers::list_slots),
        )
        .route(
            "/calendars/:slug/slots",
            axum::routing::post(handlers::create_slot),
        )
        .route(
            "/calendars/:slug/slots/:slot_id",
            axum::routing::patch(handlers::update_slot),
        )
        .route(
            "/calendars/:slug/slots/:slot_id",
            axum::routing::delete(handlers::delete_slot),
        )
        .route(
            "/calendars/:slug/questions",
            axum::routing::get(handlers::get_questions),
        )
        .route(
            "/calendars/:slug/available",
            axum::routing::get(handlers::get_available),
        )
        // POST /checkout was RETIRED (2026-11-21): it marked a booking 'active' that something else
        // had left held, and NOTHING in this app ever creates a booking in any state but 'active' -
        // the public checkout writes it active at creation. So the route could not be reached by any
        // writer and no screen could honestly offer it. The bookings list, per-booking read and the
        // cancel arm stay, and are now reachable from the "Bookings taken" screen.
        .route("/bookings", axum::routing::get(handlers::list_bookings))
        .route("/bookings/:id", axum::routing::get(handlers::get_booking))
        .route(
            "/bookings/:id/cancel",
            axum::routing::post(handlers::cancel_booking),
        )
        // `PATCH /bookings/bookings/:id/adjust-slots` was DELETED (kanban t_4b6f1a5c): it took a
        // SLOT id under a path that says "bookings", and it was a strict subset of
        // `PATCH /bookings/calendars/:slug/slots/:slot_id` above (same column, no slug scoping,
        // no existence check against the slug). Two implementations of one capability is drift;
        // the slug-scoped one is the survivor the Bookings tab drives.
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "bookings",
                "Bookings & scheduling",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// Internal (x-internal-key auth) endpoints for cross-app calendar provisioning
pub fn internal_router() -> Router<AppState> {
    Router::new()
        .route(
            "/calendars",
            axum::routing::post(handlers::internal_create_calendar),
        )
        .route(
            "/slots/default",
            axum::routing::post(handlers::internal_create_default_slot),
        )
}

/// Public (no-auth) endpoints for booking portal access
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route(
            "/public/slots/questions",
            axum::routing::get(handlers::public_questions),
        )
        .route(
            "/public/slots/available/:tenant_id",
            axum::routing::get(handlers::public_available),
        )
        .route(
            "/public/checkout",
            axum::routing::post(handlers::public_create_booking),
        )
}
