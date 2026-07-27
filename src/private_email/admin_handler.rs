//! Admin endpoints for tenant email limits.
//! All routes require agency_admin or owner role.

use axum::{extract::{Path, State}, Extension, Json};
use uuid::Uuid;

use super::feature_gate;
use super::models::*;

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
