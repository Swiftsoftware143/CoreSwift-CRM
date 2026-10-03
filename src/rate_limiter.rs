//! Rate limiting middleware using the `governor` crate.
//!
//! Provides three rate limiters, all keyed by the client IP:
//! - Auth routes: stricter limit (default 5/min per IP) — credential guessing.
//! - Password recovery: strictest, window-shaped (default 3 per 15 min per IP) — reset mailing.
//! - General API: browser-sized (default 120/min per IP), plus a SEPARATE and more generous bucket
//!   (default 600/min per IP) for requests that present a credential, so a signed-in console session
//!   is never throttled at the anonymous rate. See `api_rate_limit_middleware`.

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
    /// The general API bucket (kanban t_3130f105), in two tiers that share this struct: `api_limiter`
    /// carries traffic that presents no credential, `console_limiter` carries traffic that does. Both
    /// are keyed by the CLIENT IP — a header cannot be rotated to mint a fresh bucket — so the
    /// credential only selects a tier and can never escape the caller's own per-IP bound.
    pub console_limiter:
        Arc<GovernorRateLimiter<IpAddr, DefaultKeyedStateStore<IpAddr>, DefaultClock>>,
}

impl RateLimiterState {
    /// Create rate limiter instances from configuration
    pub fn from_config(config: &crate::config::AppConfig) -> Self {
        let auth_burst =
            NonZeroU32::new(config.auth_rate_limit_per_minute).unwrap_or(nonzero!(5u32));
        let api_burst =
            NonZeroU32::new(config.api_rate_limit_per_minute).unwrap_or(nonzero!(120u32));
        let console_burst =
            NonZeroU32::new(config.console_rate_limit_per_minute).unwrap_or(nonzero!(600u32));

        let auth_quota = Quota::per_minute(auth_burst);
        let api_quota = Quota::per_minute(api_burst);
        let console_quota = Quota::per_minute(console_burst);

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
            console_limiter: Arc::new(GovernorRateLimiter::keyed(console_quota)),
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

/// General API rate limiting middleware — MOUNTED on the API surface (kanban t_3130f105).
///
/// The card this closes offered mount-or-retire; this MOUNTS it, with a browser-sized limit and a
/// separate bucket for credentialed console traffic.
///
/// Sizing is measured rather than guessed. A console page load calls a handful of `/api/` endpoints
/// (the app shell references 66 distinct paths in total, and the fleet's 24 h access-log peak for
/// `/api/` traffic from a single client IP is ~215/min — a deliberate flood in an earlier card's
/// proof, not real usage). So:
///   * `api_limiter`     — 120/min per IP (`API_RATE_LIMIT_PER_MINUTE`): requests on the API surface
///                         that present NO credential. ~2/s, far above any anonymous page load, and it
///                         is the bucket a scanner or a form-spammer hits.
///   * `console_limiter` — 600/min per IP (`CONSOLE_RATE_LIMIT_PER_MINUTE`): requests that present
///                         a bearer credential in the `Authorization` header. 10/s gives a signed-in
///                         console session headroom above the anonymous bucket while still being a bound.
///
/// Both buckets are keyed by the CLIENT IP, so the credential only selects a tier — it cannot mint a
/// fresh bucket, and a rotated or forged token still counts against the caller's own IP. That is why
/// tier selection from a client-supplied header is safe here: the worst it can buy is the higher of
/// two per-IP ceilings.
///
/// Exempt (never limited here): non-API paths (SPA shells and static assets), the two health probes,
/// and the machine-to-machine receivers — those authenticate their CALLER by shared secret and are
/// paced by their senders, so a per-IP ceiling is the wrong instrument. Enumerated in
/// `EXEMPT_PREFIXES`. On-box callers are skipped entirely (see `is_internal`).
pub async fn api_rate_limit_middleware(
    State(state): State<RateLimiterState>,
    req: Request,
    next: Next,
) -> Response {
    // Owned so the request can still be moved into `next.run` below.
    let path = req.uri().path().to_string();
    if !is_rate_limited_surface(&path) {
        return next.run(req).await;
    }

    let ip = extract_client_ip(&req);
    if is_internal(ip) {
        return next.run(req).await;
    }

    let (limiter, tier) = if has_bearer_credential(req.headers()) {
        (&state.console_limiter, "console")
    } else {
        (&state.api_limiter, "anonymous")
    };

    match limiter.check_key(&ip) {
        Ok(_) => next.run(req).await,
        Err(_) => {
            tracing::warn!(
                "General API rate limit exceeded ({}) for {} on {} {}",
                tier,
                ip,
                req.method(),
                path
            );
            rate_limit_response()
        }
    }
}

/// Machine-to-machine receivers that the general API limiter must never throttle: each one
/// authenticates its CALLER (shared secret, provider signature, allowlisted app) and is paced by its
/// sender, so a per-IP ceiling here would break an integration rather than protect anything. Kept as
/// data so the exemption list is reviewable in ONE place instead of spread across the router.
const EXEMPT_PREFIXES: &[&str] = &[
    // Liveness — polled by monitors.
    "/api/health",
    "/api/ready",
    // App-to-app, shared-secret authenticated. Other fleet apps arrive on loopback and are skipped by
    // `is_internal`; these arms cover any caller that does not.
    "/api/internal/",
    "/api/v1/internal/",
    // Inbound webhook receivers.
    "/api/webhook",
    "/api/v1/webhooks",
    "/api/messages/webhook",
    "/api/external/",
    "/api/telnyx/webhook",
    "/api/telnyx/sms-webhook",
    "/api/google-calendar/webhook",
    "/api/events/ingest/",
    "/inbound",
];

/// Does the general API limiter cover this path? The API surface is `/api/…` plus the two public
/// redirect/portal prefixes; everything else (SPA shells, static assets, /favicon.ico) passes through
/// untouched, and the machine receivers are carved out by `EXEMPT_PREFIXES`.
fn is_rate_limited_surface(path: &str) -> bool {
    let on_api_surface = path.starts_with("/api/")
        || path == "/api"
        || path.starts_with("/s/")
        || path.starts_with("/track/")
        || path.starts_with("/inbound");
    on_api_surface
        && !EXEMPT_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
}

/// Is the caller presenting a credential? Used ONLY to pick the console bucket — the bucket is keyed
/// by IP, so this is a tier selector, not an authorization decision. Anything shaped like a bearer
/// credential counts; a forged one is bounded by the same per-IP ceiling.
fn has_bearer_credential(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|token| !token.trim().is_empty())
        .unwrap_or(false)
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
