//! CoreSwift booking engine.
//!
//! One engine, several calendar types (`CalendarKind`): appointment, inventory (ad slots), date_range.
//! Mount it from the host app:
//!
//! ```ignore
//! let app = Router::new()
//!     .nest("/api/booking", booking::admin_router(pool.clone()).layer(your_auth_layer)) // must insert Extension<TenantId>
//!     .nest("/public/booking", booking::public_router(pool.clone()));
//! ```

pub mod availability;
pub mod error;
pub mod handlers;
pub mod models;
pub mod notify;
pub mod pricing;
pub mod service;
pub mod tenant;

pub use error::BookingError;
pub use models::{CalendarKind, TenantId};

use axum::{
    routing::{get, patch, post, put},
    Router,
};
use handlers as h;
use sqlx::PgPool;

/// Authenticated routes. The host's auth middleware must insert `Extension(TenantId(..))`.
pub fn admin_router(pool: PgPool) -> Router {
    Router::new()
        .route(
            "/calendars",
            get(h::list_calendars).post(h::create_calendar),
        )
        // what this booking address does at its root
        .route("/host", get(h::get_host).put(h::set_host))
        // ONE READ FOR A WHOLE ACCOUNT (ZaarHub, 2026-11-23): every calendar with its workspace, own
        // domain, positions, tag rule and list rule — and the problems worth looking at. Mounted
        // BEFORE /calendars/:id so the word "summary" is never read as a calendar id.
        .route("/calendars/summary", get(h::calendars_summary))
        .route(
            "/calendars/:id",
            get(h::get_calendar).patch(h::update_calendar),
        )
        // COPY A CALENDAR INTO ANOTHER WORKSPACE (David, 2026-11-23). The console talks to THIS
        // module (/api/booking), so the door has to be here — the same implementation the legacy
        // /api/bookings route calls, never a second copy of it.
        .route("/calendars/:id/clone", post(h::clone_calendar))
        .route("/calendars/:id/resources", post(h::create_resource))
        // the operator filling a position by hand (phone, invoice, a sponsor who never opens the page)
        .route("/calendars/:id/bookings", post(h::admin_create_booking))
        .route("/calendars/:id/bundles", post(h::create_bundle))
        // who hears about a booking, and when to remind
        .route(
            "/calendars/:id/notifications",
            get(h::get_notifications).put(h::set_notifications),
        )
        // Can a customer book this calendar right now, and if not why (David, 2026-11-20).
        .route("/calendars/:id/readiness", get(h::readiness))
        .route("/calendars/:id/availability", get(h::admin_availability))
        // WHAT THE BUYER SEES, with the limits applied — the console renders this instead of
        // describing the settings, so console and public page cannot disagree.
        .route("/calendars/:id/buyer-preview", get(h::buyer_preview))
        .route("/calendars/:id/orders", get(h::list_orders))
        .route("/resources/:id/availability-rules", put(h::set_rules))
        // AN EXISTING POSITION CAN BE CORRECTED OR REMOVED (ZaarHub, 2026-11-23): a mis-priced slot
        // used to be un-fixable once created — the console offered only "Slots".
        .route(
            "/resources/:id",
            patch(h::update_resource).delete(h::delete_resource),
        )
        .route("/resources/:id/slots/generate", post(h::generate_slots))
        .route("/slots/:id", patch(h::update_slot))
        .route("/orders/:id", get(h::admin_get_order))
        .route("/orders/:id/confirm", post(h::confirm_order))
        .route("/orders/:id/cancel", post(h::cancel_order))
        .route("/bookings/:id/creative/review", post(h::review_creative))
        .with_state(pool)
}

/// Unauthenticated routes for booking pages and embeds. Put rate limiting in front of these.
pub fn public_router(pool: PgPool) -> Router {
    Router::new()
        .route("/c/:slug", get(h::public_calendar))
        .route("/c/:slug/availability", get(h::public_availability))
        // What the page is showing and why — the line that tells a visitor more dates open later.
        .route("/c/:slug/limits", get(h::public_limits))
        .route("/c/:slug/quote", post(h::public_quote))
        .route("/c/:slug/orders", post(h::public_create_order))
        .route("/orders/:id", get(h::public_get_order))
        .route(
            "/orders/:order_id/bookings/:booking_id/creative",
            put(h::public_submit_creative),
        )
        // The manage link: the customer reschedules, cancels or rebooks with no account. The token
        // IS the credential and it is scoped to one order.
        // the root of a booking address: redirect where the owner said, or list what is bookable
        .route("/host", get(h::public_host))
        .route("/manage/:token", get(h::manage_view))
        .route("/manage/:token/cancel", post(h::manage_cancel))
        .route("/manage/:token/reschedule", post(h::manage_reschedule))
        .route("/manage/:token/rebook", post(h::manage_rebook))
        .with_state(pool)
}
