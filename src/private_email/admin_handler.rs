//! Admin endpoints for tenant email limits.
//! All routes require agency_admin or owner role.

use axum::{extract::{Path, State}, Extension, Json};
use chrono::{DateTime, Utc};
use serde_json::Value as SerdeJson;
use uuid::Uuid;

use super::feature_gate;
use super::models::*;
use super::purge;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Require agency_admin (or owner). Returns Forbidden if not.
fn require_agency_admin(claims: &Claims) -> Result<(), AppError> {
    if claims.role != "agency_admin" && claims.role != "owner" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// GET /api/v1/private-email/admin/limits/:tenant_id
/// Returns the tenant's plan defaults, any overrides, and effective limits.
pub async fn get_tenant_limits(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agency_admin(&claims)?;

    let features = feature_gate::get_plan_features(&state.db, tenant_id)
        .await?
        .unwrap_or_default();

    let overrides = feature_gate::get_tenant_limits(&state.db, tenant_id).await?;

    let response = TenantEmailLimitsResponse {
        tenant_id,
        effective_max_domains: overrides
            .as_ref()
            .and_then(|o| o.max_domains)
            .unwrap_or(features.max_domains),
        effective_max_mailboxes: overrides
            .as_ref()
            .and_then(|o| o.max_mailboxes)
            .unwrap_or(features.max_mailboxes),
        effective_max_aliases_per_mailbox: overrides
            .as_ref()
            .and_then(|o| o.max_aliases_per_mailbox)
            .unwrap_or(features.max_aliases_per_mailbox),
        plan_defaults: features,
        overrides,
    };

    Ok(Json(serde_json::to_value(&response).unwrap()))
}

/// PATCH /api/v1/private-email/admin/limits/:tenant_id
/// Update (upsert) tenant-specific email limit overrides.
pub async fn set_tenant_limits(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id): Path<Uuid>,
    Json(req): Json<SetTenantEmailLimitsRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agency_admin(&claims)?;

    // Upsert: insert or update the override row
    sqlx::query(
        r#"
        INSERT INTO tenant_email_limits (tenant_id, max_domains, max_mailboxes, max_aliases_per_mailbox)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (tenant_id)
        DO UPDATE SET
            max_domains = COALESCE($2, tenant_email_limits.max_domains),
            max_mailboxes = COALESCE($3, tenant_email_limits.max_mailboxes),
            max_aliases_per_mailbox = COALESCE($4, tenant_email_limits.max_aliases_per_mailbox),
            updated_at = NOW()
        "#,
    )
    .bind(tenant_id)
    .bind(req.max_domains)
    .bind(req.max_mailboxes)
    .bind(req.max_aliases_per_mailbox)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Return the updated row
    let overrides = feature_gate::get_tenant_limits(&state.db, tenant_id).await?;
    let features = feature_gate::get_plan_features(&state.db, tenant_id)
        .await?
        .unwrap_or_default();

    let response = TenantEmailLimitsResponse {
        tenant_id,
        effective_max_domains: overrides
            .as_ref()
            .and_then(|o| o.max_domains)
            .unwrap_or(features.max_domains),
        effective_max_mailboxes: overrides
            .as_ref()
            .and_then(|o| o.max_mailboxes)
            .unwrap_or(features.max_mailboxes),
        effective_max_aliases_per_mailbox: overrides
            .as_ref()
            .and_then(|o| o.max_aliases_per_mailbox)
            .unwrap_or(features.max_aliases_per_mailbox),
        plan_defaults: features,
        overrides,
    };

    Ok(Json(serde_json::to_value(&response).unwrap()))
}

/// GET /api/v1/private-email/admin/limits
/// List all tenants that have email limit overrides.
pub async fn list_all_overrides(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    require_agency_admin(&claims)?;

    let rows = sqlx::query_as::<_, TenantEmailLimits>(
        "SELECT * FROM tenant_email_limits ORDER BY created_at DESC",
    )
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&rows).unwrap()))
}

// ── Retention Endpoints ──

/// GET /api/v1/private-email/admin/limits/:tenant_id/retention
/// Returns the tenant's email data retention settings.
pub async fn get_retention(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Json<SerdeJson>> {
    require_agency_admin(&claims)?;

    let row: Option<(Option<i32>, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT retention_days, last_purged_at FROM tenant_email_limits WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (retention_days, last_purged_at) = row
        .map(|(d, p)| (d.unwrap_or(365), p))
        .unwrap_or((365, None));

    Ok(Json(serde_json::to_value(RetentionResponse {
        tenant_id,
        retention_days,
        last_purged_at,
    }).unwrap()))
}

/// PATCH /api/v1/private-email/admin/limits/:tenant_id/retention
/// Sets a custom email data retention period for a tenant.
/// Minimum: 30 days, Maximum: 3650 days (10 years).
pub async fn set_retention(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(tenant_id): Path<Uuid>,
    Json(req): Json<SetRetentionRequest>,
) -> ApiResult<Json<SerdeJson>> {
    require_agency_admin(&claims)?;

    // Validate range
    if req.retention_days < 30 {
        return Err(AppError::BadRequest(
            "retention_days must be at least 30 days".into(),
        ));
    }
    if req.retention_days > 3650 {
        return Err(AppError::BadRequest(
            "retention_days must not exceed 3650 days (10 years)".into(),
        ));
    }

    // Upsert the retention setting
    sqlx::query(
        r#"
        INSERT INTO tenant_email_limits (tenant_id, retention_days)
        VALUES ($1, $2)
        ON CONFLICT (tenant_id)
        DO UPDATE SET
            retention_days = $2,
            updated_at = NOW()
        "#,
    )
    .bind(tenant_id)
    .bind(req.retention_days)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(serde_json::json!({
        "tenant_id": tenant_id,
        "retention_days": req.retention_days,
        "updated": true,
    })).unwrap()))
}

// ── Purge Trigger Endpoint ──

/// POST /api/v1/private-email/admin/purge/run
/// Triggers an immediate purge of expired email data.
/// - `tenant_id` in body: limits to one tenant (any role allowed if it's their own tenant).
/// - `tenant_id` omitted: purges ALL tenants (agency_admin only).
pub async fn trigger_purge(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PurgeTriggerRequest>,
) -> ApiResult<Json<SerdeJson>> {
    // Single-tenant purge requires caller to belong to that tenant
    if let Some(tid) = req.tenant_id {
        if claims.role != "agency_admin" && claims.role != "owner" {
            let caller_tenant = Uuid::parse_str(&claims.aid)
                .map_err(|_| AppError::Unauthorized)?;
            if caller_tenant != tid {
                return Err(AppError::Forbidden);
            }
        }
    } else {
        // Purge all tenants — agency_admin only
        require_agency_admin(&claims)?;
    }

    // Run purge synchronously (for real-time admin trigger)
    let summary = purge::purge_expired_emails(&state.db, req.tenant_id).await;

    Ok(Json(serde_json::to_value(&summary).unwrap()))
}
