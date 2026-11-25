//! The agent door: one credential-agnostic write surface for an AI agent working inside an account.
//!
//! David, 2026-11-25: "are they all read and write?" — they were not. The MCP could read 150 entities and
//! change four payment settings, and the reason it could not do more was measured, not assumed: the
//! console routes refuse an issued API key with `this route needs a console session — an API key cannot be
//! used here`. That refusal is DELIBERATE (auth/boundary.rs, plan §6.2/§8): a key must never reach a session
//! door such as the key-reveal route.
//!
//! David then asked the bigger question — "why can only 13 right? I want to be able to run the whole
//! business through CoreSwift using my agents" — and he is right that thirteen hand-picked writes are not
//! a business. Wrapping the console's handlers one at a time cannot reach that either: two hundred wrappers
//! means two hundred places to drift from the handler they wrap.
//!
//! So this module resolves EITHER credential itself (`Bearer <csk_…>` or a console session) to exactly one
//! account, and then FORWARDS: `/api/agent/<any console path>` re-issues the same request against the app's
//! own `/api/<path>` with a two-minute session token minted in-process. Every handler therefore keeps its
//! own tenancy scope, its own validation and its own audit, because it is literally the handler the console
//! calls. A key still cannot reach `auth/`, the key-reveal and personal-key surfaces, or `/api/mcp`.
//!
//! Money still arrives as a provider fact: the payment webhook is public and key-checked, so no agent tool
//! marks a booking paid.

use axum::body::Bytes;
use axum::http::{Method, Uri};
use axum::{extract::State, http::HeaderMap, routing::any, Router};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};

/// Who is calling, and may they write?
///
/// A `read_only` issued key is refused here, in the same words the REST boundary uses, so the two doors
/// agree about what that role means.
pub struct Agent {
    pub claims: Claims,
    pub tenant: Uuid,
    pub label: String,
}

/// Resolve either credential to one account.
async fn authenticate(state: &crate::AppState, headers: &HeaderMap) -> Result<Agent, AppError> {
    let bearer = headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(AppError::Unauthorized)?;

    if bearer.starts_with("csk_") {
        // The SAME resolver the rest of the issued-key surface uses (`/api/external/**`): it decides
        // whether the key is live, whether its OWNER may still act (an issued key whose user was
        // deactivated or closed is not a credential), stamps `last_used_at` and meters the call
        // against the account's daily ceiling. A second hand-rolled key lookup here would be a
        // second rule that can disagree with the first.
        let (tenant, key_id) = crate::external_api::resolve_key(state, headers).await?;
        let role = crate::external_api::issued_key_role(state, bearer)
            .await
            .unwrap_or_default();
        if role != crate::personal_api_keys::ROLE_WORKSPACE {
            // The boundary's own sentence, so both doors say the same thing about the same role.
            return Err(AppError::ForbiddenReason(
                crate::auth::boundary::READ_ONLY_REFUSAL.to_string(),
            ));
        }
        let key_id = key_id.to_string();
        // The key authenticates the caller; WHO the forwarded call is attributed to is decided in exactly
        // one place (`api_proxy::acting_user`, shared with the app's MCP door), which picks a real, active
        // user of this account. A second copy of that rule here would be a second answer to "who did it".
        return Ok(Agent {
            claims: Claims {
                sub: key_id.clone(),
                aid: tenant.to_string(),
                // A key is not a person and holds no role: the forwarded call carries the acting user's
                // own role, decided by `api_proxy`.
                role: "api_key".to_string(),
                wsid: None,
                exp: 0,
                iat: 0,
                aud: None,
                iss: None,
            },
            tenant,
            label: format!("agent key {key_id}"),
        });
    }

    // Otherwise it is a console session, verified exactly as every other module does.
    let claims = crate::auth::middleware::verify_token(bearer, &state.config.jwt_secret)?;
    let tenant = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Agent {
        label: claims.sub.clone(),
        claims,
        tenant,
    })
}

/// EVERY console path, reachable with a key — the whole business, not thirteen hand-picked writes.
///
/// David, 2026-11-25: "why can only 13 right? I want to be able to run the whole business through
/// CoreSwift using my agents." The thirteen were what the booking/payment loop needed, each proved one at
/// a time. That approach cannot reach "the whole business": wrapping two hundred handlers by hand means
/// two hundred chances to drift from the handler it wraps.
///
/// So the door forwards instead of re-implementing. `/api/agent/<any console path>` authenticates the
/// caller (key or session), resolves who they act as, and re-issues the SAME request against the app's own
/// `/api/<path>` with a two-minute session token minted in-process. Every handler keeps its own tenancy
/// scope, its own validation, its own audit — because it is literally the same handler the console calls.
///
/// Three things stay shut, deliberately:
///   * the surfaces that hand out the account's own credentials (`auth/`, key reveal/rotate, personal
///     keys) — an issued key must not be able to widen itself into a session credential;
///   * everything under `/api/mcp` — an agent calling itself in a loop — and `/api/agent` itself, so a
///     forwarded path can never re-enter this door;
///   * **DELETE**, refused here before anything is forwarded: an agent may run the business, but a
///     delete is the one action nobody can undo, and no agent should hold it. Nothing in this app marks
///     a booking paid either — money arrives as the provider's own signed fact, never an agent's word.
/// Anything the console's own router does not serve answers 404 through the same call.
pub fn router(state: crate::AppState) -> Router<crate::AppState> {
    Router::new()
        .route("/*rest", any(forward))
        .with_state(state)
}

/// The sentence a caller gets when it asks the door to delete something.
const DELETE_REFUSAL: &str =
    "an agent may not delete: this door forwards reads and writes, never a delete";

async fn forward(
    State(state): State<crate::AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    if method == Method::DELETE {
        return Err(AppError::ForbiddenReason(DELETE_REFUSAL.to_string()));
    }
    let rest = uri.path().trim_start_matches('/').to_string();
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    // ONE implementation of "act as this account, as this person": the app's own MCP door uses it too
    // (`crate::api_proxy`). A second copy here would be a second rule about who a call is attributed to.
    let (status, text) = crate::api_proxy::call(
        &state,
        agent.tenant,
        method.as_str(),
        &format!("{rest}{query}"),
        Some(body.to_vec()),
    )
    .await?;
    let code = axum::http::StatusCode::from_u16(status)
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    axum::response::Response::builder()
        .status(code)
        .header("content-type", "application/json")
        .header("x-agent-door", "forwarded")
        .body(axum::body::Body::from(text))
        .map_err(|e| AppError::Internal(e.to_string()))
} // The denied list now lives in `crate::api_proxy::DENIED_PREFIXES`, beside the one function that applies
  // it: the agent door and the app's own MCP tool must refuse the same surfaces, so they read one list.
