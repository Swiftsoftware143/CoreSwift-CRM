//! Industry Dashboard Handlers
//!
//! Manages user industry dashboard selections within CoreSwift CRM.
//! Industries map to template_categories in the workflowswift database.
//!
//! Two numbers exist and they are now the SAME number (kanban t_0986ba98):
//!
//!   * `active_count()` — how many industry tabs this TENANT has active. It is the expression the
//!     plan gate checks (`set_user_industry`) and the one `GET /api/auth/me/usage` reports, so the
//!     gate and the customer-visible usage read can never disagree. It reads the `industries` view
//!     (migration 096), i.e. this module is the WRITER behind the read t_a8a3fa27 made real.
//!   * `industry_limit()` — the plan ceiling, resolved through `module_registry` from the admin's
//!     `plan_module_features` assignment of `limit_max_industries`. `plans.max_industries` was only
//!     the SEED for that assignment (migration 072) and is deliberately no longer read here: the
//!     module used to look it up via `tenants.plan_id`, which is NULL for every one of the 118 live
//!     tenants, so the "limit" was silently the hardcoded fallback 1 and would not have moved when
//!     the admin changed the limit in the UI — the exact "gate silently stops enforcing" class the
//!     registry replaced.

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use super::models::*;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

// ── Canonical industry list ──
// Mirrors template_categories from the workflowswift database.
const CANONICAL_INDUSTRIES: &[(&str, &str, &str, &str, i32)] = &[
    (
        "site-flipping",
        "Site Flipping",
        "Website flipping, marketplace listings, TinyBrander funnel",
        "🔄",
        0,
    ),
    (
        "sales-lead-gen",
        "Sales & Lead Generation",
        "Lead capture, nurturing, and sales pipeline automation",
        "💼",
        1,
    ),
    (
        "service-businesses",
        "Service Businesses",
        "Estimate, schedule, invoice workflows",
        "🔧",
        2,
    ),
    (
        "recruitment-staffing",
        "Recruitment & Staffing",
        "Resume screening, interview coordination, placements",
        "👥",
        3,
    ),
    (
        "marketing-agencies",
        "Marketing Agencies",
        "Content calendars, ad campaigns, reporting",
        "📣",
        4,
    ),
    (
        "professional-services",
        "Professional Services",
        "Tax, legal, consulting workflows",
        "⚖️",
        5,
    ),
    (
        "ecommerce-retail",
        "Ecommerce & Retail",
        "Order fulfillment, inventory, dropshipping",
        "🛒",
        6,
    ),
    (
        "healthcare-wellness",
        "Healthcare & Wellness",
        "Patient intake, appointments, treatment planning",
        "🏥",
        7,
    ),
    (
        "construction-development",
        "Construction & Development",
        "Permit management, subcontractor bidding, development",
        "🏗️",
        8,
    ),
    (
        "grant-funding",
        "Grant & Funding",
        "Grant writing, research, submission tracking",
        "💰",
        9,
    ),
    (
        "education-training",
        "Education & Training",
        "Course creation, enrollment, certificates",
        "📚",
        10,
    ),
    (
        "publishing-media",
        "Publishing & Media",
        "Content approval, newsletters, editorial calendars",
        "📰",
        11,
    ),
    (
        "government-contracting",
        "Government Contracting",
        "Opportunity discovery, bidding, contract management",
        "🏛️",
        12,
    ),
    (
        "content-creation",
        "Content Creation",
        "AI video, images, voiceover workflows",
        "🎬",
        13,
    ),
    (
        "newsletter",
        "Newsletter",
        "Email newsletter creation and management",
        "📧",
        14,
    ),
];

fn fallback_industries() -> Vec<IndustryOption> {
    CANONICAL_INDUSTRIES
        .iter()
        .map(|(slug, name, desc, icon, order)| IndustryOption {
            slug: slug.to_string(),
            name: name.to_string(),
            description: Some(desc.to_string()),
            icon: Some(icon.to_string()),
            sort_order: Some(*order),
        })
        .collect()
}

