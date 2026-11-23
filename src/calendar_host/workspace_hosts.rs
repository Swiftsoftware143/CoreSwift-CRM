//! An address PER WORKSPACE, not one per account (David, 2026-11-23).
//!
//! > *"each workspace might have multiple calendars so they need to be able to have their sub domain"*.
//!
//! `calendar_hosts` used to hold exactly one row per ACCOUNT, so an account selling one newsletter per
//! town had a single public address for all of them. These two doors let an owner give each workspace
//! its own address, and the public site then lists only that workspace's calendars — one newsletter's
//! address must never advertise another newsletter's slots.
//!
//! Nothing outside the app has to be provisioned per address: nginx already answers any
//! `<label>.coreswiftcrm.com` with the hosted calendar page, and the origin certificate carries
//! `DNS:*.coreswiftcrm.com`. An address therefore works, over https, the moment the row exists.
//!
//! Two rules kept from the account-level arm, because they are the ones that actually bite:
//!
//! * **The database owns uniqueness.** `UNIQUE (lower(subdomain))` (plus the per-workspace key) means a
//!   check-then-insert would race two owners into one address. A unique violation is answered as
//!   "somebody owns that name", with a suggested free one.
//! * **A change of address never breaks the old one silently.** The response carries the address that
//!   is now live, so the console can show the link the owner should hand out.

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::handlers::require_account_admin;
use super::{
    ensure_for_tenant, is_reserved, is_unique_violation, public_url, suggest_subdomain,
    system_domain, taken_message_with_suggestion, valid_subdomain,
};
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// `GET /api/calendar-host/workspaces` — every workspace with the address it answers on today.
pub async fn list_workspace_hosts(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);

    // The account-level address is created on demand, exactly as before, so the console shows one even
    // on an account that has never opened this screen.
    ensure_for_tenant(&s.db, tenant_id).await?;

    let rows: Vec<(
        Uuid,
        String,
        bool,
        Option<Uuid>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT w.id, w.name, w.is_default, wh.workspace_id, wh.subdomain, wh.custom_domain, \
                    wh.custom_domain_status \
               FROM workspaces w \
               LEFT JOIN workspace_hosts wh ON wh.workspace_id = w.id AND wh.is_active \
              WHERE w.tenant_id = $1 \
              ORDER BY w.is_default DESC, w.name",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    // How many calendars each address would actually offer, so the screen can say what the address
    // serves instead of describing an empty page.
    let counts: Vec<(Option<Uuid>, i64)> = sqlx::query_as(
        "SELECT workspace_id, count(*)::bigint FROM booking_calendars \
          WHERE tenant_id = $1 AND is_active \
          GROUP BY workspace_id",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    let account: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT subdomain, custom_domain, custom_domain_status FROM calendar_hosts \
          WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?;

    let workspaces: Vec<serde_json::Value> = rows
        .into_iter()
        .map(
            |(id, name, is_default, _host_ws, subdomain, custom, status)| {
                let calendars = counts
                    .iter()
                    .filter(|(wid, _)| *wid == Some(id))
                    .map(|(_, n)| *n)
                    .next()
                    .unwrap_or(0);
                // An address is only shown as live when one is actually set: a blank input must not look
                // like a working link.
                let url = subdomain
                    .as_deref()
                    .map(|sub| public_url(sub, None, &system))
                    .or_else(|| {
                        custom
                            .as_deref()
                            .filter(|_| status.as_deref() == Some("verified"))
                            .map(|d| format!("https://{d}"))
                    });
                json!({
                    "id": id,
                    "name": name,
                    "is_default": is_default,
                    "subdomain": subdomain,
                    "custom_domain": custom,
                    "custom_domain_status": status,
                    "url": url,
                    "calendars": calendars,
                })
            },
        )
        .collect();

    Ok(Json(json!({
        "system_domain": system,
        "account_address": account.map(|(sub, custom, status)| json!({
            "subdomain": sub,
            "url": public_url(&sub, None, &system),
            "custom_domain": custom,
            "custom_domain_status": status,
        })),
        "workspaces": workspaces,
    })))
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceAddressRequest {
    /// The address label, e.g. `palm-bay-pulse` for https://palm-bay-pulse.coreswiftcrm.com
    #[serde(default)]
    pub subdomain: Option<String>,
    /// The workspace's OWN domain, e.g. `palmbaypulse.com`. Saved as `pending` until a real request
    /// arrives on it (or the DNS check succeeds), never as `verified` on the owner's say-so.
    #[serde(default)]
    pub custom_domain: Option<String>,
}

/// A domain a customer may point here: lowercase hostname, no scheme and no path.
fn clean_domain(raw: &str) -> Option<String> {
    let d = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    if d.is_empty() || d.len() > 253 || d.contains('/') || d.contains(' ') || d.contains('@') {
        return None;
    }
    let labels: Vec<&str> = d.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty() || l.len() > 63) {
        return None;
    }
    let ok = d
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    ok.then_some(d)
}

/// `PATCH /api/calendar-host/workspaces/:id` — claim or change one workspace's address.
pub async fn set_workspace_host(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(workspace_id): Path<Uuid>,
    Json(body): Json<WorkspaceAddressRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);

    // The workspace must be this account's own. A path id is caller-supplied, so it is never trusted
    // to name a workspace in another account.
    let workspace: Option<String> =
        sqlx::query_scalar("SELECT name FROM workspaces WHERE id = $1 AND tenant_id = $2")
            .bind(workspace_id)
            .bind(tenant_id)
            .fetch_optional(&s.db)
            .await?;
    let workspace = workspace.ok_or_else(|| AppError::NotFound("Workspace not found".into()))?;

    // A caller may set either the platform label, the workspace's own domain, or both. Blank fields
    // mean "leave what is there", so the DNS panel can be used before an address exists.
    let asked = body
        .subdomain
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    let custom = match body
        .custom_domain
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(raw) => Some(clean_domain(raw).ok_or_else(|| {
            AppError::Validation(
                "That does not look like a domain — type it as palmbaypulse.com or news.palmbaypulse.com, with no https:// and no path.".into(),
            )
        })?),
        None => None,
    };
    if asked.is_empty() && custom.is_none() {
        return Err(AppError::Validation(
            "Type the address you want this workspace to answer on, or the domain that should point here."
                .into(),
        ));
    }
    let requested = if asked.is_empty() {
        // Only a custom domain was given: keep the platform address this workspace already has, and
        // mint one from its name if it has none, so the DNS target always exists.
        match sqlx::query_scalar::<_, String>(
            "SELECT subdomain FROM workspace_hosts WHERE tenant_id = $1 AND workspace_id = $2",
        )
        .bind(tenant_id)
        .bind(workspace_id)
        .fetch_optional(&s.db)
        .await?
        {
            Some(existing) => existing,
            None => {
                let base = super::sanitise_base(&workspace);
                suggest_subdomain(&s.db, &base).await.unwrap_or(base)
            }
        }
    } else {
        asked
    };
    if !valid_subdomain(&requested) {
        return Err(AppError::Validation(
            "An address uses 3 to 63 letters, numbers and hyphens, and starts and ends with a letter or number. Example: palm-bay-pulse".into(),
        ));
    }
    if is_reserved(&requested) {
        return Err(AppError::Validation(format!(
            "'{requested}' is reserved for the platform. Pick a different name."
        )));
    }

    // The two namespaces are served by ONE wildcard vhost, so a workspace cannot take a name a
    // calendar already answers on.
    let calendar_owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT cs.calendar_id FROM calendar_subdomains cs WHERE lower(cs.subdomain) = $1",
    )
    .bind(&requested)
    .fetch_optional(&s.db)
    .await?;
    if calendar_owner.is_some() {
        let suggestion: Option<String> = suggest_subdomain(&s.db, &requested)
            .await
            .map(|s| format!("{s}.{system}"));
        return Err(AppError::Conflict(taken_message_with_suggestion(
            &format!("{requested}.{system}"),
            suggestion.as_deref(),
        )));
    }

    // Upsert on the workspace key: the first claim inserts, a later change updates in place, and the
    // unique index on (lower(subdomain)) is what decides a collision — not a read-then-write race.
    // `custom_domain_status` is NOT NULL on this table: 'none' is the value that means "no domain
    // claimed", which is what a row that only wanted a platform address has. Sending NULL took the
    // first real claim down with a 500.
    let saved = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "INSERT INTO workspace_hosts \
             (tenant_id, workspace_id, subdomain, custom_domain, custom_domain_status) \
         VALUES ($1, $2, $3, $4, CASE WHEN $4::text IS NULL THEN 'none' ELSE 'pending' END) \
         ON CONFLICT (workspace_id) \
         DO UPDATE SET subdomain = EXCLUDED.subdomain, \
                       custom_domain = COALESCE(EXCLUDED.custom_domain, workspace_hosts.custom_domain), \
                       custom_domain_status = CASE \
                           WHEN EXCLUDED.custom_domain IS NULL THEN workspace_hosts.custom_domain_status \
                           WHEN workspace_hosts.custom_domain IS DISTINCT FROM EXCLUDED.custom_domain \
                               THEN 'pending' \
                           ELSE workspace_hosts.custom_domain_status END, \
                       is_active = true, updated_at = now() \
         RETURNING subdomain, custom_domain, custom_domain_status",
    )
    .bind(tenant_id)
    .bind(workspace_id)
    .bind(&requested)
    .bind(&custom)
    .fetch_one(&s.db)
    .await;

    let (subdomain, custom, status) = match saved {
        Ok(row) => row,
        Err(e) if is_unique_violation(&e) => {
            let suggestion: Option<String> = suggest_subdomain(&s.db, &requested)
                .await
                .map(|s| format!("{s}.{system}"));
            return Err(AppError::Conflict(taken_message_with_suggestion(
                &format!("{requested}.{system}"),
                suggestion.as_deref(),
            )));
        }
        Err(e) => return Err(AppError::from(e)),
    };

    Ok(Json(json!({
        "workspace": workspace,
        "workspace_id": workspace_id,
        "subdomain": subdomain,
        "url": public_url(&subdomain, None, &system),
        "custom_domain": custom,
        "custom_domain_status": status,
        "message": format!("{workspace} now answers on {}.", public_url(&subdomain, None, &system)),
    })))
}
