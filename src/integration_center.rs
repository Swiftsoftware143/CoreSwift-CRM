//! Hub Integration Center — CoreSwift is the CRM hub and the single home for every lead.
//!
//! The sisters (ADASwift, FunnelSwift, IncentiveSwift, WorkflowSwift, MissedCall Responder,
//! Multi-Directory, FunnelSwift-Mobile) are **capture** apps: data flows DOWNWARD into
//! CoreSwift. This module is the hub-side surface that answers both directions of the
//! relationship, from live data — never from a hardcoded array in a SPA:
//!
//!   * `native_integrations` — the hub's first-party connectors, read from the
//!     `available_providers` catalogue and joined to this tenant's `app_connections`
//!     row for live status (connected / last test result / last test time).
//!   * `lead_sources`       — which capture apps are actually feeding THIS tenant's CRM,
//!     computed from the contacts table (`metadata->>'source_app'`, falling back to
//!     `source`), so "leads by capture app" is a real query, not a guess.
//!   * `personal_keys`      — the tenant's `csk_…` keys (prefix only, never the secret).
//!   * `cross_sell`         — one CTA per sister app (reverse cross-sell: a CoreSwift
//!     account that owns the CRM is the natural buyer of the capture apps).
//!
//! Auth: every route here is behind `auth_middleware` and scoped to the caller's tenant
//! (`Claims.aid`). No route accepts a tenant id from the body.

use axum::{extract::State, middleware, response::IntoResponse, Extension, Json, Router};
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// A capture app that pushes leads INTO CoreSwift — the canonical source-attribution
/// vocabulary. The slugs below are what /api/external/contacts records as
/// `source_app`; every spoke must send one of them (aliases are normalized in
/// `normalize_source_app`). `url`/`cta` drive the hub's reverse cross-sell.
pub struct LeadSource {
    pub key: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub icon: &'static str,
    pub url: &'static str,
    pub cta: &'static str,
}

/// The 7 capture apps. Order is the render order (CoreSwift's own marketing site first
/// is NOT included here — these are the sources that feed the CRM).
pub const LEAD_SOURCES: &[LeadSource] = &[
    LeadSource {
        key: "funnelswift",
        name: "FunnelSwift",
        description: "Kinetic card + sales-funnel leads pushed straight into this CRM.",
        icon: "filter",
        url: "https://funnelswift.net",
        cta: "Capture leads with a Kinetic card funnel",
    },
    LeadSource {
        key: "incentiveswift",
        name: "IncentiveSwift",
        description: "Loyalty campaigns and giveaway entries — every participant is a lead.",
        icon: "gift",
        url: "https://incentiveswift.com",
        cta: "Run a giveaway and collect entries",
    },
    LeadSource {
        key: "adaswift",
        name: "ADASwift",
        description: "ADA scan-report requests and proposal downloads.",
        icon: "briefcase",
        url: "https://adaswift.com",
        cta: "Add an ADA scan report to your site",
    },
    LeadSource {
        key: "workflowswift",
        name: "WorkflowSwift",
        description: "n8n workflow automation that captures and routes leads.",
        icon: "workflow",
        url: "https://workflowswift.com",
        cta: "Automate your lead capture",
    },
    LeadSource {
        key: "missedcallrespondr",
        name: "MissedCall Responder",
        description: "Missed-call text-back — every recovered caller becomes a contact.",
        icon: "phone",
        url: "https://missedcallrespondr.com",
        cta: "Recover missed calls as leads",
    },
    LeadSource {
        key: "multidirectory",
        name: "Multi-Directory",
        description: "Directory listings and business-enrichment leads.",
        icon: "list",
        url: "https://zaarhub.com",
        cta: "Publish to directories",
    },
    LeadSource {
        key: "funnel-swift-mobile",
        name: "FunnelSwift Mobile",
        description: "Leads captured in the FunnelSwift mobile app.",
        icon: "smartphone",
        url: "https://funnelswift.net",
        cta: "Capture leads from your phone",
    },
];

/// Alias → canonical slug. Spokes in the wild send `missedcall_responder`,
/// `multi-directory`, `IncentiveSwift`, …; attribution must not fragment.
const SOURCE_ALIASES: &[(&str, &str)] = &[
    ("missedcall_responder", "missedcallrespondr"),
    ("missedcall-responder", "missedcallrespondr"),
    ("missedcallresponder", "missedcallrespondr"),
    ("missed_call_respondr", "missedcallrespondr"),
    ("multi_directory", "multidirectory"),
    ("multi-directory", "multidirectory"),
    ("multi-directory-app", "multidirectory"),
    ("zaarhub", "multidirectory"),
    ("incentive_swift", "incentiveswift"),
    ("incentive-swift", "incentiveswift"),
    ("ada_swift", "adaswift"),
    ("ada-swift", "adaswift"),
    ("workflow_swift", "workflowswift"),
    ("workflow-swift", "workflowswift"),
    ("funnel_swift", "funnelswift"),
    ("funnel-swift", "funnelswift"),
    ("funnelswift_mobile", "funnel-swift-mobile"),
    ("funnelswift-mobile", "funnel-swift-mobile"),
    ("funnel_swift_mobile", "funnel-swift-mobile"),
];

