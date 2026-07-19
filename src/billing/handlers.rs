use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use sqlx::Row;
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
        r#"INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, checkout_url, payment_provider, thank_you_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(&r.name)
    .bind(&r.slug)
    .bind(&r.description)
    .bind(Decimal::from_f64_retain(r.price_monthly).unwrap_or(Decimal::ZERO))
    .bind(Decimal::from_f64_retain(r.price_yearly).unwrap_or(Decimal::ZERO))
    .bind(&r.features)
    .bind(&r.checkout_url)
    .bind(&r.payment_provider)
    .bind(&r.thank_you_url)
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
            payment_provider = COALESCE($7, payment_provider),
            thank_you_url = COALESCE($8, thank_you_url),
            is_active = COALESCE($9, is_active),
            updated_at = NOW()
           WHERE id = $10 RETURNING *"#
    )
    .bind(&r.name)
    .bind(&r.description)
    .bind(price_monthly)
    .bind(price_yearly)
    .bind(&r.features)
    .bind(&r.checkout_url)
    .bind(&r.payment_provider)
    .bind(&r.thank_you_url)
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

    let row = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, Option<String>, serde_json::Value, serde_json::Value)>(
        r#"SELECT p.id, p.slug, p.checkout_url, p.payment_provider, p.thank_you_url, p.features, COALESCE(tp.feature_overrides, '{}'::jsonb)
           FROM tenant_plans tp
           JOIN plans p ON tp.plan_id = p.id
           WHERE tp.tenant_id = $1 AND tp.status IN ('active', 'trialing')"#
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    let (plan_id, plan_slug, checkout_url, payment_provider, thank_you_url, plan_features, overrides) = match row {
        Some(r) => r,
        None => return Err(AppError::NotFound("No active subscription — assign a plan first".to_string())),
    };

    let (features, limits) = merge_features(&plan_features, &overrides);

    Ok(Json(json!(FeaturesResponse {
        plan: PlanSummary { id: plan_id, name: plan_slug.clone(), slug: plan_slug, checkout_url, payment_provider, thank_you_url },
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

// ──────────────────────────────────────────────
// Stripe/PayPal Checkout
// ──────────────────────────────────────────────

#[derive(serde::Deserialize)]
pub struct CreateCheckoutRequest {
    pub provider_type: String,          // "stripe" or "paypal"
    pub plan_id: Option<Uuid>,
    pub amount: Option<f64>,
    pub currency: Option<String>,
    pub success_url: Option<String>,
    pub cancel_url: Option<String>,
    pub metadata: Option<Value>,
}

/// POST /api/billing/checkout/create — Create a Stripe/PayPal checkout session
pub async fn create_checkout_session(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateCheckoutRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let provider_type = r.provider_type.to_lowercase();

    if provider_type != "stripe" && provider_type != "paypal" {
        return Err(AppError::Validation("provider_type must be 'stripe' or 'paypal'".to_string()));
    }

    // Get the API key for this provider
    let api_key = sqlx::query_scalar::<_, String>(
        "SELECT api_key FROM provider_keys WHERE tenant_id = $1 AND provider = $2 AND is_active = true"
    )
    .bind(tenant_id)
    .bind(&provider_type)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::Validation(format!("No active {} API key configured", provider_type)))?;

    // Determine amount and plan info
    let (amount, currency, purchasable_type, purchasable_id) = if let Some(pid) = r.plan_id {
        let plan = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
            .bind(pid)
            .fetch_optional(&s.db)
            .await?
            .ok_or_else(|| AppError::NotFound("Plan not found".to_string()))?;
        (plan.price_monthly, "USD".to_string(), "plan".to_string(), Some(pid))
    } else {
        let amt = Decimal::from_f64_retain(r.amount.unwrap_or(0.0)).unwrap_or(Decimal::ZERO);
        (amt, r.currency.unwrap_or_else(|| "USD".to_string()), "credits".to_string(), None)
    };

    let return_url = r.success_url.unwrap_or_default();
    let metadata = r.metadata.unwrap_or_else(|| json!({}));

    // Call Stripe/PayPal API
    let provider_session = match provider_type.as_str() {
        "stripe" => create_stripe_session(&api_key, amount, &currency, &purchasable_type, &return_url, &metadata).await?,
        "paypal" => create_paypal_session(&api_key, amount, &currency, &purchasable_type, &return_url, &metadata).await?,
        _ => return Err(AppError::Validation("Invalid provider".to_string())),
    };

    let provider_session_id = provider_session["id"].as_str().unwrap_or("").to_string();
    let checkout_url = provider_session["url"].as_str().unwrap_or("").to_string();

    // Store checkout session
    let session_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO checkout_sessions (id, tenant_id, user_id, provider_type, provider_session_id, purchasable_type, purchasable_id, amount, currency, metadata, return_url)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#
    )
    .bind(session_id)
    .bind(tenant_id)
    .bind(c.sub.parse::<Uuid>().ok())
    .bind(&provider_type)
    .bind(&provider_session_id)
    .bind(&purchasable_type)
    .bind(purchasable_id)
    .bind(amount)
    .bind(&currency)
    .bind(&metadata)
    .bind(&return_url)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "id": session_id,
        "provider_session_id": provider_session_id,
        "checkout_url": checkout_url,
        "provider_type": provider_type,
    })))
}

