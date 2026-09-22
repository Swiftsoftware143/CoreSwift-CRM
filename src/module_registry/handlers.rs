//! Admin API for the module registry (CS-27).
//!
//! Every route here is mounted on `admin_actions::router`'s PROTECTED group, which carries
//! `auth::platform_admin::require_platform_admin_middleware` — platform-admin authority resolved from
//! the database by JWT subject (`users.is_platform_admin`, migration 071). Never a role string, and
//! never a client-supplied flag.
//!
//! GET  /api/admin/modules                        catalogue + the full per-plan assignment matrix
//! POST /api/admin/plans/:slug/modules            {module_key, enabled}
//! POST /api/admin/plans/:slug/features           {feature_key, enabled, limit_value}
//! POST /api/admin/tenants/:id/overrides          {feature_key, enabled, limit_value}
//! GET  /api/admin/tenants/:id/entitlements       the RESOLVED effective set for that tenant

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::module_registry;
use crate::AppState;

/// GET /api/admin/modules
pub async fn list_modules(State(s): State<AppState>) -> ApiResult<impl IntoResponse> {
    let cat = module_registry::catalogue(&s.db).await?;
    Ok(Json(cat))
}

#[derive(Debug, Deserialize)]
pub struct ModuleAssign {
    pub module_key: String,
    pub enabled: bool,
}

/// POST /api/admin/plans/:slug/modules — grant or revoke a whole module for a plan.
pub async fn assign_module(
    State(s): State<AppState>,
    Path(slug): Path<String>,
    Json(r): Json<ModuleAssign>,
) -> ApiResult<impl IntoResponse> {
    let plan_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM plans WHERE slug = $1")
        .bind(&slug)
        .fetch_optional(&s.db)
        .await?;
    let Some(plan_id) = plan_id else {
        return Err(AppError::NotFound(format!("No plan with slug '{slug}'")));
    };

    let module_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM modules WHERE key = $1")
        .bind(&r.module_key)
        .fetch_optional(&s.db)
        .await?;
    let Some(module_id) = module_id else {
        return Err(AppError::Validation(format!(
            "'{}' is not a registered module",
            r.module_key
        )));
    };

    sqlx::query(
        "INSERT INTO plan_modules (plan_id, module_id, enabled) VALUES ($1, $2, $3)
         ON CONFLICT (plan_id, module_id)
         DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
    )
    .bind(plan_id)
    .bind(module_id)
    .bind(r.enabled)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "plan": slug,
        "module_key": r.module_key,
        "enabled": r.enabled,
        "ok": true
    })))
}

#[derive(Debug, Deserialize)]
pub struct FeatureAssign {
    pub feature_key: String,
    pub enabled: bool,
    #[serde(default)]
    pub limit_value: Option<f64>,
}

/// POST /api/admin/plans/:slug/features — grant or revoke ONE feature, and set a limit when the
/// feature is of kind `limit`. Toggling one feature never touches a sibling (separate rows).
pub async fn assign_feature(
    State(s): State<AppState>,
    Path(slug): Path<String>,
    Json(r): Json<FeatureAssign>,
) -> ApiResult<impl IntoResponse> {
    let plan_id: Option<Uuid> = sqlx::query_scalar("SELECT id FROM plans WHERE slug = $1")
        .bind(&slug)
        .fetch_optional(&s.db)
        .await?;
    let Some(plan_id) = plan_id else {
        return Err(AppError::NotFound(format!("No plan with slug '{slug}'")));
    };

    let feat: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, kind FROM module_features WHERE key = $1")
            .bind(&r.feature_key)
            .fetch_optional(&s.db)
            .await?;
    let Some((feature_id, kind)) = feat else {
        return Err(AppError::Validation(format!(
            "'{}' is not a registered feature",
            r.feature_key
        )));
    };
    if kind == "limit" && r.enabled && r.limit_value.is_none() {
        // A limit feature needs a value to mean anything. Refuse the write rather than store a row
        // that reads as "on" with no ceiling.
        return Err(AppError::Validation(format!(
            "'{}' is a limit — send limit_value with it",
            r.feature_key
        )));
    }

    sqlx::query(
        "INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (plan_id, module_feature_id)
         DO UPDATE SET enabled = EXCLUDED.enabled, limit_value = EXCLUDED.limit_value,
                       updated_at = now()",
    )
    .bind(plan_id)
    .bind(feature_id)
    .bind(r.enabled)
    .bind(if kind == "limit" { r.limit_value } else { None })
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "plan": slug,
        "feature_key": r.feature_key,
        "kind": kind,
        "enabled": r.enabled,
        "limit_value": if kind == "limit" { r.limit_value } else { None },
        "ok": true
    })))
}

#[derive(Debug, Deserialize)]
pub struct OverrideAssign {
    pub feature_key: String,
    pub enabled: bool,
    #[serde(default)]
    pub limit_value: Option<f64>,
}

/// POST /api/admin/tenants/:id/overrides — an explicit per-tenant override, which wins over the
/// tenant's plan (resolution step 1).
pub async fn set_override(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    Json(r): Json<OverrideAssign>,
) -> ApiResult<impl IntoResponse> {
    let known: Option<String> = sqlx::query_scalar(
        "SELECT key FROM modules WHERE key = $1
         UNION ALL SELECT key FROM module_features WHERE key = $1 LIMIT 1",
    )
    .bind(&r.feature_key)
    .fetch_optional(&s.db)
    .await?;
    if known.is_none() {
        return Err(AppError::Validation(format!(
            "'{}' is neither a registered module nor feature",
            r.feature_key
        )));
    }

    let value: JsonValue = match r.limit_value {
        Some(l) => json!({ "enabled": r.enabled, "limit_value": l }),
        None => json!(r.enabled),
    };

    let n = sqlx::query(
        "UPDATE tenant_plans
            SET feature_overrides = jsonb_set(
                    COALESCE(feature_overrides, '{}'::jsonb), ARRAY[$2], $3::jsonb, true),
                updated_at = now()
          WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(tenant_id)
    .bind(&r.feature_key)
    .bind(value.to_string())
    .execute(&s.db)
    .await?
    .rows_affected();

    if n == 0 {
        return Err(AppError::NotFound(format!(
            "Tenant {tenant_id} has no active plan — assign a plan before overriding features"
        )));
    }

    let resolved = module_registry::resolve(&s.db, tenant_id, &r.feature_key).await?;
    Ok(Json(json!({
        "tenant_id": tenant_id,
        "feature_key": r.feature_key,
        "resolved": resolved,
        "ok": true
    })))
}

/// GET /api/admin/tenants/:id/entitlements — the effective set the tenant actually gets.
pub async fn tenant_entitlements(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let plan: Option<(String, String)> = sqlx::query_as(
        "SELECT p.slug, p.name FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id
          WHERE tp.tenant_id = $1 AND tp.status = 'active' LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?;

    let resolved = module_registry::resolve_all(&s.db, tenant_id).await?;
    let denied: Vec<&str> = resolved
        .iter()
        .filter(|e| !e.enabled)
        .map(|e| e.key.as_str())
        .collect();

    Ok(Json(json!({
        "tenant_id": tenant_id,
        "plan": plan.map(|(slug, name)| json!({ "slug": slug, "name": name })),
        "entitlements": resolved,
        "denied": denied,
    })))
}