/// Canonicalize a `source_app` string coming off the wire: lowercase, `_` → `-`,
/// strip anything that is not `[a-z0-9-]`, then apply the alias table. Unknown values
/// are returned cleaned (never discarded) so an unexpected caller is still visible in
/// "leads by capture app" instead of being silently folded into a known app.
pub fn normalize_source_app(raw: &str) -> String {
    let mut cleaned = String::new();
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '-' {
            cleaned.push(c);
        } else if c == '_' || c == ' ' || c == '.' {
            cleaned.push('-');
        }
    }
    while cleaned.contains("--") {
        cleaned = cleaned.replace("--", "-");
    }
    let cleaned = cleaned.trim_matches('-').to_string();

    if let Some((_, canonical)) = SOURCE_ALIASES.iter().find(|(a, _)| *a == cleaned) {
        return (*canonical).to_string();
    }
    if let Some(src) = LEAD_SOURCES.iter().find(|s| s.key == cleaned) {
        return src.key.to_string();
    }
    if cleaned.is_empty() {
        "unknown".to_string()
    } else {
        cleaned
    }
}

fn display_name_for(key: &str) -> Option<&'static str> {
    LEAD_SOURCES.iter().find(|s| s.key == key).map(|s| s.name)
}

// ───────────────────────── handlers ─────────────────────────

/// Lead sources for this tenant: the canonical 7 (always listed, so an idle app is
/// visible as idle) plus any unrecognized source that actually delivered leads.
async fn lead_source_rows(s: &AppState, tenant_id: Uuid) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT COALESCE(NULLIF(metadata->>'source_app',''), NULLIF(source,''), 'unknown') AS src,
                COUNT(*) AS total,
                COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '30 days') AS recent,
                MAX(created_at) AS last_lead_at
         FROM contacts
         WHERE tenant_id = $1
         GROUP BY 1",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;

    let mut out: Vec<Value> = vec![];
    for ls in LEAD_SOURCES {
        let hit = rows.iter().find(|r| r.get::<String, _>("src") == ls.key);
        let total: i64 = hit.map(|r| r.get::<i64, _>("total")).unwrap_or(0);
        let recent: i64 = hit.map(|r| r.get::<i64, _>("recent")).unwrap_or(0);
        let last: Option<chrono::DateTime<chrono::Utc>> =
            hit.and_then(|r| r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_lead_at"));
        out.push(json!({
            "key": ls.key,
            "name": ls.name,
            "description": ls.description,
            "icon": ls.icon,
            "url": ls.url,
            "cta": ls.cta,
            "leads_total": total,
            "leads_30d": recent,
            "last_lead_at": last,
            "status": if recent > 0 { "feeding" } else if total > 0 { "idle" } else { "never" },
        }));
    }

    // Unrecognized sources that really delivered leads — never hide a caller.
    for r in rows.iter() {
        let src: String = r.get("src");
        if LEAD_SOURCES.iter().any(|ls| ls.key == src) {
            continue;
        }
        let total: i64 = r.get("total");
        let recent: i64 = r.get("recent");
        let last: Option<chrono::DateTime<chrono::Utc>> =
            r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_lead_at");
        out.push(json!({
            "key": src,
            "name": display_name_for(&src).unwrap_or("Unrecognized source"),
            "description": "Leads arrived with this source_app value — add it to the hub contract if it is a real capture app.",
            "icon": "help-circle",
            "url": Value::Null,
            "cta": Value::Null,
            "leads_total": total,
            "leads_30d": recent,
            "last_lead_at": last,
            "status": if recent > 0 { "feeding" } else { "idle" },
            "unrecognized": true,
        }));
    }

    Ok(out)
}

/// GET /api/integration-center/lead-sources
pub async fn lead_sources(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let items = lead_source_rows(&s, tenant_id).await?;
    let feeding = items
        .iter()
        .filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("feeding"))
        .count();
    Ok(Json(json!({
        "items": items,
        "feeding": feeding,
        "auth": "Authorization: Bearer csk_… (create one below, then paste it into the capture app)",
        "push_endpoint": "/api/external/contacts",
    })))
}

