//! Authentication module — JWT-based auth with Argon2 password hashing.
//!
//! Provides register, login, refresh, logout, invite management, and current-user endpoints.
//! Users are "team members" belonging to an "account" (tenant in DB).

pub mod models;
pub mod middleware;
pub mod handlers;

// Re-export Claims for convenience (used by all modules)
pub use models::Claims;

use axum::Router;

/// Build the auth router. No auth middleware needed on these endpoints.
pub fn router() -> Router<crate::AppState> {
    Router::new()
        .route("/register", axum::routing::post(handlers::register))
        .route("/login", axum::routing::post(handlers::login))
        .route("/refresh", axum::routing::post(handlers::refresh))
        .route("/me", axum::routing::get(handlers::me))
        .route("/logout", axum::routing::post(handlers::logout))
        .route("/invite", axum::routing::post(handlers::create_invite))
        .route("/invites", axum::routing::get(handlers::list_invites))
        .route("/forgot-password", axum::routing::post(handlers::forgot_password))
        .route("/reset-password", axum::routing::post(handlers::reset_password))
}
