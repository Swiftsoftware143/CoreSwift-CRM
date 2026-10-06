//! The one credential boundary every guarded path passes through (kanban t_d8d782f2).
//!
//! CoreSwift-CRM had no global gate: 40 module routers mounted `auth::middleware::auth_middleware`
//! themselves and the rest relied on a handler-local check, and that middleware carried a blanket
//! `/internal/` bypass. [`require_credential`] is mounted once, outside the routing layers, so every
//! request to a path [`crate::auth::route_policy::is_guarded_path`] covers is decided here first.
//!
//! It answers exactly one question — *does this caller present a credential?* — and it can only
//! refuse an anonymous caller. Authorization (which tenant, which role, which row) stays where it
//! already was: the module's own `auth_middleware` and the handler.
//!
//! The refusal is deliberately distinguishable from every handler's own: the body is
//! `{"error":"Authentication required"}` with no `code` field, which is what makes the boundary
//! provable live (a curl that gets the boundary's body never reached the handler).

use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

use crate::auth::route_policy;
use crate::AppState;

/// The one user-visible string for "no credential at all", distinct from `AppError::Unauthorized`'s
/// `{"code":401,"error":true,"message":"Authentication required"}` so a live curl can tell which
/// gate refused it.
pub const BOUNDARY_REFUSAL: &str = "Authentication required";

/// Length-independent constant-time compare, same shape as the handler-local helpers in
/// `contacts_internal` / `tags::internal_handler` / `bookings::handlers`; this copy is the one the
/// boundary uses.
fn ct_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut diff = a.len() ^ b.len();
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// Arm for [`route_policy::INTERNAL_ROUTES`]: the app's own shared secret in `x-internal-key`. An
/// app with NO key configured never authorises anything (an empty configured key must not match an
/// empty header).
fn presents_internal_key(state: &AppState, req: &Request) -> bool {
    if state.config.internal_sync_key.is_empty() {
        return false;
    }
    match req
        .headers()
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
    {
        Some(key) => ct_eq(key, &state.config.internal_sync_key),
        None => false,
    }
}

/// Arm for [`route_policy::API_KEY_ROUTES`]: an issued personal API key, presented as an opaque
/// bearer. Its validation is a database read (`external_api::resolve_key` against
/// `personal_api_keys`) that the handler already performs, so the boundary only requires that a
/// non-empty bearer be present.
fn presents_opaque_bearer(req: &Request) -> bool {
    bearer(req).map(|t| !t.is_empty()).unwrap_or(false)
}

/// Arm for everything else: a genuinely valid app JWT — HS256 over `JWT_SECRET`, `iss=coreswift`,
/// `aud=coreswift-api`, unexpired. The SAME `auth::middleware::verify_token` the per-module gate and
/// the handlers use, so a token that passes here cannot fail the handler's own check, and a garbage
/// bearer is refused here rather than being handed to a route that has no check of its own.
fn presents_app_jwt(state: &AppState, req: &Request) -> bool {
    match bearer(req) {
        Some(token) => {
            crate::auth::middleware::verify_token(token, &state.config.jwt_secret).is_ok()
        }
        None => false,
    }
}

fn bearer(req: &Request) -> Option<&str> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn reject(status: StatusCode, error: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "status": status.as_u16() })),
    )
        .into_response()
}

/// Global fail-closed auth. Runs outside the routing layers, so it sees every request before any
/// module's own middleware does.
pub async fn require_credential(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();

    // Served surfaces (the SPA, the tracked-link redirect, the support portal) carry no credential
    // and return no tenant data of their own — see `route_policy::is_guarded_path`.
    if !route_policy::is_guarded_path(&path) || route_policy::is_public_route(&path) {
        return next.run(req).await;
    }

    // ── the internal surface ────────────────────────────────────────────────────────────────────
    // Named in `route_policy::INTERNAL_ROUTES`. Before this the only thing standing between an
    // anonymous caller and these handlers was `auth_middleware`'s blanket `/internal/` bypass plus
    // each handler remembering its own key check (6 of the 14 deserialize the body before checking,
    // so an anonymous post reaches the handler). The key is now demanded HERE as well, so a route
    // added under one of these prefixes without an entry above is an ordinary private route.
    if route_policy::is_internal_route(&path) {
        return if presents_internal_key(&state, &req) {
            next.run(req).await
        } else {
            reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
        };
    }

    // ── the issued-API-key surface ──────────────────────────────────────────────────────────────
    if route_policy::is_api_key_route(&path) {
        return if presents_opaque_bearer(&req) {
            next.run(req).await
        } else {
            reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
        };
    }

    // ── everything else: a valid app JWT ────────────────────────────────────────────────────────
    if presents_app_jwt(&state, &req) {
        next.run(req).await
    } else {
        reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_internal_key_compare_is_exact() {
        assert!(ct_eq("s3cret", "s3cret"));
        assert!(!ct_eq("s3cret", "s3cre"));
        assert!(!ct_eq("s3cre", "s3cret"));
        assert!(!ct_eq("s3cret", "s3cres"));
        assert!(!ct_eq("", "s3cret"));
        // A prefix of the real key must not pass — the compare is on the whole value.
        assert!(!ct_eq("s3cre", "s3cret"));
        // An empty configured key is refused before the compare (see presents_internal_key).
        assert!(ct_eq("", ""));
    }

    /// The boundary reads the committed allowlist, not a local array: this pins the delegation so
    /// the two cannot drift, and that the shapes the old blanket bypass covered are now NAME-gated.
    #[test]
    fn the_boundary_reads_the_committed_allowlist() {
        assert!(route_policy::is_public_route("/api/health"));
        assert!(!route_policy::is_public_route("/api/contacts"));
        assert!(route_policy::is_internal_route("/api/internal/tags/list"));
        assert!(route_policy::is_internal_route(
            "/api/v1/internal/tag-provision"
        ));
        // ...and the blanket `/internal/` prefix bypass is gone: an unnamed sibling is NOT internal
        // and NOT public, so it is an ordinary private route.
        assert!(!route_policy::is_internal_route("/api/internal/whatever"));
        assert!(!route_policy::is_public_route("/api/internal/whatever"));
        assert!(!route_policy::is_internal_route(
            "/api/v1/internal/whatever"
        ));
        // the API-key surface is not anonymous either
        assert!(route_policy::is_api_key_route("/api/external/contacts"));
        assert!(!route_policy::is_api_key_route("/api/external/anything"));
        assert!(!route_policy::is_public_route("/api/external/contacts"));
    }
}
