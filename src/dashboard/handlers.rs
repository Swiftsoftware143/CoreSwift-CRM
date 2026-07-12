//! Dashboard handlers — aggregate stats for tenant home view

use axum::{extract::{State, Extension}, Json, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;

/// GET /api/dashboard/stats — aggregate counts for the tenant
pub async fn stats(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let total_contacts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    let total_companies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    let total_opportunities: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    let total_revenue: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(value), 0) FROM opportunities WHERE tenant_id = $1 AND is_won = true"
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or(0.0);

    Ok(Json(json!({
        "total_contacts": total_contacts,
        "total_companies": total_companies,
        "total_opportunities": total_opportunities,
        "total_revenue": total_revenue
    })))
}

/// GET /api/dashboard/search/query — quick search counts
pub async fn search_query(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let contacts_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    let companies_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM companies WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    Ok(Json(json!({
        "total_contacts": contacts_count,
        "total_companies": companies_count
    })))
}
