//! The one credential boundary every guarded path passes through (kanban t_d8d782f2).
//!
//! CoreSwift-CRM had no global gate: 40 module routers mounted `auth::middleware::auth_middleware`
//! themselves and the rest relied on a handler-local check, and that middleware carried a blanket
//! `/internal/` bypass. [`require_credential`] is mounted once, outside the routing layers, so every
//! request to a path [`crate::auth::route_policy::is_guarded_path`] covers is decided here first.
//!
//! It answers exactly one question — *does this caller present a credential?* — and it can only
//! refuse an anonymous caller, plus ONE property of the credential itself: a `read_only` issued key
//! (kanban t_ce51cf34) is refused on every method/path that is not one of the read arms in
//! [`route_policy::READ_ONLY_KEY_ROUTES`], so the REST surface can never be wider than the MCP door
//! that role was minted for. Authorization (which tenant, which row) stays where it already was: the
//! module's own `auth_middleware` and the handler.
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

/// The one user-visible sentence for "this credential may not write" (kanban t_ce51cf34). 403, not
/// 401: the key is real and valid — it is the ROLE that refuses, and the caller is told which key to
/// use instead.
pub const READ_ONLY_REFUSAL: &str = "this key is read-only — use a workspace key to write";

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
    // A door that resolves the credential ITSELF answers its own refusal in the protocol's shape
    // (`/api/mcp`: JSON-RPC -32001). Refusing it here first would hand an MCP client this
    // middleware's generic body, which it cannot parse — and the door is not weaker for it: every
    // call still runs the full resolution, and no call is served without a resolved caller.
    if route_policy::is_self_authenticating(&path) {
        return next.run(req).await;
    }

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
        if !presents_opaque_bearer(&req) {
            return reject(StatusCode::UNAUTHORIZED, BOUNDARY_REFUSAL);
        }
        // A `read_only` key is read-only on BOTH doors (kanban t_ce51cf34). The MCP door gates its
        // own tools; this is the REST half, decided ONCE here, before any handler runs, from the
        // committed allowlist. A console JWT (or any non-`csk_` bearer) has no role here and is
        // untouched. The bearer is copied out first: `Request`'s body is not `Sync`, so holding
        // `&Request` across the lookup's await would make this middleware's future non-`Send`.
        let issued = bearer(&req)
            .filter(|t| t.starts_with("csk_"))
            .map(str::to_owned);
        if let Some(token) = issued {
            if let Some(role) = crate::external_api::issued_key_role(&state, &token).await {
                if role == crate::personal_api_keys::ROLE_READ_ONLY
                    && !route_policy::is_read_only_allowed(req.method().as_str(), &path)
                {
                    return reject(StatusCode::FORBIDDEN, READ_ONLY_REFUSAL);
                }
            }
        }
        return next.run(req).await;
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
