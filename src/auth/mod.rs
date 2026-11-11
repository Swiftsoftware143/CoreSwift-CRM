//! Authentication module — JWT-based auth with Argon2 password hashing.
//!
//! Provides register, login, refresh, logout, invite management, and current-user endpoints.
//! Users are "team members" belonging to an "account" (tenant in DB).

pub mod boundary;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod platform_admin;
pub mod route_policy;
/// The ONE account-minting writer both the signup door and the machine (tag) door call
/// (kanban t_e968e9ad).
pub mod signup;

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
        // The way back in when signup MINTED a password and the mail carrying it did not go out
        // (kanban t_d841df4f). Authenticated like every other act on the caller's own account: the
        // token the signup response already carried IS the credential, and the one-shot
        // `users.password_undelivered` latch bounds the offer. Deliberately NOT public — it is on
        // this guarded group, so the boundary refuses an anonymous caller before the handler runs.
        .route(
            "/initial-password",
            axum::routing::post(handlers::set_initial_password),
        )
        // Ask to move the caller's OWN sign-in address (kanban t_bd31809e). Authenticated like every
        // other act on the caller's own account — the handler reads the user id from the SESSION, so
        // there is no path id to spoof — and the address only moves when the link mailed to it is
        // opened, which is the CONFIRM arm below.
        .route(
            "/email-change",
            axum::routing::post(handlers::request_email_change),
        )
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
    // PASSWORD RECOVERY — its own group with the STRICTEST limit. David, 2026-10-02: *"if you mean
    // forgot password then yeah 3 attempts"*. Split out of `public` because these two routes SEND AN
    // EMAIL: the abuse is mail-bombing a real person and probing reset tokens, not general traffic.
    let password = Router::new()
        .route(
            "/forgot-password",
            axum::routing::post(handlers::forgot_password),
        )
        .route(
            "/reset-password",
            axum::routing::post(handlers::reset_password),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.rate_limiter.clone(),
            crate::rate_limiter::password_rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ));

    let public = Router::new()
        .route("/register", axum::routing::post(handlers::register))
        .route("/login", axum::routing::post(handlers::login))
        .route("/refresh", axum::routing::post(handlers::refresh))
        .route("/me", axum::routing::get(handlers::me))
        .route("/logout", axum::routing::post(handlers::logout))
        .route("/invites", axum::routing::get(handlers::list_invites))
        // The credential-guessing surface: login/register/refresh. This is the limiter that stops
        // password guessing, and it is a SEPARATE bucket from password recovery on purpose — one reset
        // request must not eat a caller's login allowance.
        .layer(axum::middleware::from_fn_with_state(
            state.rate_limiter.clone(),
            crate::rate_limiter::auth_rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ));

    // The sign-in address change's SECOND arm (kanban t_bd31809e). The customer opens the mailed link
    // in a browser and carries NO session — the token in the address IS the credential — so this route
    // must answer anonymously and is named in `route_policy::PUBLIC_ROUTES`. It gets the body deadline
    // but deliberately NOT the credential-guessing limiter: the link is often opened from a shared
    // office IP that has already spent its login allowance, there is nothing to guess (a 122-bit uuid
    // token) and no mail to bomb, and a redeemed token is refused by the CLAIM itself, not a counter.
    let email_confirm = Router::new()
        .route(
            "/email-change/confirm",
            axum::routing::post(handlers::confirm_email_change),
        )
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            crate::body_deadline::body_read_deadline_middleware,
        ));

    Router::new()
        .merge(public)
        .merge(password)
        .merge(protected)
        .merge(email_confirm)
}
