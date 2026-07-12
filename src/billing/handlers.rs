use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use serde_json::{json, Value};
use uuid::Uuid;
use rust_decimal::Decimal;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::errors::validate_pagination;
use super::models::*;
use super::credits;

fn count_or_zero(v: Option<i64>) -> i64 { v.unwrap_or(0) }

/// GET /api/billing/plans — List all active plans
pub async fn list_plans(State(s): State<AppState>, Query(p): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let (page, per_page) = validate_pagination(
        p.get("page").and_then(|v| v.as_i64()),
        p.get("per_page").and_then(|v| v.as_i64()),
    );
    let offset = (page - 1) * per_page;

    let plans = sqlx::query_as::<_, Plan>(
        "SELECT * FROM plans WHERE is_active = true ORDER BY sort_order ASC LIMIT $1 OFFSET $2"
    )
    .bind(per_page)
    .bind(offset)
    .fetch_all(&s.db)
    .await?;

    let total = count_or_zero(sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM plans WHERE is_active = true"
    ).fetch_one(&s.db).await?);

    Ok(Json(json!({"plans": plans, "total": total, "page": page, "per_page": per_page})))
}

/// POST /api/billing/plans — Create a plan (admin only)
pub async fn create_plan(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreatePlanRequest>) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" { return Err(AppError::Forbidden); }
    if r.name.is_empty() || r.slug.is_empty() {
        return Err(AppError::Validation("Name and slug are required".to_string()));
    }

    let plan = sqlx::query_as::<_, Plan>(
        r#"INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, checkout_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(&r.name)
    .bind(&r.slug)
    .bind(&r.description)
    .bind(Decimal::from_f64_retain(r.price_monthly).unwrap_or(Decimal::ZERO))
    .bind(Decimal::from_f64_retain(r.price_yearly).unwrap_or(Decimal::ZERO))
    .bind(&r.features)
    .bind(&r.checkout_url)
    .fetch_one(&s.db)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref d) = e {
            if d.constraint() == Some("plans_slug_key") {
                return AppError::Duplicate(format!("Plan slug '{}' exists", r.slug));
            }
        }
        AppError::Database(e)
    })?;

    Ok((StatusCode::CREATED, Json(json!(plan))))
}

/// GET /api/billing/plans/{id} — Get plan details
pub async fn get_plan(State(s): State<AppState>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let plan = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    Ok(Json(json!(plan)))
}

/// PATCH /api/billing/plans/{id} — Update plan (admin only)
pub async fn update_plan(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdatePlanRequest>) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" { return Err(AppError::Forbidden); }

    let existing = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    let price_monthly = r.price_monthly.and_then(Decimal::from_f64_retain).unwrap_or(existing.price_monthly);
    let price_yearly = r.price_yearly.and_then(Decimal::from_f64_retain).unwrap_or(existing.price_yearly);

    let plan = sqlx::query_as::<_, Plan>(
        r#"UPDATE plans SET
            name = COALESCE($1, name),
            description = COALESCE($2, description),
            price_monthly = $3,
            price_yearly = $4,
            features = COALESCE($5, features),
            checkout_url = COALESCE($6, checkout_url),
            is_active = COALESCE($7, is_active),
            updated_at = NOW()
           WHERE id = $8 RETURNING *"#
    )
    .bind(&r.name)
    .bind(&r.description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&r.features)
    .bind(&r.checkout_url)
    .bind(r.is_active)
    .bind(id)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(plan)))
}

/// DELETE /api/billing/plans/{id} — Delete plan (admin only)
pub async fn delete_plan(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" { return Err(AppError::Forbidden); }

    let r = sqlx::query("DELETE FROM plans WHERE id = $1")
        .bind(id)
        .execute(&s.db)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Plan {id} not found")));
    }
    Ok(Json(json!({"message": "Plan deleted"})))
}

// ====== Subscription ======

/// GET /api/billing/subscription — Get current tenant's subscription
pub async fn get_subscription(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let sub = sqlx::query_as::<_, TenantPlan>(
        "SELECT * FROM tenant_plans WHERE tenant_id = $1"
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    match sub {
        Some(s) => Ok(Json(json!(s))),
        None => {
            let free_plan = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE slug = 'free' LIMIT 1")
                .fetch_optional(&s.db)
                .await?;
            Ok(Json(json!({"subscription": null, "default_plan": free_plan})))
        }
    }
}

/// POST /api/billing/subscription — Create subscription
pub async fn create_subscription(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateSubscriptionRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    if c.role != "client_admin" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    sqlx::query_scalar::<_, Option<Uuid>>("SELECT id FROM plans WHERE id = $1 AND is_active = true")
        .bind(r.plan_id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound("Plan not found or inactive".to_string()))?;

    if !["monthly", "yearly"].contains(&r.billing_cycle.as_str()) {
        return Err(AppError::Validation("billing_cycle must be 'monthly' or 'yearly'".to_string()));
    }

    let count = count_or_zero(sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM tenant_plans WHERE tenant_id = $1"
    ).bind(tid).fetch_one(&s.db).await?);

    if count > 0 {
        return Err(AppError::Duplicate("Account already has a subscription".to_string()));
    }

    let sub = sqlx::query_as::<_, TenantPlan>(
        r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle, trial_ends_at, current_period_starts_at, current_period_ends_at)
           VALUES ($1, $2, $3, 'trialing', $4, NOW() + INTERVAL '14 days', NOW(), NOW() + INTERVAL '1 month')
           RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(r.plan_id).bind(&r.billing_cycle)
    .fetch_one(&s.db).await?;

    crate::audit::logger::log_event(&s.db, tid, Some(uid), "subscription.created", "subscription", Some(sub.id), Some(json!({"plan_id": r.plan_id})), None).await;

    Ok((StatusCode::CREATED, Json(json!(sub))))
}

