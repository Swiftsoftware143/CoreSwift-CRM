//! Industry Dashboard Handlers
//!
//! Manages user industry dashboard selections within CoreSwift CRM.
//! Industries map to template_categories in the workflowswift database.
//! Plan limits are enforced via `plans.max_industries`.

use axum::{
    extract::{State, Extension, Path},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;

// ── Canonical industry list ──
// Mirrors template_categories from the workflowswift database.
const CANONICAL_INDUSTRIES: &[(&str, &str, &str, &str, i32)] = &[
    ("site-flipping", "Site Flipping", "Website flipping, marketplace listings, TinyBrander funnel", "🔄", 0),
    ("sales-lead-gen", "Sales & Lead Generation", "Lead capture, nurturing, and sales pipeline automation", "💼", 1),
    ("service-businesses", "Service Businesses", "Estimate, schedule, invoice workflows", "🔧", 2),
    ("recruitment-staffing", "Recruitment & Staffing", "Resume screening, interview coordination, placements", "👥", 3),
    ("marketing-agencies", "Marketing Agencies", "Content calendars, ad campaigns, reporting", "📣", 4),
    ("professional-services", "Professional Services", "Tax, legal, consulting workflows", "⚖️", 5),
    ("ecommerce-retail", "Ecommerce & Retail", "Order fulfillment, inventory, dropshipping", "🛒", 6),
    ("healthcare-wellness", "Healthcare & Wellness", "Patient intake, appointments, treatment planning", "🏥", 7),
    ("construction-development", "Construction & Development", "Permit management, subcontractor bidding, development", "🏗️", 8),
    ("grant-funding", "Grant & Funding", "Grant writing, research, submission tracking", "💰", 9),
    ("education-training", "Education & Training", "Course creation, enrollment, certificates", "📚", 10),
    ("publishing-media", "Publishing & Media", "Content approval, newsletters, editorial calendars", "📰", 11),
    ("government-contracting", "Government Contracting", "Opportunity discovery, bidding, contract management", "🏛️", 12),
    ("content-creation", "Content Creation", "AI video, images, voiceover workflows", "🎬", 13),
    ("newsletter", "Newsletter", "Email newsletter creation and management", "📧", 14),
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
/// Lists the user's active industry dashboards.
pub async fn list_user_industries(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let dashboards = sqlx::query_as::<_, UserIndustryDashboard>(
        "SELECT * FROM user_industry_dashboards WHERE user_id = $1 AND tenant_id = $2 ORDER BY created_at ASC"
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(dashboards)))
}

/// POST /api/industries
/// Sets/activates an industry dashboard for the current user.
/// Checks plan max_industries limit before allowing a new industry.
pub async fn set_user_industry(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SetIndustryRequest>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if req.industry_slug.is_empty() {
        return Err(AppError::Validation("industry_slug is required".to_string()));
    }

    // Count current active industries for this user
    let current_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM user_industry_dashboards WHERE user_id = $1 AND is_active = true"
    )
    .bind(user_id)
    .fetch_one(&s.db)
    .await?;

    // Get plan limit — use the tenant's plan's max_industries, default to 1
    let max_industries: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(p.max_industries, 1)
           FROM tenants t
           LEFT JOIN plans p ON p.id = t.plan_id
           WHERE t.id = $1"#
    )
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?
    .unwrap_or(1);

    // Check if we're adding a new one
    let existing = sqlx::query_as::<_, UserIndustryDashboard>(
        "SELECT * FROM user_industry_dashboards WHERE user_id = $1 AND industry_slug = $2"
    )
    .bind(user_id)
    .bind(&req.industry_slug)
    .fetch_optional(&s.db)
    .await?;

    if existing.is_none() && current_count.0 >= max_industries as i64 && max_industries >= 0 {
        return Err(AppError::Validation(format!(
            "Industry dashboard limit reached ({}/{})",
            current_count.0, max_industries
        )));
    }

    let dashboard_name = req.dashboard_name
        .unwrap_or_else(|| format!("{} Dashboard", req.industry_slug.replace('-', " ")));

    // Upsert: insert or reactivate
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

    let result = sqlx::query(
        "UPDATE user_industry_dashboards SET is_active = false, updated_at = NOW() WHERE user_id = $1 AND industry_slug = $2"
    )
    .bind(user_id)
    .bind(&slug)
    .execute(&s.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Industry dashboard not found".to_string()));
    }

    Ok(Json(json!({"message": "Industry dashboard deactivated"})))
}

/// GET /api/industries/limit
/// Returns the user's plan industry limit and current usage.
pub async fn get_industry_limit(
    State(s): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let current_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM user_industry_dashboards WHERE user_id = $1 AND is_active = true"
    )
    .bind(user_id)
    .fetch_one(&s.db)
    .await?;

    let max_industries: i32 = sqlx::query_scalar(
        r#"SELECT COALESCE(p.max_industries, 1)
           FROM tenants t
           LEFT JOIN plans p ON p.id = t.plan_id
           WHERE t.id = $1"#
    )
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?
    .unwrap_or(1);

    Ok(Json(json!({
        "current": current_count.0,
        "max": max_industries,
        "remaining": if max_industries < 0 { -1 } else { max_industries as i64 - current_count.0 }
    })))
}
