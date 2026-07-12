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
    clock::DefaultClock,
    state::keyed::DefaultKeyedStateStore,
    Quota, RateLimiter as GovernorRateLimiter,
};
use serde_json::json;
use nonzero_ext::nonzero;
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
    pub auth_limiter: Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
    pub api_limiter: Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
}

impl RateLimiterState {
    /// Create rate limiter instances from configuration
    pub fn from_config(config: &crate::config::AppConfig) -> Self {
        let auth_burst = NonZeroU32::new(config.auth_rate_limit_per_minute)
            .unwrap_or(nonzero!(5u32));
        let api_burst = NonZeroU32::new(config.api_rate_limit_per_minute)
            .unwrap_or(nonzero!(20u32));

        let auth_quota = Quota::per_minute(auth_burst);
        let api_quota = Quota::per_minute(api_burst);

        Self {
            auth_limiter: Arc::new(GovernorRateLimiter::keyed(auth_quota)),
            api_limiter: Arc::new(GovernorRateLimiter::keyed(api_quota)),
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
    let limiter = &state.auth_limiter;

    match limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => rate_limit_response(),
    }
}

/// General API rate limiting middleware — applied to all other routes
pub async fn api_rate_limit_middleware(
    State(state): State<RateLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&req);
    let limiter = &state.api_limiter;

    match limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => rate_limit_response(),
    }
}

/// Extract client IP from request (X-Forwarded-For or connection remote addr)
fn extract_client_ip(req: &Request) -> IpAddr {
    if let Some(forwarded) = req
        .headers()
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(first_ip) = forwarded.split(',').next() {
            if let Ok(ip) = first_ip.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }

    // Fallback to 127.0.0.1 if we can't determine
    IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1))
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
