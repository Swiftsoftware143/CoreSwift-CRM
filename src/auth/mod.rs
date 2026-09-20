//! Authentication module — JWT-based auth with Argon2 password hashing.
//!
//! Provides register, login, refresh, logout, invite management, and current-user endpoints.
//! Users are "team members" belonging to an "account" (tenant in DB).

pub mod handlers;
pub mod middleware;
pub mod models;

// Re-export Claims for convenience (used by all modules)
pub use models::Claims;

use crate::AppState;
use axum::Router;

/// Build the auth router.
///
/// Two groups:
///   * public — `/register`, `/login`, `/refresh`, `/forgot-password`,
///     `/reset-password` (no token) plus `/me`, `/logout`, `/invites`, which
///     parse the `Authorization` header themselves via `extract_claims`.
///   * `auth_middleware`-guarded — `/invite` and `/me/usage` extract
///     `Extension<Claims>`, so the injecting middleware MUST run for them.
///     Without the layer the extractor rejects every caller with HTTP 500
///     ("Missing request extension: Extension of type Claims").
pub fn router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/invite", axum::routing::post(handlers::create_invite))
        .route("/me/usage", axum::routing::get(handlers::get_usage))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ));

    Router::new()
        .route("/register", axum::routing::post(handlers::register))
        .route("/login", axum::routing::post(handlers::login))
        .route("/refresh", axum::routing::post(handlers::refresh))
        .route("/me", axum::routing::get(handlers::me))
        .route("/logout", axum::routing::post(handlers::logout))
        .route("/invites", axum::routing::get(handlers::list_invites))
        .route(
            "/forgot-password",
            axum::routing::post(handlers::forgot_password),
        )
        .route(
            "/reset-password",
            axum::routing::post(handlers::reset_password),
        )
        .merge(protected)
}
