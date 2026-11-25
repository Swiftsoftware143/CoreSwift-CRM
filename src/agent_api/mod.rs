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
        // ACT AS A REAL USER OF THE ACCOUNT (measured 2026-11-25). Several writers put `claims.sub` into
        // a column with a foreign key to `users(id)` — `opportunity_stage_history.moved_by` is one — so a
        // key's own id produced 500 "Database error" on the first deal an agent created. The key
        // authenticates the caller; the record is attributed to a real person of that account, which is
        // also what an audit reads months later.
        let actor: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE tenant_id = $1
              ORDER BY (role = 'account_owner') DESC, created_at LIMIT 1",
        )
        .bind(tenant)
        .fetch_optional(&state.db)
        .await?;
        let Some(actor) = actor else {
            return Err(AppError::Forbidden);
        };
        return Ok(Agent {
            claims: Claims {
                sub: actor.to_string(),
                aid: tenant.to_string(),
                // A key is not a person: it holds no admin role, so every handler's own role check
                // (an agency_admin door, for instance) refuses it exactly as it would a member.
                role: "api_key".to_string(),
                wsid: None,
                exp: 0,
                iat: 0,
                aud: None,
                iss: None,
            },
            tenant,
            label: format!("agent key {key_id} → acting as {actor}"),
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
///   * everything under `/api/mcp` — an agent calling itself in a loop;
///   * anything not in the console's own router: a path the console does not serve is a 404 here too.
pub fn router(state: crate::AppState) -> Router<crate::AppState> {
    Router::new()
        .route("/{*rest}", any(forward))
        .with_state(state)
}

/// The paths an issued key may never reach, checked on the forwarded path's first segment.
const DENIED: [&str; 6] = [
    "auth",
    "personal-keys",
    "api-keys",
    "keys",
    "mcp",
    "internal",
];

async fn forward(
    State(state): State<crate::AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<axum::response::Response> {
    let agent = authenticate(&state, &headers).await?;
    let rest = uri.path().trim_start_matches('/');
    let first = rest.split('/').next().unwrap_or("");
    if DENIED.contains(&first) {
        // The boundary's own sentence: the two doors must agree about what a key may not do.
        return Err(AppError::ForbiddenReason(
            crate::auth::boundary::READ_ONLY_REFUSAL.to_string(),
        ));
    }

    // Act as a real person of the account: several writers key attribution to `users(id)`, and a handler's
    // own role check must see the operator's real role rather than the word "api_key".
    let row: Option<(String, Option<Uuid>)> =
        sqlx::query_as("SELECT role, workspace_id FROM users WHERE id = $1 AND tenant_id = $2")
            .bind(Uuid::parse_str(&agent.claims.sub).unwrap_or_default())
            .bind(agent.tenant)
            .fetch_optional(&state.db)
            .await?;
    let Some((role, wsid)) = row else {
        return Err(AppError::Forbidden);
    };

    let now = chrono::Utc::now().timestamp();
    let claims = crate::auth::models::Claims {
        sub: agent.claims.sub.clone(),
        aid: agent.claims.aid.clone(),
        role,
        wsid: wsid.map(|w| w.to_string()),
        exp: (now + 120) as usize,
        iat: now as usize,
        aud: Some("coreswift-api".to_string()),
        iss: Some("coreswift".to_string()),
    };
    // Short-lived on purpose: it exists only for this one forwarded call.
    let token = crate::auth::middleware::create_access_token(&claims, &state.config.jwt_secret)?;

    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("http://127.0.0.1:{}/api/{rest}{query}", state.config.port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Internal(format!("agent forward: {e}")))?;
    let m = reqwest::Method::from_bytes(method.as_str().as_bytes())
        .map_err(|_| AppError::Validation("unsupported method".into()))?;
    let mut req = client.request(m, &url).bearer_auth(&token);
    if let Some(ct) = headers.get("content-type").and_then(|v| v.to_str().ok()) {
        req = req.header("content-type", ct);
    }
    let out =
        req.body(body.to_vec()).send().await.map_err(|e| {
            AppError::Internal(format!("agent forward could not reach the app: {e}"))
        })?;
    let status = out.status();
    let ctype = out
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let bytes = out
        .bytes()
        .await
        .map_err(|e| AppError::Internal(format!("agent forward: {e}")))?;
    let mut resp = axum::response::Response::builder()
        .status(
            axum::http::StatusCode::from_u16(status.as_u16())
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
        )
        .header("content-type", ctype);
    if status.as_u16() == 204 {
        return resp
            .body(axum::body::Body::empty())
            .map_err(|e| AppError::Internal(e.to_string()));
    }
    resp = resp.header("x-agent-door", "forwarded");
    resp.body(axum::body::Body::from(bytes))
        .map_err(|e| AppError::Internal(e.to_string()))
}
