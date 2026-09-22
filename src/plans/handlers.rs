//! Plans CRUD handlers — all require `agency_admin` role.

use axum::{
    extract::{Extension, Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use super::models::*;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Helper to enforce agency_admin role.
fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if claims.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// Fire-and-forget sync of this plan to FunnelSwift's affiliate_products
async fn sync_plan_to_affiliate(
    config: &crate::config::AppConfig,
    action: &str,
    plan_name: &str,
    plan_price: f64,
    is_active: bool,
) {
    let url = format!(
        "{}/api/v1/internal/sync-affiliate-plan",
        config.funnelswift_url.trim_end_matches('/')
    );
    let api_key = config.internal_sync_key.clone();

    let action_owned = action.to_string();
    let plan_name_owned = plan_name.to_string();

    let payload = serde_json::json!({
        "action": &action_owned,
        "plan_name": &plan_name_owned,
        "plan_price": plan_price,
        "source_app": "coreswift",
        "is_active": is_active,
        "owner_name": "SwiftSoftware",
        "product_type": "software",
        "api_key": &api_key,
    });

    tokio::spawn(async move {
        match reqwest::Client::new()
            .post(&url)
            .json(&payload)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    tracing::info!(
                        "sync-affiliate-plan {} {}: {}",
                        action_owned,
                        plan_name_owned,
                        status
                    );
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        "sync-affiliate-plan {} {} failed: {} - {}",
                        action_owned,
                        plan_name_owned,
                        status,
                        body
                    );
                }
            }
            Err(e) => tracing::warn!(
                "sync-affiliate-plan {} {} error: {}",
                action_owned,
                plan_name_owned,
                e
            ),
        }
    });
}

/// GET /api/plans — List all plans (agency_admin only)
pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    let plans = sqlx::query_as::<_, Plan>("SELECT * FROM plans ORDER BY sort_order ASC, name ASC")
        .fetch_all(&s.db)
        .await?;

    Ok(Json(json!({"plans": plans})))
}

/// The feature keys the admin can toggle per plan, plus every plan's current values.
/// Read straight from the module registry tables now — the catalogue is DATA, so a newly
/// registered module appears here with no code change — the catalogue used to be a Rust const. Shape kept identical for the existing admin console:
/// `{ "features": [{key, label, module, note}], "plans": [{slug, name, features}] }`.
pub async fn feature_registry(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    // Read explicit columns rather than `SELECT *` into the `Plan` model: that model still
    // declares columns this table does not have (max_deals, max_users, max_storage_mb,
    // payment_link), so a star-select fails to map and the endpoint 500s.
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT slug, name, COALESCE(features, '{}'::jsonb) AS features
           FROM plans
          ORDER BY sort_order ASC NULLS LAST, name ASC",
    )
    .fetch_all(&s.db)
    .await?;

    let feats: Vec<(String, String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT f.key, f.name, m.key, f.description, f.is_active
           FROM module_features f JOIN modules m ON m.id = f.module_id
          WHERE f.kind = 'boolean' AND m.is_active
          ORDER BY m.sort_order, f.sort_order, f.key",
    )
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({
        "features": feats
            .into_iter()
            .map(|(key, label, module, note, is_active)| json!({
                "key": key,
                "label": label,
                "module": module,
                "note": note.unwrap_or_default(),
                "is_active": is_active,
            }))
            .collect::<Vec<_>>(),
        "plans": rows
            .into_iter()
            .map(|(slug, name, features)| json!({ "slug": slug, "name": name, "features": features }))
            .collect::<Vec<_>>(),
    })))
}

/// POST /api/plans — Create a new plan (agency_admin only)
pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreatePlanRequest>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    if r.name.is_empty() {
        return Err(AppError::Validation("Plan name is required".to_string()));
    }

    let plan = sqlx::query_as::<_, Plan>(
        r#"INSERT INTO plans (id, name, description, price_monthly, price_yearly,
           max_contacts, max_deals, max_users, max_storage_mb, features, payment_link, payment_provider, sort_order, max_industries)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(&r.name)
    .bind(&r.description)
    .bind(Decimal::from_f64_retain(r.price_monthly.unwrap_or(0.0)).unwrap_or(Decimal::ZERO))
    .bind(Decimal::from_f64_retain(r.price_yearly.unwrap_or(0.0)).unwrap_or(Decimal::ZERO))
    .bind(r.max_contacts.unwrap_or(-1))
    .bind(r.max_deals.unwrap_or(-1))
    .bind(r.max_users.unwrap_or(-1))
    .bind(r.max_storage_mb.unwrap_or(100))
    .bind(r.features.unwrap_or(serde_json::Value::Object(Default::default())))
    .bind(&r.payment_link)
    .bind(&r.payment_provider)
    .bind(r.sort_order.unwrap_or(0))
    .bind(r.max_industries.unwrap_or(1))
    .fetch_one(&s.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "Failed to create plan");
        AppError::Database(e)
    })?;

    // Sync to FunnelSwift affiliate products
    let plan_name_str = r.name.clone();
    let plan_price_f64 = r.price_monthly.unwrap_or(0.0);
    let config_clone = s.config.clone();
    tokio::spawn(async move {
        sync_plan_to_affiliate(
            &config_clone,
            "create",
            &plan_name_str,
            plan_price_f64,
            true,
        )
        .await;
    });

    Ok((StatusCode::CREATED, Json(json!(plan))))
}

