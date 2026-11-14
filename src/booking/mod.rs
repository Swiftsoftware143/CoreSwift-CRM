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
        .route("/calendars/:id", get(h::get_calendar))
        .route("/calendars/:id/resources", post(h::create_resource))
        .route("/calendars/:id/bundles", post(h::create_bundle))
        .route("/calendars/:id/availability", get(h::admin_availability))
        .route("/calendars/:id/orders", get(h::list_orders))
        .route("/resources/:id/availability-rules", put(h::set_rules))
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
        .route("/c/:slug/quote", post(h::public_quote))
        .route("/c/:slug/orders", post(h::public_create_order))
        .route("/orders/:id", get(h::public_get_order))
        .route(
            "/orders/:order_id/bookings/:booking_id/creative",
            put(h::public_submit_creative),
        )
        .with_state(pool)
}
