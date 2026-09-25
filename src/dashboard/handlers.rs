//! Dashboard handlers — aggregate stats for tenant home view

use axum::{
    extract::{Extension, Query, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// GET /api/dashboard/stats — aggregate counts for the tenant
pub async fn stats(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Every counter propagates a DB error with `?`. The old `.unwrap_or(0)` folded a failed query
    // into a fabricated 0, which is indistinguishable — to any consumer — from "this tenant has
    // no rows of that kind".
    let total_contacts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&s.db)
            .await?;

    let total_companies: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM companies WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&s.db)
            .await?;

    let total_opportunities: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM opportunities WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&s.db)
            .await?;

    // `opportunities.value` is NUMERIC, and sqlx cannot decode NUMERIC into f64 — the SUM has to
    // be cast to float8 in the SELECT (same trap documented in `src/pipelines/opportunity.rs`).
    // Without the cast the decode failed on EVERY call and the old `.unwrap_or(0.0)` folded that
    // failure into a hard 0.0, so every tenant that had won a deal was told it had won nothing.
    let total_revenue: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(value), 0)::float8 FROM opportunities \
         WHERE tenant_id = $1 AND is_won = true",
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!({
        "total_contacts": total_contacts,
        "total_companies": total_companies,
        "total_opportunities": total_opportunities,
        "total_revenue": total_revenue
    })))
}

/// Query parameters of the cross-entity search (`?q=`).
#[derive(Debug, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
}

/// Rows returned per entity type — the "top 10 per type" the served customer guide advertises.
const SEARCH_LIMIT: i64 = 10;

/// GET /api/dashboard/search/query?q=david — cross-entity search (kanban t_442dc2be).
///
/// The served customer guide (`public/guide.html` → `www/coreswift/guide.html` and
/// `www-app/coreswift/guide.html`, section "Global Search") promises a search that matches
/// contacts on name/email/phone, companies on name/email and opportunities on name and returns
/// the top 10 of each. Until this change the handler never read `q` — it answered two tenant-wide
/// `COUNT(*)`s, so the documented contract existed nowhere in the code (measured 2026-09-25:
/// fleet-wide grep for `search/query` found the guide text and nothing else, i.e. no caller).
///
/// `total_contacts` / `total_companies` are KEPT because they are the only shape this route ever
/// returned, and the guide now says what they are: the tenant's own row totals — the same numbers
/// `GET /api/dashboard/stats` reports — and NOT the number of matches. The matches are the three
/// arrays. A blank or absent `q` answers `200` with three empty arrays (a search with no term
/// matches nothing); that keeps the route's historical "answered 200 without q" behaviour.
///
/// `value::float8` on opportunities is required: sqlx cannot decode NUMERIC into f64 (the same
/// trap `pipelines::opportunity` documents). Nothing here is folded into a default with
/// `unwrap_or` — a DB failure must surface as a 500, never as an empty result list.
pub async fn search_query(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(params): Query<SearchParams>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let q = params.q.unwrap_or_default().trim().to_string();
    let pattern = format!("%{}%", q);

    let contacts: Vec<serde_json::Value> = if q.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, (Uuid, String, String, Option<String>, Option<String>)>(
            r#"SELECT id, first_name, last_name, email, phone
               FROM contacts
               WHERE tenant_id = $1 AND is_active = true
                 AND (first_name ILIKE $2
                      OR last_name ILIKE $2
                      OR (first_name || ' ' || last_name) ILIKE $2
                      OR email ILIKE $2
                      OR phone ILIKE $2)
               ORDER BY last_name, first_name
               LIMIT $3"#,
        )
        .bind(tenant_id)
        .bind(&pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&s.db)
        .await?
        .into_iter()
        .map(|(id, first_name, last_name, email, phone)| {
            json!({
                "id": id,
                "first_name": first_name,
                "last_name": last_name,
                "email": email,
                "phone": phone
            })
        })
        .collect()
    };

    let companies: Vec<serde_json::Value> = if q.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, (Uuid, String, Option<String>)>(
            r#"SELECT id, name, email
               FROM companies
               WHERE tenant_id = $1 AND (name ILIKE $2 OR email ILIKE $2)
               ORDER BY name
               LIMIT $3"#,
        )
        .bind(tenant_id)
        .bind(&pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&s.db)
        .await?
        .into_iter()
        .map(|(id, name, email)| json!({"id": id, "name": name, "email": email}))
        .collect()
    };

    let opportunities: Vec<serde_json::Value> = if q.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, (Uuid, String, Option<f64>, Option<bool>)>(
            r#"SELECT id, name, value::float8, is_won
               FROM opportunities
               WHERE tenant_id = $1 AND name ILIKE $2
               ORDER BY created_at DESC
               LIMIT $3"#,
        )
        .bind(tenant_id)
        .bind(&pattern)
        .bind(SEARCH_LIMIT)
        .fetch_all(&s.db)
        .await?
        .into_iter()
        .map(|(id, name, value, is_won)| {
            json!({"id": id, "name": name, "value": value, "is_won": is_won})
        })
        .collect()
    };

    // Legacy keys — the tenant's own totals, unchanged in meaning, scoped by the same tenant_id.
    let total_contacts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&s.db)
            .await?;

    let total_companies: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM companies WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&s.db)
            .await?;

    Ok(Json(json!({
        "query": q,
        "total_contacts": total_contacts,
        "total_companies": total_companies,
        "contacts": contacts,
        "companies": companies,
        "opportunities": opportunities
    })))
}