/// GET /api/plans/:id — Get a single plan (agency_admin only)
pub async fn get(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    let plan = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    Ok(Json(json!(plan)))
}

/// PATCH /api/plans/:id — Update a plan (agency_admin only)
pub async fn update(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdatePlanRequest>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    let plan = sqlx::query_as::<_, Plan>(
        r#"UPDATE plans SET
           name = COALESCE($1, name),
           description = COALESCE($2, description),
           price_monthly = CASE WHEN $3 IS NOT NULL THEN $3 ELSE price_monthly END,
           price_yearly = CASE WHEN $4 IS NOT NULL THEN $4 ELSE price_yearly END,
           max_contacts = COALESCE($5, max_contacts),
           max_deals = COALESCE($6, max_deals),
           max_users = COALESCE($7, max_users),
           max_storage_mb = COALESCE($8, max_storage_mb),
           features = COALESCE($9, features),
           payment_link = COALESCE($10, payment_link),
           payment_provider = COALESCE($11, payment_provider),
           is_active = COALESCE($12, is_active),
           sort_order = COALESCE($13, sort_order),
           max_industries = COALESCE($14, max_industries),
           updated_at = NOW()
           WHERE id = $15 RETURNING *"#,
    )
    .bind(&r.name)
    .bind(&r.description)
    .bind(
        r.price_monthly
            .map(|v| Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)),
    )
    .bind(
        r.price_yearly
            .map(|v| Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)),
    )
    .bind(r.max_contacts)
    .bind(r.max_deals)
    .bind(r.max_users)
    .bind(r.max_storage_mb)
    .bind(&r.features)
    .bind(&r.payment_link)
    .bind(&r.payment_provider)
    .bind(r.is_active)
    .bind(r.sort_order)
    .bind(r.max_industries)
    .bind(id)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    // The registry tables are the source of truth for gating now, so a `features` write through this
    // legacy path has to land there too — otherwise the admin console's switches would look saved
    // and change nothing. Key mapping comes from `modules.legacy_feature_key` (data), not a const.
    if let Some(f) = r.features.as_ref() {
        match crate::module_registry::sync_legacy_features(&s.db, id, f).await {
            Ok(n) => {
                tracing::info!(rows = n, plan = %id, "plan features mirrored into module registry")
            }
            Err(e) => tracing::warn!(error = %e, "plan feature sync to module registry failed"),
        }
    }

    // Sync to FunnelSwift affiliate products
    let plan_name_str = r.name.clone().unwrap_or_else(|| plan.name.clone());
    let plan_price_f64 = r.price_monthly.unwrap_or(0.0);
    let is_active_val = r.is_active.unwrap_or(true);
    let config_clone = s.config.clone();
    tokio::spawn(async move {
        sync_plan_to_affiliate(
            &config_clone,
            "update",
            &plan_name_str,
            plan_price_f64,
            is_active_val,
        )
        .await;
    });

    Ok(Json(json!(plan)))
}

/// DELETE /api/plans/:id — Delete a plan (agency_admin only)
pub async fn delete(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    // Get plan name for sync before deleting
    let plan_for_sync = sqlx::query_as::<_, (String,)>("SELECT name FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?;

    if let Some((ref plan_name,)) = plan_for_sync {
        let plan_name_str = plan_name.clone();
        let config_clone = s.config.clone();
        tokio::spawn(async move {
            sync_plan_to_affiliate(&config_clone, "deactivate", &plan_name_str, 0.0, false).await;
        });
    }

    // Clear plan_id references from tenants first
    sqlx::query("UPDATE tenants SET plan_id = NULL WHERE plan_id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;

    let r = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Plan {id} not found")));
    }

    Ok(Json(json!({"message": "Plan deleted"})))
}
