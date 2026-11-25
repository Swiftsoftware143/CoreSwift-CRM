//! One place that turns "this account, acting as this person" into a call against the app's OWN API.
//!
//! Why this exists (David, 2026-11-25: *"read only is the fleet MCP but the write should be within the
//! software MCP"*). The write surface an agent uses must live inside CoreSwift, not in the fleet service,
//! so there is one product to reason about and one credential per account. But wrapping two hundred console
//! handlers one at a time is not a business either — every wrapper is a place to drift from the handler it
//! wraps.
//!
//! So both the agent door (`/api/agent/**`) and the app's own MCP door (`/api/mcp`, tool `coreswift_api`)
//! use THIS function: it resolves the person the call is attributed to, mints a two-minute session token
//! in-process, and re-issues the request against `/api/<path>`. Every handler therefore keeps its own
//! tenancy scope, its own validation and its own audit, because it is literally the handler the console
//! calls.
//!
//! Two rules it enforces so callers cannot get them wrong:
//!   * the path is relative to `/api` and may not escape it (`..`, a scheme, a host);
//!   * the person acted as is a real, active user of THAT account, never an invented identity.

use uuid::Uuid;

use crate::errors::AppError;
use crate::AppState;

/// Paths an issued key or an agent may never reach, whatever door it came through. These hand out the
/// account's own credentials, so reaching one would let a key widen itself into a session credential.
pub const DENIED_PREFIXES: [&str; 7] = [
    "auth",
    "personal-keys",
    "api-keys",
    "keys",
    "mcp",
    "internal",
    // A forwarded path must never re-enter the forwarding door itself.
    "agent",
];

/// Is this forwarded path allowed?
pub fn allowed(path: &str) -> bool {
    let first = path.trim_start_matches('/').split('/').next().unwrap_or("");
    !DENIED_PREFIXES.contains(&first)
}

/// Who a call made with a credential is attributed to: a real user of the account.
///
/// Several writers put `claims.sub` straight into a column with a foreign key to `users(id)`
/// (`opportunity_stage_history.moved_by` measured, 2026-11-25), so an invented identity is not a harmless
/// shortcut — it is a 500 on the first deal an agent creates.
pub async fn acting_user(
    state: &AppState,
    tenant: Uuid,
) -> Result<(Uuid, String, Option<Uuid>), AppError> {
    // `users` has NO workspace_id column (measured 2026-11-25: a first attempt selected it and every
    // forwarded call answered "internal error"). A user's workspace is chosen per request, not stored on
    // the row — which is why the login path also puts `wsid: None` in its claims.
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, role FROM users
          WHERE tenant_id = $1 AND (is_active IS NULL OR is_active = true)
          ORDER BY (role = 'account_owner') DESC, created_at LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    let (id, role) = row.ok_or(AppError::Forbidden)?;
    Ok((id, role, None))
}

/// Call this app's own API as `tenant`, acting as a real user of it.
///
/// `path` is relative to `/api` (e.g. `contacts` or `booking/calendars/<id>/slots/generate`).
/// Returns the status code and the body, exactly as the app answered.
pub async fn call(
    state: &AppState,
    tenant: Uuid,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
) -> Result<(u16, String), AppError> {
    let clean = path.trim_start_matches('/');
    if clean.contains("..") || clean.contains("://") || clean.starts_with('/') {
        return Err(AppError::Validation(
            "the path must be relative to /api (for example \"contacts\")".into(),
        ));
    }
    if !allowed(clean) {
        return Err(AppError::ForbiddenReason(
            "that surface hands out credentials, so an agent may not call it".into(),
        ));
    }

    let (user_id, role, wsid) = acting_user(state, tenant).await?;
    // NEVER carry the PLATFORM role through this door. `agency_admin` is the fleet operator's role and its
    // surfaces answer for every tenant at once, so a key minted for ONE account must not inherit it — that
    // would turn a workspace key into a cross-tenant read (`t_d8d782f2` tenancy rule: no tenant sees
    // another's data). The account's own owner is the ceiling: everything inside the account, nothing
    // outside it.
    let role = if role == "agency_admin" {
        "owner".to_string()
    } else {
        role
    };
    let now = chrono::Utc::now().timestamp();
    let claims = crate::auth::models::Claims {
        sub: user_id.to_string(),
        aid: tenant.to_string(),
        role,
        wsid: wsid.map(|w| w.to_string()),
        exp: (now + 120) as usize,
        iat: now as usize,
        aud: Some("coreswift-api".to_string()),
        iss: Some("coreswift".to_string()),
    };
    // Two minutes on purpose: the token exists for this one forwarded call and nothing else.
    let token = crate::auth::middleware::create_access_token(&claims, &state.config.jwt_secret)?;

    let url = format!("http://127.0.0.1:{}/api/{clean}", state.config.port);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Internal(format!("api proxy: {e}")))?;
    let m = reqwest::Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|_| AppError::Validation(format!("\"{method}\" is not an HTTP method")))?;
    let mut req = client
        .request(m, &url)
        .bearer_auth(&token)
        .header("content-type", "application/json");
    if let Some(b) = body {
        if !b.is_empty() {
            req = req.body(b);
        }
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("the app did not answer: {e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}