/// GET /api/industries/available
/// Returns the full list of available industries (from hardcoded canonical list).
pub async fn list_available() -> ApiResult<impl IntoResponse> {
    Ok(Json(json!(fallback_industries())))
}

/// GET /api/industries
/// Lists the user's ACTIVE industry dashboards.
///
/// The route always said "active" and never filtered, so a deactivated tab kept coming back and the
/// picker could re-select something the user had removed (the soft-delete lie class, kanban
/// t_0986ba98). The catalogue the SPA pairs this with is `GET /api/industries/available`.
pub async fn list_user_industries(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let dashboards = sqlx::query_as::<_, UserIndustryDashboard>(
        "SELECT * FROM user_industry_dashboards WHERE user_id = $1 AND tenant_id = $2 AND is_active = true ORDER BY created_at ASC"
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(dashboards)))
}

// ── The two numbers: ONE usage expression, ONE limit source ─────────────────────────────────────

/// How many industry tabs this TENANT has active right now.
///
/// THE shared expression: the plan gate below and `features::get_usage_json`
/// (`GET /api/auth/me/usage`) both go through it, so the number a customer sees and the number the
/// gate enforces cannot drift. It reads the `industries` view (migration 096 = a lossless mirror of
/// `user_industry_dashboards`), which is the name the live usage read already used.
///
/// Scoped by TENANT, not user: a plan is assigned to a tenant and every other limit in this app
/// (`count_usage`'s contacts / users / pipelines / integrations) is tenant-scoped, so "1 industry"
/// is a workspace entitlement. DISTINCT slug because two users of one workspace activating the same
/// industry is still one industry tab.
pub async fn active_count(db: &PgPool, tenant_id: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(DISTINCT industry_slug) FROM industries \
         WHERE tenant_id = $1 AND is_active = true",
    )
    .bind(tenant_id)
    .fetch_one(db)
    .await
}

/// The tenant's effective industry ceiling, from the data-driven module registry.
///
/// `limit_max_industries` is the admin-assigned limit feature (Features & Plans → the `limits`
/// module): live it is `enabled = true, limit_value = 1` on all six plans. `None` means NO ceiling:
/// a negative assignment is the documented unlimited sentinel, and the registry's deliberate
/// `no_plan` path (a tenant with no active `tenant_plans` row resolves as ALLOWED, source = "no_plan")
/// reports `limit_value = NULL` — inventing a ceiling there would be this module re-hardcoding a
/// limit the admin never assigned.
async fn industry_limit(s: &AppState, tenant_id: Uuid) -> Result<Option<i64>, AppError> {
    let ent = crate::module_registry::resolve(&s.db, tenant_id, "limit_max_industries").await?;
    if !ent.enabled {
        return Err(AppError::UpgradeRequired(
            "Industry dashboards are not available on your current plan.".to_string(),
        ));
    }
    Ok(match ent.limit_value {
        Some(v) if v < 0.0 => None,
        Some(v) => Some(v.floor() as i64),
        None => None,
    })
}

