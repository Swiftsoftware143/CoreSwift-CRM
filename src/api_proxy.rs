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

/// Calls the forwarding doors may not carry, as (method, path-pattern) pairs. `*` matches exactly one
/// path segment.
///
/// A SEND cannot be undone: once mail leaves, every recipient has it. So the arms that hand a message to
/// the provider — or start an automation that does — are refused here, at the one place both doors pass
/// through, rather than trusted to a prompt. A model told "do not send" is a wish; a refusal at the door
/// is a control. Drafting, editing and scheduling are all still allowed: the work is kept, only the
/// irreversible click is not the agent's.
pub const DENIED_CALLS: &[(&str, &str)] = &[
    // Start or resume a campaign's sending loop.
    ("POST", "campaigns/*/activate"),
    // Put a person into a campaign (the enroll arm sends the next step).
    ("POST", "campaigns/*/enroll"),
    // Attach a trigger that can enroll people on its own.
    ("POST", "campaigns/*/triggers"),
    // Hand a message straight to the transport.
    ("POST", "comms/messages"),
];

/// One user-visible sentence, so both doors say the same thing about the same refusal.
pub const SEND_REFUSAL: &str =
    "sending is not an agent's call: draft it, and a person presses send";

/// Is this call a DELETE, in the sense a key that may not delete must be refused on?
///
/// Two shapes, one decision, so no door can be more generous than another:
///   * the DELETE method itself — every removing arm in this app is a `delete` route;
///   * a non-read call whose own PATH names a delete (a bulk-delete POST, for example), which is a
///     delete that happens to be spelled POST. Reads are never deletes, so a `?q=delete` search
///     cannot be over-refused by this rule.
pub fn delete_refused(method: &str, path: &str) -> bool {
    let m = method.to_uppercase();
    if m == "DELETE" {
        return true;
    }
    if m == "GET" || m == "HEAD" {
        return false;
    }
    let clean = path.split('?').next().unwrap_or("").trim_start_matches('/');
    clean
        .split('/')
        .any(|seg| seg.to_ascii_lowercase().contains("delete"))
}

/// Does (method, path) match a one-segment `*` pattern?
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let t: Vec<&str> = pattern.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return false;
    }
    t.iter()
        .zip(p.iter())
        .all(|(tseg, pseg)| *tseg == "*" || tseg == pseg)
}

/// May this method + path be forwarded at all? (The credential surfaces are refused by [`allowed`]; this
/// answers the second question — is it a send?)
pub fn send_allowed(method: &str, path: &str) -> bool {
    let clean = path.trim_start_matches('/');
    let m = method.to_uppercase();
    !DENIED_CALLS
        .iter()
        .any(|(pm, pp)| *pm == m && pattern_matches(pp, clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_surfaces_are_refused_and_ordinary_paths_pass() {
        for p in [
            "auth/login",
            "personal-keys",
            "api-keys/1/reveal",
            "keys",
            "mcp",
            "internal/x",
            "agent/contacts",
        ] {
            assert!(
                !allowed(p),
                "{p} must not be reachable through a forwarded door"
            );
        }
        for p in [
            "contacts",
            "comms/templates",
            "campaigns/9/steps",
            "booking/calendars",
        ] {
            assert!(allowed(p), "{p} is ordinary console work");
        }
    }

    /// A key that may not delete is refused on the DELETE method and on a non-read arm whose own
    /// path names a delete — and nothing else, so ordinary writes keep working.
    #[test]
    fn a_no_delete_credential_is_refused_only_on_deletes() {
        for (m, p) in [
            ("DELETE", "contacts/9b1c7d2e"),
            ("delete", "/api/objects/orders/records/9b1c7d2e"),
            ("POST", "contacts/bulk-delete"),
            ("PATCH", "objects/orders/fields/bulk-delete"),
        ] {
            assert!(delete_refused(m, p), "{m} {p} is a delete");
        }
        for (m, p) in [
            ("GET", "contacts"),
            ("GET", "contacts?q=delete"),
            ("HEAD", "contacts?q=delete"),
            ("POST", "contacts"),
            ("PATCH", "contacts/9b1c7d2e"),
            ("PUT", "redirects"),
            ("POST", "booking/orders/42/cancel"),
            ("POST", "campaigns/abc/steps"),
        ] {
            assert!(!delete_refused(m, p), "{m} {p} is not a delete");
        }
    }

    #[test]
    fn the_send_arms_are_the_only_calls_refused_by_method_and_path() {
        // Refused: the four irreversible arms, on the method that performs them.
        for (m, p) in [
            ("POST", "campaigns/abc/activate"),
            ("post", "campaigns/abc/enroll"),
            ("POST", "campaigns/abc/triggers"),
            ("POST", "comms/messages"),
        ] {
            assert!(!send_allowed(m, p), "{m} {p} must be refused");
        }
        // Allowed: everything that keeps the work without sending it.
        for (m, p) in [
            ("PATCH", "campaigns/abc"),
            ("POST", "campaigns"),           // create a draft
            ("POST", "campaigns/abc/steps"), // add a step to a draft
            ("PATCH", "campaigns/abc/1"),    // edit a step
            ("POST", "comms/templates"),     // write a template
            ("POST", "comms/messages/42"),   // not a route; also not the send arm
            ("GET", "campaigns/abc/activate"),
        ] {
            assert!(send_allowed(m, p), "{m} {p} must still be allowed");
        }
    }
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
/// `delete_allowed` is the CALLER's own capability, and it is a required argument on purpose: a
/// forwarding door has to say which credential it is acting for, so "may not delete" cannot be
/// forgotten and default to yes (kanban t_30fc7d62).
/// Returns the status code and the body, exactly as the app answered.
pub async fn call(
    state: &AppState,
    tenant: Uuid,
    method: &str,
    path: &str,
    body: Option<Vec<u8>>,
    delete_allowed: bool,
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
    // A send is refused HERE, for a key and for an agent, on both forwarding doors. It is not the
    // prompt's job to remember: the door is where "irreversible" can be enforced.
    if !send_allowed(method, clean) {
        return Err(AppError::ForbiddenReason(SEND_REFUSAL.into()));
    }
    // A delete is refused HERE, for a credential that may not delete, before anything is
    // forwarded — the same place both forwarding doors pass through. A model told "do not delete"
    // is a wish; a refusal at the door is a control.
    if !delete_allowed && delete_refused(method, &clean) {
        return Err(AppError::ForbiddenReason(
            crate::personal_api_keys::NO_DELETE_REFUSAL.into(),
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