/// GET /api/integration-center/overview — everything the hub's Integration Center renders.
pub async fn overview(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // 1. Native connectors from the CATALOGUE (available_providers), joined to this
    //    tenant's live connection state. Names/descriptions/icons are the DB rows.
    let connector_slugs: Vec<String> = crate::native_apps::connectors::NATIVE_APPS
        .iter()
        .map(|a| a.slug.to_string())
        .collect();
    let rows = sqlx::query(
        "SELECT ap.key, ap.name, ap.description, ap.icon, ap.requires_base_url,
                ac.status, ac.last_test_ok, ac.last_test_at, ac.error_message,
                (ac.id IS NOT NULL) AS connected
         FROM available_providers ap
         LEFT JOIN app_connections ac
                ON ac.app_slug = ap.key AND ac.tenant_id = $1
         WHERE ap.key = ANY($2)
         ORDER BY ap.name",
    )
    .bind(tenant_id)
    .bind(&connector_slugs)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;

    let native: Vec<Value> = rows
        .iter()
        .map(|r| {
            let key: String = r.get("key");
            let access_level = crate::native_apps::connectors::NATIVE_APPS
                .iter()
                .find(|a| a.slug == key)
                .map(|a| a.access_level)
                .unwrap_or("admin_tenant");
            json!({
                "key": key,
                "name": r.get::<String, _>("name"),
                "description": r.get::<Option<String>, _>("description"),
                "icon": r.get::<Option<String>, _>("icon"),
                "access_level": access_level,
                "auth_type": "api_key",
                "requires_base_url": r.get::<bool, _>("requires_base_url"),
                "connected": r.get::<bool, _>("connected"),
                "status": r.get::<Option<String>, _>("status"),
                "last_test_ok": r.get::<Option<bool>, _>("last_test_ok"),
                "last_test_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_test_at"),
                "error_message": r.get::<Option<String>, _>("error_message"),
            })
        })
        .collect();

    // 2. Lead sources (live, from contacts).
    let sources = lead_source_rows(&s, tenant_id).await?;
    let feeding = sources
        .iter()
        .filter(|i| i.get("status").and_then(|v| v.as_str()) == Some("feeding"))
        .count();

    // 3. The tenant's csk_ keys — prefix only.
    let key_rows = sqlx::query(
        "SELECT id, name, key_prefix, is_active, last_used_at, created_at
         FROM personal_api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;
    let keys: Vec<Value> = key_rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "name": r.get::<String, _>("name"),
                "prefix": r.get::<String, _>("key_prefix"),
                "is_active": r.get::<bool, _>("is_active"),
                "last_used_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_used_at"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            })
        })
        .collect();

    // 4. Contacts totals, for the header.
    let totals = sqlx::query(
        "SELECT COUNT(*) AS contacts,
                COUNT(*) FILTER (WHERE created_at > NOW() - INTERVAL '30 days') AS contacts_30d
         FROM contacts WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .map_err(AppError::Database)?;

    // 5. Reverse cross-sell — one CTA per capture app.
    let cross_sell: Vec<Value> = LEAD_SOURCES
        .iter()
        .map(|ls| {
            json!({
                "key": ls.key,
                "name": ls.name,
                "description": ls.description,
                "icon": ls.icon,
                "url": ls.url,
                "cta": ls.cta,
            })
        })
        .collect();

    Ok(Json(json!({
        "tenant_id": tenant_id,
        "native_integrations": {
            "items": native,
            "connected": native.iter().filter(|n| n.get("connected").and_then(|v| v.as_bool()) == Some(true)).count(),
            "catalogue_source": "available_providers",
        },
        "lead_sources": {
            "items": sources,
            "feeding": feeding,
            "auth": "Authorization: Bearer csk_…",
            "push_endpoint": "/api/external/contacts",
        },
        "personal_keys": {
            "items": keys,
            "active": keys.iter().filter(|k| k.get("is_active").and_then(|v| v.as_bool()) == Some(true)).count(),
        },
        "cross_sell": cross_sell,
        "totals": {
            "contacts": totals.get::<i64, _>("contacts"),
            "contacts_30d": totals.get::<i64, _>("contacts_30d"),
        },
    })))
}

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/overview", axum::routing::get(overview))
        .route("/lead-sources", axum::routing::get(lead_sources))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_aliases_and_junk() {
        assert_eq!(normalize_source_app("IncentiveSwift"), "incentiveswift");
        assert_eq!(
            normalize_source_app("missedcall_responder"),
            "missedcallrespondr"
        );
        assert_eq!(
            normalize_source_app("MissedCall-Responder"),
            "missedcallrespondr"
        );
        assert_eq!(normalize_source_app("multi_directory"), "multidirectory");
        assert_eq!(
            normalize_source_app("FunnelSwift-Mobile"),
            "funnel-swift-mobile"
        );
        assert_eq!(normalize_source_app("  ADA-Swift "), "adaswift");
        assert_eq!(normalize_source_app("funnelswift"), "funnelswift");
        assert_eq!(normalize_source_app(""), "unknown");
        assert_eq!(normalize_source_app("Some New App"), "some-new-app");
    }
}