/// POST /api/industries
/// Sets/activates an industry dashboard for the current user.
/// Checks the tenant's industry ceiling before activating a NEW industry tab.
pub async fn set_user_industry(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SetIndustryRequest>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if req.industry_slug.is_empty() {
        return Err(AppError::Validation(
            "industry_slug is required".to_string(),
        ));
    }

    // The ceiling is checked only when this activation would ADD an industry to the workspace.
    // Re-checking `user_id`-scoped (the old arm) missed the reactivation path: a row that exists with
    // is_active = false flipped back to true without consuming anything, and no gate ran at all.
    let already_active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM industries \
         WHERE tenant_id = $1 AND industry_slug = $2 AND is_active = true)",
    )
    .bind(tenant_id)
    .bind(&req.industry_slug)
    .fetch_one(&s.db)
    .await?;

    if !already_active {
        let usage = active_count(&s.db, tenant_id).await?;
        if let Some(max) = industry_limit(&s, tenant_id).await? {
            if usage >= max {
                return Err(AppError::Validation(format!(
                    "Industry dashboard limit reached ({}/{}). Upgrade to increase your limit.",
                    usage, max
                )));
            }
        }
    }

    // Upsert: insert or reactivate
    let existing = sqlx::query_as::<_, UserIndustryDashboard>(
        "SELECT * FROM user_industry_dashboards WHERE user_id = $1 AND industry_slug = $2",
    )
    .bind(user_id)
    .bind(&req.industry_slug)
    .fetch_optional(&s.db)
    .await?;

    let dashboard_name = req
        .dashboard_name
        .unwrap_or_else(|| format!("{} Dashboard", req.industry_slug.replace('-', " ")));

    let dashboard = if let Some(existing) = existing {
        sqlx::query_as::<_, UserIndustryDashboard>(
            "UPDATE user_industry_dashboards SET is_active = true, dashboard_name = $1, updated_at = NOW() WHERE id = $2 RETURNING *"
        )
        .bind(&dashboard_name)
        .bind(existing.id)
        .fetch_one(&s.db)
        .await?
    } else {
        sqlx::query_as::<_, UserIndustryDashboard>(
            "INSERT INTO user_industry_dashboards (user_id, tenant_id, industry_slug, dashboard_name) VALUES ($1, $2, $3, $4) RETURNING *"
        )
        .bind(user_id)
        .bind(tenant_id)
        .bind(&req.industry_slug)
        .bind(&dashboard_name)
        .fetch_one(&s.db)
        .await?
    };

    // Also update the tenant's default industry
    sqlx::query("UPDATE tenants SET industry_slug = $1 WHERE id = $2")
        .bind(&req.industry_slug)
        .bind(tenant_id)
        .execute(&s.db)
        .await?;

    Ok((StatusCode::CREATED, Json(json!(dashboard))))
}

/// DELETE /api/industries/:slug
/// Deactivates an industry dashboard (soft delete via is_active = false).
pub async fn remove_user_industry(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query(
        "UPDATE user_industry_dashboards SET is_active = false, updated_at = NOW() \
         WHERE user_id = $1 AND tenant_id = $2 AND industry_slug = $3 AND is_active = true",
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&slug)
    .execute(&s.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "Industry dashboard not found".to_string(),
        ));
    }

    // Deactivating the tenant's default industry leaves a dangling primary: `tenants.industry_slug`
    // is what a NEW user of this workspace starts on, so point it at the next still-active industry
    // (or NULL when none remain) instead of advertising a tab nobody has (kanban t_0986ba98).
    sqlx::query(
        "UPDATE tenants SET industry_slug = (\
             SELECT industry_slug FROM user_industry_dashboards \
             WHERE tenant_id = $1 AND is_active = true ORDER BY created_at ASC LIMIT 1) \
         WHERE id = $1 AND industry_slug = $2",
    )
    .bind(tenant_id)
    .bind(&slug)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({"message": "Industry dashboard deactivated"})))
}

/// GET /api/industries/limit
/// Returns the workspace plan's industry ceiling and the SAME usage number `GET /api/auth/me/usage`
/// reports, so the panel and the usage line can never disagree. `max`/`remaining` are `-1` when the
/// plan sets no ceiling (the sentinel this route has always used for unlimited).
pub async fn get_industry_limit(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let current = active_count(&s.db, tenant_id).await?;
    let max = industry_limit(&s, tenant_id).await?;

    Ok(Json(json!({
        "current": current,
        "max": max.unwrap_or(-1),
        "remaining": match max {
            None => -1,
            Some(m) => std::cmp::max(0, m - current),
        }
    })))
}
