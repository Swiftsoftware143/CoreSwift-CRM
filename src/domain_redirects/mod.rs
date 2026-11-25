//! Where a visitor is sent when they open one of this account's addresses.
//!
//! David, 2026-11-25: "can you set it up so the user can put a redirect for the domain or subdomain?
//! That will be closing up a loop. And that feature should be found anywhere that users put their custom
//! domain, even sender domain."
//!
//! So the list an owner sees is THEIR OWN addresses, gathered from every surface that claims a name:
//! the account booking address, each workspace booking address, each calendar's own address, and each
//! sending domain. One row per hostname in `domain_redirects` decides where it sends people, and the
//! nginx applier turns those rows into server blocks — the app never answers the redirect itself, so a
//! redirect works even when the app is restarting.

use axum::{
    extract::{Path, State},
    routing::{delete, get, put},
    Extension, Json, Router,
};
use serde_json::{json, Value};

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};

/// The module's handlers, reachable by the agent door (which resolves its own credential and must call
/// the SAME implementation rather than a second copy).
pub mod handlers {
    pub use super::save_redirect;
}

pub fn router(state: crate::AppState) -> Router<crate::AppState> {
    // Take AppState and pull the pool out, exactly like every other router here: a `Router<Pool<..>>`
    // nests as the wrong service and answers 404 at run time while every gate stays green.
    Router::new()
        .route("/", get(list_addresses))
        .route("/", put(save_redirect))
        .route("/:host", delete(remove_redirect))
        // THE CLAIMS LAYER IS OURS TO ADD. The global boundary in main.rs refuses an anonymous caller but
        // deliberately does not attach `Claims`; without this layer every handler here answers 500
        // "Missing request extension: Claims" — measured on the first live call, not assumed.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// A hostname this account can redirect: where it came from, and where it currently sends people.
#[derive(Debug, sqlx::FromRow)]
struct AddressRow {
    host: String,
    source: String,
    target_url: Option<String>,
    status_code: Option<i32>,
    is_active: Option<bool>,
}

/// The caller's account. The claim is a string; a token that cannot name an account reaches nothing.
fn tenant_of(claims: &Claims) -> uuid::Uuid {
    claims.aid.parse().unwrap_or_else(|_| uuid::Uuid::nil())
}

/// The platform's own domain, for building `<label>.<domain>` addresses.
fn app_domain() -> String {
    std::env::var("APP_DOMAIN").unwrap_or_else(|_| "coreswiftcrm.com".to_string())
}

/// Normalise what an owner typed as a host into the form this table stores.
fn clean_host(raw: &str) -> Option<String> {
    let t = raw.trim().to_lowercase();
    let t = t
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let t = t.split('/').next().unwrap_or("").to_string();
    if t.is_empty() || t.len() > 253 {
        return None;
    }
    let ok = t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    if !ok || !t.contains('.') {
        return None;
    }
    Some(t)
}

/// A redirect target is an http(s) address or a path on this site — nothing else.
///
/// The value becomes a browser navigation for whoever opens the owner's domain, so `javascript:` or
/// `data:` here would be a stored attack and `//host` would quietly hand the visitor to a third party.
fn clean_target(raw: &str) -> Result<String, AppError> {
    let t = raw.trim();
    if t.is_empty() {
        return Err(AppError::BadRequest(
            "a redirect needs a destination".into(),
        ));
    }
    if t.starts_with("//") {
        return Err(AppError::BadRequest(
            "a redirect cannot start with // — that sends visitors to a different site without saying so"
                .into(),
        ));
    }
    let lower = t.to_lowercase();
    if lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
    {
        return Err(AppError::BadRequest(
            "that destination is not a web address — use an https:// link or a path starting with /".into(),
        ));
    }
    if t.starts_with("https://") || t.starts_with("http://") {
        return Ok(t.to_string());
    }
    if t.starts_with('/') {
        return Ok(t.to_string());
    }
    Err(AppError::BadRequest(
        "that destination is not a web address — use an https:// link or a path starting with /"
            .into(),
    ))
}

/// Every address this account owns, with where it currently sends a visitor.
///
/// The four sources are gathered in one query so a new surface (a fifth kind of address) is a line here
/// rather than a second screen an owner has to find.
async fn list_addresses(
    State(state): State<crate::AppState>,
    Extension(user): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let rows: Vec<AddressRow> = sqlx::query_as(
        r#"
        WITH mine AS (
            -- the account's own booking address
            SELECT subdomain || '.' || $2 AS host, 'account booking address'::text AS source
              FROM calendar_hosts WHERE tenant_id = $1 AND is_active
            UNION ALL
            SELECT custom_domain, 'account booking domain'
              FROM calendar_hosts WHERE tenant_id = $1 AND is_active AND custom_domain IS NOT NULL
            UNION ALL
            -- a workspace's booking address
            SELECT wh.subdomain || '.' || $2, 'workspace booking address (' || coalesce(w.name, 'workspace') || ')'
              FROM workspace_hosts wh LEFT JOIN workspaces w ON w.id = wh.workspace_id
             WHERE wh.tenant_id = $1 AND wh.is_active
            UNION ALL
            SELECT wh.custom_domain, 'workspace domain (' || coalesce(w.name, 'workspace') || ')'
              FROM workspace_hosts wh LEFT JOIN workspaces w ON w.id = wh.workspace_id
             WHERE wh.tenant_id = $1 AND wh.is_active AND wh.custom_domain IS NOT NULL
            UNION ALL
            -- a calendar's own address
            SELECT cs.subdomain || '.' || $2, 'calendar address (' || coalesce(c.name, 'calendar') || ')'
              FROM calendar_subdomains cs JOIN booking_calendars c ON c.id = cs.calendar_id
             WHERE c.tenant_id = $1
            UNION ALL
            -- the domain a person SENDS from: it opens to nothing unless it is told where to go
            SELECT pd.domain, 'sending domain'
              FROM private_email_domains pd WHERE pd.tenant_id = $1
        )
        SELECT DISTINCT ON (lower(m.host))
               m.host, m.source, r.target_url, r.status_code, r.is_active
          FROM mine m
          LEFT JOIN domain_redirects r ON lower(r.host) = lower(m.host)
         WHERE m.host IS NOT NULL AND m.host <> ''
         ORDER BY lower(m.host), m.source
        "#,
    )
    .bind(tenant_of(&user))
    .bind(app_domain())
    .fetch_all(&state.db)
    .await?;

    let addresses: Vec<Value> = rows
        .into_iter()
        .map(|r| {
            json!({
                "host": r.host,
                "source": r.source,
                // A sending domain usually has no page at all — say so, because "nothing there" is the
                // reason an owner wants a redirect in the first place.
                "sends_visitors_to": r.target_url,
                "status_code": r.status_code,
                "is_active": r.is_active,
            })
        })
        .collect();

    let configured = addresses
        .iter()
        .filter(|a| {
            a.get("sends_visitors_to")
                .map(|v| !v.is_null())
                .unwrap_or(false)
        })
        .count();

    Ok(Json(json!({
        "addresses": addresses,
        "count": addresses.len(),
        "with_redirect": configured,
        "note": "A redirect is served by the web server, not by this app, so it keeps working while the app restarts. A change is picked up by the vhost sync within a minute.",
    })))
}

/// Save (or replace) where one of these addresses sends a visitor.
pub async fn save_redirect(
    State(state): State<crate::AppState>,
    Extension(user): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let host = body
        .get("host")
        .and_then(|v| v.as_str())
        .and_then(clean_host)
        .ok_or_else(|| AppError::BadRequest("that does not look like a hostname".into()))?;
    let target = clean_target(
        body.get("target_url")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    )?;
    let status = body
        .get("status_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(302);
    if ![301, 302, 307, 308].contains(&status) {
        return Err(AppError::BadRequest(
            "a redirect is 301, 302, 307 or 308".into(),
        ));
    }
    let active = body
        .get("is_active")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // THE ADDRESS MUST BE THEIRS. Without this an owner could point a stranger's domain at any site they
    // liked, using our origin to do it.
    let owns: Option<bool> = sqlx::query_scalar(
        r#"
        SELECT EXISTS (
            SELECT 1 FROM calendar_hosts
             WHERE tenant_id = $1 AND (lower(coalesce(custom_domain,'')) = lower($2)
                                       OR lower(subdomain || '.' || $3) = lower($2))
            UNION ALL
            SELECT 1 FROM workspace_hosts
             WHERE tenant_id = $1 AND (lower(coalesce(custom_domain,'')) = lower($2)
                                       OR lower(subdomain || '.' || $3) = lower($2))
            UNION ALL
            SELECT 1 FROM calendar_subdomains cs
              JOIN booking_calendars c ON c.id = cs.calendar_id
             WHERE c.tenant_id = $1 AND lower(cs.subdomain || '.' || $3) = lower($2)
            UNION ALL
            SELECT 1 FROM private_email_domains
             WHERE tenant_id = $1 AND lower(domain) = lower($2)
        )
        "#,
    )
    .bind(tenant_of(&user))
    .bind(&host)
    .bind(app_domain())
    .fetch_optional(&state.db)
    .await?
    .flatten();

    if owns != Some(true) {
        return Err(AppError::BadRequest(format!(
            "{host} is not one of this account's addresses"
        )));
    }

    // A host already claimed by another account is refused BY NAME rather than overwritten: a redirect
    // that silently changes hands is someone's live link breaking with no notice.
    let holder: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT r.tenant_id, coalesce(t.name, t.id::text) FROM domain_redirects r
           JOIN tenants t ON t.id = r.tenant_id
          WHERE lower(r.host) = lower($1) AND r.tenant_id <> $2 LIMIT 1",
    )
    .bind(&host)
    .bind(tenant_of(&user))
    .fetch_optional(&state.db)
    .await?;
    if let Some((_, name)) = holder {
        return Err(AppError::BadRequest(format!(
            "{host} is already redirected by another account ({name}) — remove it there first"
        )));
    }

    sqlx::query(
        r#"
        INSERT INTO domain_redirects (tenant_id, host, target_url, status_code, is_active)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (lower(host)) DO UPDATE
           SET target_url = EXCLUDED.target_url,
               status_code = EXCLUDED.status_code,
               is_active = EXCLUDED.is_active,
               updated_at = now()
         WHERE domain_redirects.tenant_id = EXCLUDED.tenant_id
        "#,
    )
    .bind(tenant_of(&user))
    .bind(&host)
    .bind(&target)
    .bind(status as i32)
    .bind(active)
    .execute(&state.db)
    .await?;

    // The vhost file is generated from these rows. The app is in a container and cannot run the host's
    // script, so the timer `coreswift-domain-redirects.timer` applies it within a minute (and the deploy
    // runs it too). `apply_hook` is still tried, for the case where the script happens to be reachable.
    crate::domain_redirects::apply_hook().await;

    Ok(Json(json!({
        "saved": true,
        "host": host,
        "target_url": target,
        "status_code": status,
    })))
}

/// Stop redirecting one address. The row goes; the vhost sync stops writing its block.
async fn remove_redirect(
    State(state): State<crate::AppState>,
    Extension(user): Extension<Claims>,
    Path(host): Path<String>,
) -> ApiResult<Json<Value>> {
    let host = clean_host(&host)
        .ok_or_else(|| AppError::BadRequest("that does not look like a hostname".into()))?;
    let gone = sqlx::query(
        "DELETE FROM domain_redirects WHERE tenant_id = $1 AND lower(host) = lower($2)",
    )
    .bind(tenant_of(&user))
    .bind(&host)
    .execute(&state.db)
    .await?
    .rows_affected();
    if gone == 0 {
        return Err(AppError::NotFound(format!("no redirect on {host}")));
    }
    crate::domain_redirects::apply_hook().await;
    Ok(Json(json!({"removed": true, "host": host})))
}

/// Regenerate the vhost file from the saved rows.
///
/// Best effort and never fatal: a redirect that saved but did not yet reach nginx is a delay, not a lost
/// configuration, and the applier is also run on deploy.
pub async fn apply_hook() {
    let script = "/opt/swift/bin/coreswift-domain-redirect-applier.sh";
    if !std::path::Path::new(script).exists() {
        tracing::info!("domain redirect applier not present; rows saved, vhost sync skipped");
        return;
    }
    match tokio::process::Command::new(script).output().await {
        Ok(out) if out.status.success() => {}
        Ok(out) => tracing::warn!(
            status = ?out.status,
            stderr = %String::from_utf8_lossy(&out.stderr),
            "domain redirect applier did not finish cleanly"
        ),
        Err(e) => tracing::warn!(error = %e, "domain redirect applier could not be run"),
    }
}
