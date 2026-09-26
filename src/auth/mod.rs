//! Authentication module — JWT-based auth with Argon2 password hashing.
//!
//! Provides register, login, refresh, logout, invite management, and current-user endpoints.
//! Users are "team members" belonging to an "account" (tenant in DB).

pub mod handlers;
pub mod middleware;
pub mod models;
pub mod platform_admin;

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
    // Body-read deadline (kanban t_59745689): how long a request body may take to arrive before the
    // request is answered 408 with `Connection: close`. Mounted on both groups below.
    let body_deadline =
        crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs);

    let protected = Router::new()
        .route("/invite", axum::routing::post(handlers::create_invite))
        .route("/me/usage", axum::routing::get(handlers::get_usage))
        // INNERMOST, deliberately: `auth_middleware` is applied below (a later `.layer()` is
        // OUTERMOST in axum) so it stays outside the deadline. An unauthenticated request with a
        // declared body is answered 401 at once without its body ever being read, and a declared
        // body is never buffered before the credential has been checked.
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::auth_middleware,
        ));

    // The public group reads bodies too (/register, /login, /refresh, /forgot-password,
    // /reset-password all take a `Json` payload), and a stranger can reach every one of them.
    let public = Router::new()
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
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ));

    Router::new().merge(public).merge(protected)
}