/// PATCH /api/billing/subscription — Update subscription
pub async fn update_subscription(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<UpdateSubscriptionRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    if c.role != "client_admin" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    let existing = sqlx::query_as::<_, TenantPlan>("SELECT * FROM tenant_plans WHERE tenant_id = $1")
        .bind(tid)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound("No subscription found".to_string()))?;

    let sub = sqlx::query_as::<_, TenantPlan>(
        r#"UPDATE tenant_plans SET
            plan_id = COALESCE($1, plan_id),
            billing_cycle = COALESCE($2, billing_cycle),
            feature_overrides = COALESCE($3, feature_overrides),
            updated_at = NOW()
           WHERE id = $4 RETURNING *"#
    )
    .bind(r.plan_id).bind(&r.billing_cycle).bind(&r.feature_overrides).bind(existing.id)
    .fetch_one(&s.db).await?;

    crate::audit::logger::log_event(&s.db, tid, Some(uid), "subscription.updated", "subscription", Some(sub.id),
        Some(json!({"plan_id_old": existing.plan_id, "plan_id_new": sub.plan_id})), None).await;

    Ok(Json(json!(sub)))
}

/// POST /api/billing/subscription/cancel — Cancel subscription (downgrades to free)
pub async fn cancel_subscription(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    if c.role != "client_admin" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    let existing = sqlx::query_as::<_, TenantPlan>(
        "SELECT * FROM tenant_plans WHERE tenant_id = $1 AND (status = 'active' OR status = 'trialing')"
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("No active subscription".to_string()))?;

    let _ = sqlx::query("UPDATE tenant_plans SET status = 'canceled', canceled_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(existing.id).execute(&s.db).await?;

    // Auto-assign free plan
    if let Some(fp) = sqlx::query_scalar::<_, Option<Uuid>>("SELECT id FROM plans WHERE slug = 'free' LIMIT 1")
        .fetch_one(&s.db).await?
    {
        let _ = sqlx::query(
            r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle, current_period_starts_at, current_period_ends_at)
               VALUES ($1, $2, $3, 'active', 'monthly', NOW(), NOW() + INTERVAL '100 years')
               ON CONFLICT (tenant_id) DO UPDATE SET plan_id = $3, status = 'active', canceled_at = NULL, updated_at = NOW()"#
        )
        .bind(Uuid::new_v4()).bind(tid).bind(fp)
        .execute(&s.db).await;
    }

    crate::audit::logger::log_event(&s.db, tid, Some(uid), "subscription.canceled", "subscription", Some(existing.id), None, None).await;

    Ok(Json(json!({"message": "Subscription canceled, downgraded to Free plan"})))
}

/// GET /api/billing/features — Get effective features for current tenant
pub async fn get_features(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query_as::<_, (Uuid, String, Option<String>, serde_json::Value, serde_json::Value)>(
        r#"SELECT p.id, p.slug, p.checkout_url, p.features, COALESCE(tp.feature_overrides, '{}'::jsonb)
           FROM tenant_plans tp
           JOIN plans p ON tp.plan_id = p.id
           WHERE tp.tenant_id = $1 AND tp.status IN ('active', 'trialing')"#
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    let (plan_id, plan_slug, checkout_url, plan_features, overrides) = match row {
        Some(r) => r,
        None => return Err(AppError::NotFound("No active subscription — assign a plan first".to_string())),
    };

    let (features, limits) = merge_features(&plan_features, &overrides);

    Ok(Json(json!(FeaturesResponse {
        plan: PlanSummary { id: plan_id, name: plan_slug.clone(), slug: plan_slug, checkout_url },
        features,
        limits,
    })))
}

/// GET /api/billing/credits/balance — Get available credit balance
pub async fn get_credit_balance(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let summary = credits::get_credit_summary(&s.db, tid).await;
    Ok(Json(summary))
}

/// GET /api/billing/credits/usage — Get detailed transaction history
pub async fn get_credit_usage(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let txns = sqlx::query_as::<_, (Uuid, String, i32, String, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, action_type, credits, description, created_at FROM credit_transactions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;

    let total = count_or_zero(sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM credit_transactions WHERE tenant_id = $1"
    ).bind(tid).fetch_one(&s.db).await?);

    Ok(Json(json!({"transactions": txns, "total": total, "page": page, "per_page": per_page})))
}

/// POST /api/billing/credits/buy — Purchase additional credits (placeholder for Stripe/checkout)
pub async fn buy_credits(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let amount = r.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
    if amount <= 0 {
        return Err(AppError::Validation("Amount must be positive".to_string()));
    }

    // Price: 100 credits = $1, 1000 = $9, 5000 = $40
    let price = match amount {
        a if a >= 5000 => a as f64 * 0.008,
        a if a >= 1000 => a as f64 * 0.009,
        _ => amount as f64 * 0.01,
    };

    let _ = sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, action_type, credits, description)
           VALUES ($1, $2, 'credit_purchase', $3, $4)"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(amount)
    .bind(format!("Purchased {} credits for ${:.2}", amount, price))
    .execute(&s.db).await?;

    tracing::info!(tenant = %tid, credits = %amount, price = %price, "Credits purchased");

    Ok(Json(json!({"message": format!("{} credits added to account", amount), "amount": amount, "charged": price})))
}