/// GET /api/billing/checkout/sessions — List checkout sessions for this tenant
pub async fn list_checkout_sessions(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let sessions = sqlx::query_as::<_, CheckoutSessionSummary>(
        r#"SELECT id, provider_type, provider_session_id, status, amount, currency, purchasable_type, created_at
           FROM checkout_sessions
           WHERE tenant_id = $1
           ORDER BY created_at DESC LIMIT 50"#
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(sessions)))
}

// ──────────────────────────────────────────────
// Stripe/PayPal Webhooks (public, no auth)
// ──────────────────────────────────────────────

/// POST /api/billing/webhooks/stripe — Stripe webhook
pub async fn stripe_webhook(
    State(s): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let event_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("unknown");
    tracing::info!("Stripe webhook: event_type={}", event_type);

    if event_type == "checkout.session.completed" {
        if let Some(data) = payload.get("data").and_then(|d| d.get("object")) {
            let provider_session_id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let customer_email = data.get("customer_details")
                .and_then(|d| d.get("email"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let customer_name = data.get("customer_details")
                .and_then(|d| d.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("Valued Customer");

            // Update checkout session status
            let row = sqlx::query(
                "UPDATE checkout_sessions SET status = 'completed', webhook_received_at = NOW(), updated_at = NOW() WHERE provider_session_id = $1 AND status = 'pending' RETURNING tenant_id, metadata"
            )
            .bind(provider_session_id)
            .fetch_optional(&s.db)
            .await?;

            if let Some(r) = row {
                let tenant_id: Uuid = r.get("tenant_id");
                let metadata: Value = r.get("metadata");

                let email = if !customer_email.is_empty() {
                    customer_email
                } else {
                    metadata.get("customer_email").and_then(|v| v.as_str()).unwrap_or("")
                };

                if !email.is_empty() {
                    if let Err(e) = deliver_credentials(&s.db, email, customer_name, tenant_id).await {
                        tracing::error!("Credential delivery failed: {}", e);
                    }
                }
            }
        }
    }

    Ok(Json(json!({"received": true})))
}

/// POST /api/billing/webhooks/paypal — PayPal webhook
pub async fn paypal_webhook(
    State(s): State<AppState>,
    Json(payload): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let event_type = payload.get("event_type").and_then(|v| v.as_str()).unwrap_or("unknown");
    tracing::info!("PayPal webhook: event_type={}", event_type);

    match event_type {
        "CHECKOUT.ORDER.APPROVED" | "PAYMENT.CAPTURE.COMPLETED" => {
            if let Some(resource) = payload.get("resource") {
                let provider_session_id = resource.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let payer = payload.get("resource").and_then(|r| r.get("payer"));
                let customer_email = payer.and_then(|p| p.get("email_address")).and_then(|v| v.as_str()).unwrap_or("");
                let customer_name = payer.and_then(|p| p.get("name"))
                    .and_then(|n| n.get("given_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Valued Customer");

                let row = sqlx::query(
                    "UPDATE checkout_sessions SET status = 'completed', webhook_received_at = NOW(), updated_at = NOW() WHERE provider_session_id = $1 AND status = 'pending' RETURNING tenant_id, metadata"
                )
                .bind(provider_session_id)
                .fetch_optional(&s.db)
                .await?;

                if let Some(r) = row {
                    let tenant_id: Uuid = r.get("tenant_id");
                    let metadata: Value = r.get("metadata");

                    let email = if !customer_email.is_empty() {
                        customer_email
                    } else {
                        metadata.get("customer_email").and_then(|v| v.as_str()).unwrap_or("")
                    };

                    if !email.is_empty() {
                        if let Err(e) = deliver_credentials(&s.db, email, customer_name, tenant_id).await {
                            tracing::error!("Credential delivery failed: {}", e);
                        }
                    }
                }
            }
        }
        _ => tracing::debug!("Unhandled PayPal event: {}", event_type),
    }

    Ok(Json(json!({"received": true})))
}

// ──────────────────────────────────────────────
// Credential Delivery
// ──────────────────────────────────────────────

use rand::Rng;

async fn deliver_credentials(
    db: &sqlx::PgPool,
    email: &str,
    customer_name: &str,
    tenant_id: Uuid,
) -> Result<(), String> {
    // Look up existing user
    let existing_user = sqlx::query_as::<_, UserRow>(
        "SELECT id, email, password_hash, name FROM users WHERE email = $1"
    )
    .bind(email)
    .fetch_optional(db)
    .await
    .map_err(|e| format!("DB error: {}", e))?;

    if let Some(user) = existing_user {
        let has_password = !user.password_hash.is_empty() && user.password_hash != " ";

        if has_password {
            // Existing user — queue purchase confirmation via template
            let vars = json!({
                "name": user.name,
                "plan_name": "your plan",
                "app_url": "https://app.coreswiftcrm.com",
            });
            let _ = crate::email::send_template_email(db, tenant_id, email, "purchase_confirmed", &vars)
                .await
                .map_err(|e| format!("Failed to queue purchase confirmation via template: {}", e))?;
        } else {
            // User exists but no password — generate and queue welcome
            let temp_password = generate_temp_password();
            let hash = hash_password(&temp_password);
            sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
                .bind(&hash)
                .bind(user.id)
                .execute(db)
                .await
                .map_err(|e| format!("Failed to update password: {}", e))?;

            let vars = json!({
                "name": user.name,
                "email": email,
                "password": temp_password,
                "app_url": "https://app.coreswiftcrm.com",
            });
            let _ = crate::email::send_template_email(db, tenant_id, email, "welcome", &vars)
                .await
                .map_err(|e| format!("Failed to queue welcome via template: {}", e))?;
        }
    } else {
        // New user — create user + tenant
        let user_id = Uuid::new_v4();
        let temp_password = generate_temp_password();
        let hash = hash_password(&temp_password);

        // Check if tenant exists
        let tenant = sqlx::query_as::<_, TenantRow>(
            "SELECT id, name FROM tenants WHERE id = $1"
        )
        .bind(tenant_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("DB error: {}", e))?;

        let (tid, tname) = if let Some(t) = tenant {
            (t.id, t.name)
        } else {
            let tid = Uuid::new_v4();
            sqlx::query("INSERT INTO tenants (id, name, slug, created_at, updated_at) VALUES ($1, $2, $3, NOW(), NOW())")
                .bind(tid)
                .bind(customer_name)
                .bind(&format!("cust-{}", &tid.to_string()[..8]))
                .execute(db)
                .await
                .map_err(|e| format!("Failed to create tenant: {}", e))?;
            (tid, customer_name.to_string())
        };

        sqlx::query(
            "INSERT INTO users (id, tenant_id, email, password_hash, name, role) VALUES ($1, $2, $3, $4, $5, 'owner')"
        )
        .bind(user_id)
        .bind(tid)
        .bind(email)
        .bind(&hash)
        .bind(customer_name)
        .execute(db)
        .await
        .map_err(|e| format!("Failed to create user: {}", e))?;

        let vars = json!({
                "name": customer_name,
                "email": email,
                "password": temp_password,
                "account_name": &tname,
                "app_url": "https://app.coreswiftcrm.com",
            });
        let _ = crate::email::send_template_email(db, tid, email, "welcome", &vars)
            .await
            .map_err(|e| format!("Failed to queue welcome via template: {}", e))?;
    }

    Ok(())
}

fn generate_temp_password() -> String {
    let chars: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789!@#$%&";
    let mut rng = rand::thread_rng();
    (0..14).map(|_| {
        let idx = rng.gen_range(0..chars.len());
        chars[idx] as char
    }).collect()
}

fn hash_password(password: &str) -> String {
    use argon2::{Argon2, PasswordHasher};
    use password_hash::SaltString;
    let salt = SaltString::generate(&mut rand::thread_rng());
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .expect("argon2 hashing failed")
        .to_string()
}

// ──────────────────────────────────────────────
// Stripe/PayPal API helpers
// ──────────────────────────────────────────────

async fn create_stripe_session(
    api_key: &str,
    amount: Decimal,
    currency: &str,
    _purchasable_type: &str,
    return_url: &str,
    metadata: &Value,
) -> Result<Value, AppError> {
    let client = reqwest::Client::new();
    let amount_cents = (amount * Decimal::new(100, 0)).round();
    let amount_str = amount_cents.to_string();

    let mut params = std::collections::HashMap::new();
    let currency_lower = currency.to_lowercase();
    params.insert("mode", "payment");
    params.insert("success_url", return_url);
    params.insert("cancel_url", return_url);
    params.insert("line_items[0][price_data][currency]", &currency_lower);
    params.insert("line_items[0][price_data][product_data][name]", "CoreSwift CRM Purchase");
    params.insert("line_items[0][price_data][unit_amount]", &amount_str);
    params.insert("line_items[0][quantity]", "1");

    if let Some(obj) = metadata.as_object() {
        for (k, v) in obj {
            if let Some(s) = v.as_str() {
                params.insert("metadata[0]", s);
            }
        }
    }

    let resp = client
        .post("https://api.stripe.com/v1/checkout/sessions")
        .header("Authorization", format!("Bearer {}", api_key))
        .form(&params)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Stripe API error: {}", e)))?;

    let body: Value = resp.json().await
        .map_err(|e| AppError::Internal(format!("Stripe parse error: {}", e)))?;

    let session_id = body.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let url = body.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();

    Ok(json!({"id": session_id, "url": url}))
}

async fn create_paypal_session(
    _api_key: &str,
    _amount: Decimal,
    _currency: &str,
    _purchasable_type: &str,
    _return_url: &str,
    _metadata: &Value,
) -> Result<Value, AppError> {
    // TODO: Implement PayPal order creation
    Err(AppError::Internal("PayPal checkout not yet implemented in CoreSwift".to_string()))
}

// ── Data types ──

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct CheckoutSessionSummary {
    id: Uuid,
    provider_type: String,
    provider_session_id: Option<String>,
    status: String,
    amount: Decimal,
    currency: String,
    purchasable_type: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    name: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TenantRow {
    id: Uuid,
    name: String,
}
