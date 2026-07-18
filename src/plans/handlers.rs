//! Plans CRUD handlers — all require `agency_admin` role.

use axum::{
    extract::{State, Path, Json, Extension},
    http::StatusCode,
    response::IntoResponse,
};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;

/// Helper to enforce agency_admin role.
fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if claims.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// GET /api/plans — List all plans (agency_admin only)
pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

    let plans = sqlx::query_as::<_, Plan>(
        "SELECT * FROM plans ORDER BY sort_order ASC, name ASC",
    )
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({"plans": plans})))
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
    .bind(r.price_monthly.map(|v| Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)))
    .bind(r.price_yearly.map(|v| Decimal::from_f64_retain(v).unwrap_or(Decimal::ZERO)))
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

    Ok(Json(json!(plan)))
}

/// DELETE /api/plans/:id — Delete a plan (agency_admin only)
pub async fn delete(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_admin(&c)?;

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
