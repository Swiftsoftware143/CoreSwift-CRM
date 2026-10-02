//! Rate limiting middleware using the `governor` crate.
//!
//! Provides two rate limiters:
//! - Auth routes: stricter limit (default 5/min per IP)
//! - API routes: higher limit (default 20/min per IP)
//!
//! Uses the client IP as the rate limit key.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use governor::{
    clock::DefaultClock, state::keyed::DefaultKeyedStateStore, Quota,
    RateLimiter as GovernorRateLimiter,
};
use nonzero_ext::nonzero;
use serde_json::json;
use std::net::IpAddr;
use std::num::NonZeroU32;
use std::sync::Arc;

/// Rate limiter kind for distinguishing auth vs API limits
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RateLimitKind {
    Auth,
    Api,
}

/// Combined rate limiter state
#[derive(Clone)]
pub struct RateLimiterState {
    pub auth_limiter:
        Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
    pub api_limiter: Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
    /// Password reset/recovery — the strictest of the three. David, 2026-10-02: *"if you mean forgot
    /// password then yeah 3 attempts"*. Its own limiter rather than a share of the auth one, because a
    /// reset request sends an EMAIL: the abuse is mail-bombing a real person and probing reset tokens,
    /// not the general request rate.
    pub password_limiter:
        Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
}

impl RateLimiterState {
    /// Create rate limiter instances from configuration
    pub fn from_config(config: &crate::config::AppConfig) -> Self {
        let auth_burst =
            NonZeroU32::new(config.auth_rate_limit_per_minute).unwrap_or(nonzero!(5u32));
        let api_burst =
            NonZeroU32::new(config.api_rate_limit_per_minute).unwrap_or(nonzero!(20u32));

        let auth_quota = Quota::per_minute(auth_burst);
        let api_quota = Quota::per_minute(api_burst);

        // 3 attempts per window (default 15 minutes). Window-shaped rather than per-minute because
        // "3 per minute" would allow 180 reset emails an hour — no protection at all for the case it
        // exists for.
        let password_max =
            NonZeroU32::new(config.password_rate_limit_max).unwrap_or(nonzero!(3u32));
        // `Quota::with_period` returns Option (None on a zero period), so the fallback takes no
        // argument — and `max(1)` means a mistyped env var degrades to a 1-minute window instead of
        // silently disabling the limit.
        let password_window = std::time::Duration::from_secs(
            60 * config.password_rate_limit_window_minutes.max(1) as u64,
        );
        let password_quota = Quota::with_period(password_window)
            .unwrap_or_else(|| Quota::per_minute(password_max))
            .allow_burst(password_max);

        Self {
            auth_limiter: Arc::new(GovernorRateLimiter::keyed(auth_quota)),
            api_limiter: Arc::new(GovernorRateLimiter::keyed(api_quota)),
            password_limiter: Arc::new(GovernorRateLimiter::keyed(password_quota)),
        }
    }
}

/// Auth rate limiting middleware — applied to `/api/auth/*` routes
pub async fn auth_rate_limit_middleware(
    State(state): State<RateLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&req);
    if is_internal(ip) {
        return next.run(req).await;
    }
    match state.auth_limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => rate_limit_response(),
    }
}

/// Password recovery middleware — applied to `/forgot-password` and `/reset-password`.
///
/// Strictest limit in the app, and its own bucket: these two routes EMAIL somebody, so the abuse is
/// mail-bombing a real person and probing reset tokens, not general request volume.
pub async fn password_rate_limit_middleware(
    State(state): State<RateLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&req);
    if is_internal(ip) {
        return next.run(req).await;
    }
    match state.password_limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => rate_limit_response(),
    }
}

/// General API rate limiting middleware.
///
/// MOUNTED ON: nothing, yet — deliberately, and this is the reasoning rather than an omission.
/// The configured limit is 20/minute per IP. The admin console loads dozens of endpoints in one page
/// load from a SINGLE IP, so a global 20/min limit would throttle the console for real users, turning a
/// protection into an outage. Rate limiting browser traffic needs a limit sized for a browser session
/// (and ideally a separate bucket for authenticated console traffic), which is a product decision, not a
/// default to guess. The routes that actually needed protection — credential guessing and password
/// recovery — are limited: see auth_rate_limit_middleware and password_rate_limit_middleware.
/// General API references: `provider_keys`-style public endpoints and webhook receivers.
pub async fn api_rate_limit_middleware(
    State(state): State<RateLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&req);
    if is_internal(ip) {
        return next.run(req).await;
    }
    match state.api_limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => rate_limit_response(),
    }
}

/// On-box callers are not throttled: the app's port is published to loopback only, so a request that
/// arrives without our proxy's headers came from this machine (health checks, the fleet's own
/// verification) and cannot be an outside caller.
fn is_internal(ip: IpAddr) -> bool {
    ip.is_loopback()
}

/// Who is calling — used as the rate-limit key, so getting it wrong either throttles the world or
/// throttles nobody.
///
/// Measured 2026-10-02 before turning the limiter on, against this fleet's nginx:
///   * `proxy_set_header X-Real-IP $remote_addr` — nginx REPLACES this, so a client cannot forge it.
///   * `proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for` — nginx APPENDS, so anything the
///     client sent sits IN FRONT of the address our proxy added.
///
/// The previous code returned the FIRST `X-Forwarded-For` element — the client-controlled one. A caller
/// could send a different fake value on every request and never hit a limit, which made the whole
/// limiter decorative. The trustworthy entry in an appended chain is the LAST one: that is the address
/// our own nginx saw connect.
fn extract_client_ip(req: &Request) -> IpAddr {
    if let Some(real) = req.headers().get("X-Real-IP").and_then(|v| v.to_str().ok()) {
        if let Ok(ip) = real.trim().parse::<IpAddr>() {
            return ip;
        }
    }

    if let Some(forwarded) = req
        .headers()
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
    {
        // rsplit: the LAST hop is the one our proxy appended.
        if let Some(last_ip) = forwarded.rsplit(',').next() {
            if let Ok(ip) = last_ip.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }

    // No proxy headers at all: an on-box caller (see is_internal).
    IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

/// Generate a 429 Too Many Requests response
fn rate_limit_response() -> Response {
    let body = Json(json!({
        "error": true,
        "message": "Too many requests. Please slow down.",
        "code": 429
    }));

    (StatusCode::TOO_MANY_REQUESTS, body).into_response()
}
