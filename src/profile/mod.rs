//! Profile module — the signed-in user's own record.
//!
//! The workspace SPA has always had a Profile tab (name field + change-password form) that PUT
//! `/api/profile` and PUT `/api/profile/password`. Neither route was ever registered, so both
//! answered the router fallback (405) and the tab's two Save buttons could never succeed.
//! These handlers back exactly those two calls, plus GET so the tab can re-read its own record.
//!
//! GET  /api/profile           — the caller's own profile
//! PUT  /api/profile           — rename the caller's own profile
//! PUT  /api/profile/password  — change the caller's password (verifies the current one)

pub mod handlers;

use crate::AppState;
use axum::{middleware, routing::get, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/",
            get(handlers::get_profile).put(handlers::update_profile),
        )
        .route("/password", axum::routing::put(handlers::change_password))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
