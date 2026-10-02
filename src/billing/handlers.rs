use super::credits;
use super::models::*;
use crate::auth::models::Claims;
use crate::errors::validate_pagination;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

fn count_or_zero(v: Option<i64>) -> i64 {
    v.unwrap_or(0)
}

/// The billing-cycle vocabulary the schema CHECK enforces, declared ONCE so the create and
/// update arms cannot drift into accepting different values (kanban t_0c1c58d0).
const BILLING_CYCLES: [&str; 2] = ["monthly", "yearly"];

/// The cycle a tenant holds on the Free plan. Every free-plan writer in this crate writes this
/// same value (signup seat, admin tenant create, webhook `tenant.create`, and the cancellation
/// upsert's own INSERT arm); the cancel upsert's UPDATE arm now converges to it instead of
/// keeping the cancelled plan's cycle (kanban t_0c1c58d0).
const FREE_PLAN_BILLING_CYCLE: &str = "monthly";

/// The one validator for the billing-cycle vocabulary, shared by POST (create) and PATCH
/// (update) so a bad value is refused the same way on both arms. Measured live before this:
/// POST answered 422 while PATCH bound the value unvalidated and the DB CHECK surfaced as a
/// generic 500 "Database error" (kanban t_0c1c58d0).
fn validate_billing_cycle(cycle: &str) -> Result<(), AppError> {
    if BILLING_CYCLES.contains(&cycle) {
        Ok(())
    } else {
        Err(AppError::Validation(
            "billing_cycle must be 'monthly' or 'yearly'".to_string(),
        ))
    }
}

/// GET /api/billing/plans — List all active plans
pub async fn list_plans(
    State(s): State<AppState>,
    Query(p): Query<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let (page, per_page) = validate_pagination(
        p.get("page").and_then(|v| v.as_i64()),
        p.get("per_page").and_then(|v| v.as_i64()),
    );
    let offset = (page - 1) * per_page;

    let plans = sqlx::query_as::<_, Plan>(
        "SELECT * FROM plans WHERE is_active = true ORDER BY sort_order ASC LIMIT $1 OFFSET $2",
    )
    .bind(per_page)
    .bind(offset)
    .fetch_all(&s.db)
    .await?;

    let total = count_or_zero(
        sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM plans WHERE is_active = true")
            .fetch_one(&s.db)
            .await?,
    );

    Ok(Json(
        json!({"plans": plans, "total": total, "page": page, "per_page": per_page}),
    ))
}

/// POST /api/billing/plans — Create a plan (admin only)
pub async fn create_plan(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreatePlanRequest>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }
    if r.name.is_empty() || r.slug.is_empty() {
        return Err(AppError::Validation(
            "Name and slug are required".to_string(),
        ));
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
pub async fn get_plan(
    State(s): State<AppState>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let plan = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    Ok(Json(json!(plan)))
}

/// PATCH /api/billing/plans/{id} — Update plan (admin only)
pub async fn update_plan(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdatePlanRequest>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    let existing = sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE id = $1")
        .bind(id)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Plan {id} not found")))?;

    let price_monthly = r
        .price_monthly
        .and_then(Decimal::from_f64_retain)
        .unwrap_or(existing.price_monthly);
    let price_yearly = r
        .price_yearly
        .and_then(Decimal::from_f64_retain)
        .unwrap_or(existing.price_yearly);

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
           WHERE id = $10 RETURNING *"#,
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
pub async fn delete_plan(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

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
pub async fn get_subscription(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let sub = sqlx::query_as::<_, TenantPlan>("SELECT * FROM tenant_plans WHERE tenant_id = $1")
        .bind(tid)
        .fetch_optional(&s.db)
        .await?;

    match sub {
        Some(s) => Ok(Json(json!(s))),
        None => {
            let free_plan =
                sqlx::query_as::<_, Plan>("SELECT * FROM plans WHERE slug = 'free' LIMIT 1")
                    .fetch_optional(&s.db)
                    .await?;
            Ok(Json(
                json!({"subscription": null, "default_plan": free_plan}),
            ))
        }
    }
}

/// Fire-and-forget notification to FunnelSwift that a tenant upgraded to a paid plan,
/// so the referring affiliate is credited (permanent, no expiry).
pub(crate) async fn notify_funnelswift_upgrade(
    config: &crate::config::AppConfig,
    email: &str,
    plan_name: &str,
    plan_price: f64,
    event_id: &str,
) {
    if email.is_empty() || config.funnelswift_url.is_empty() {
        return;
    }
    let url = format!(
        "{}/api/v1/internal/affiliate/upgrade-event",
        config.funnelswift_url.trim_end_matches('/')
    );
    let key = config.internal_sync_key.clone();
    let payload = serde_json::json!({
        "source_app": "coreswift",
        "email": email,
        "plan_name": plan_name,
        "plan_price": plan_price,
        "event_id": event_id,
    });
    tokio::spawn(async move {
        let _ = reqwest::Client::new()
            .post(&url)
            .header("x-internal-key", key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await;
    });
}

/// Resolve the tenant's owner email + plan, and notify FunnelSwift if it's a PAID upgrade.
/// Free plan is the initial attribution (handled at tag time), not an upgrade.
pub(crate) async fn attribute_plan_upgrade(state: &AppState, tenant_id: Uuid, plan_id: Uuid) {
    let plan: Option<(String, Option<f64>)> =
        sqlx::query_as("SELECT name, price_monthly::float8 FROM plans WHERE id = $1")
            .bind(plan_id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten();
    let Some((plan_name, price)) = plan else {
        return;
    };
    let plan_price = price.unwrap_or(0.0);
    if plan_price <= 0.0 {
        return;
    }
    let email: Option<String> = sqlx::query_scalar(
        "SELECT email FROM users WHERE tenant_id = $1 AND role = 'owner' LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();
    let Some(email) = email else {
        return;
    };
    notify_funnelswift_upgrade(
        &state.config,
        &email,
        &plan_name,
        plan_price,
        &Uuid::new_v4().to_string(),
    )
    .await;
}

/// May this caller write the tenant's plan / billing record?
///
/// DECISION (kanban t_8d91f70c, measured 2026-10-02). Both writers of `tenant_plans` in this
/// module used to open with `role != "client_admin" && role != "agency_admin"`. **No user in the
/// live database holds `client_admin`** (`select role, count(*) from users group by 1` on
/// `coreswift_crm`: agency_admin 1, owner 5, admin 1, client_admin 0), so the predicate was
/// unsatisfiable and the only arm that binds a caller-chosen `billing_cycle` was unreachable for
/// every real customer — while the served workspace panel PRINTS the cycle as a stat
/// (`www-app/coreswift/index.html`, "Billing cycle" next to the Cancel button). Measured with a
/// token minted from this container's `JWT_SECRET`: `POST`/`PATCH /api/billing/subscription` -> 403
/// for the tenant's own `owner`, 200/422 for `agency_admin`, so the route itself is alive and only
/// the role gate refuses.
///
/// The intent question the card raised was "may the tenant's own owner/admin change its own
/// plan/cycle (the cancel arm's `is_tenant_billing_owner`), or is this operator-only?". Measured
/// against the app's own contracts, it is OPERATOR-only:
///
/// * these arms are not cycle-only. `PATCH` binds `plan_id` and `feature_overrides` as well, i.e.
///   a whole-column write of WHAT THE TENANT CAN ACCESS. The platform-gated overlay for exactly
///   that is `POST /api/admin/tenants/:id/overrides` (`module_registry::handlers::set_override`,
///   behind `require_platform_admin_middleware`), and the admin guide documents per-tenant
///   overrides as the ADMIN's instrument ("an explicit per-tenant entry in
///   `tenant_plans.feature_overrides` wins"). Handing `PATCH` to tenant `owner` would therefore
///   let any owner assign itself the most expensive plan and self-grant every feature;
/// * there is no payment or settlement path in this app to compensate: the checkout arm and its
///   provider webhooks were RETIRED 2026-09-25 (kanban t_0fe500d4), no payment provider exists in
///   the four `available_providers` integrations, and the retirement note states payment
///   collection "is a deliberate build rather than a wire-up". So "the tenant picks a paid plan"
///   would be a free elevation;
/// * that is the same shape — and the same answer — as David's own queue item WS-15 on
///   WorkflowSwift ("billing.enabled=false and no self-serve checkout ... restore a WORKING admin
///   plan-assignment path ... admin-only via the real platform-admin gate", proved with a
///   non-admin 403 and no plan row changed).
///
/// The authority is resolved the way this crate already resolves platform authority
/// (`auth::platform_admin`'s module doc: no role string can express it; it is the
/// `users.is_platform_admin` column resolved from the database by `claims.sub`, and it fails
/// closed for a missing row, a non-UUID subject or a NULL flag). `cancel_subscription` deliberately
/// keeps `is_tenant_billing_owner`: cancelling to Free is a tenant-scoped downshift and is a
/// different contract from assigning entitlements.
///
/// NOTE (out of scope here, carded): with this gate the operator can only write its OWN tenant
/// (`tid` comes from `Claims.aid`), and this app has no route that moves ANOTHER tenant onto a
/// plan — the WS-15 analogue for CoreSwift.
async fn require_subscription_operator(s: &AppState, c: &Claims) -> Result<(), AppError> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await
}

/// POST /api/billing/subscription — Create subscription
pub async fn create_subscription(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateSubscriptionRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    require_subscription_operator(&s, &c).await?;

    sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT id FROM plans WHERE id = $1 AND is_active = true",
    )
    .bind(r.plan_id)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Plan not found or inactive".to_string()))?;

    validate_billing_cycle(&r.billing_cycle)?;

    let count = count_or_zero(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM tenant_plans WHERE tenant_id = $1",
        )
        .bind(tid)
        .fetch_one(&s.db)
        .await?,
    );

    if count > 0 {
        return Err(AppError::Duplicate(
            "Account already has a subscription".to_string(),
        ));
    }

    let sub = sqlx::query_as::<_, TenantPlan>(
        r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle, trial_ends_at, current_period_starts_at, current_period_ends_at)
           VALUES ($1, $2, $3, 'trialing', $4, NOW() + INTERVAL '14 days', NOW(), NOW() + INTERVAL '1 month')
           RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(r.plan_id).bind(&r.billing_cycle)
    .fetch_one(&s.db).await?;

    crate::audit::logger::log_event(
        &s.db,
        tid,
        Some(uid),
        "subscription.created",
        "subscription",
        Some(sub.id),
        Some(json!({"plan_id": r.plan_id})),
        None,
    )
    .await;

    // Credit the referring affiliate if this is a paid-plan subscription.
    attribute_plan_upgrade(&s, tid, r.plan_id).await;

    Ok((StatusCode::CREATED, Json(json!(sub))))
}

/// PATCH /api/billing/subscription — Update subscription
pub async fn update_subscription(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<UpdateSubscriptionRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    require_subscription_operator(&s, &c).await?;

    // This arm binds the caller's value straight into the column, so it must validate it the
    // same way the create arm does; without this the DB CHECK answered a generic 500.
    if let Some(cycle) = r.billing_cycle.as_deref() {
        validate_billing_cycle(cycle)?;
    }

    let existing =
        sqlx::query_as::<_, TenantPlan>("SELECT * FROM tenant_plans WHERE tenant_id = $1")
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
           WHERE id = $4 RETURNING *"#,
    )
    .bind(r.plan_id)
    .bind(&r.billing_cycle)
    .bind(&r.feature_overrides)
    .bind(existing.id)
    .fetch_one(&s.db)
    .await?;

    crate::audit::logger::log_event(
        &s.db,
        tid,
        Some(uid),
        "subscription.updated",
        "subscription",
        Some(sub.id),
        Some(json!({"plan_id_old": existing.plan_id, "plan_id_new": sub.plan_id})),
        None,
    )
    .await;

    // Credit the referring affiliate if the plan changed to a paid plan.
    if r.plan_id.is_some() {
        attribute_plan_upgrade(&s, tid, sub.plan_id).await;
    }

    Ok(Json(json!(sub)))
}

/// PUT /api/admin/tenants/:id/plan — put ANOTHER tenant on a plan (platform admin only).
///
/// WHY THIS EXISTS (kanban t_f1ffb865 — the WS-15 analogue). Before it, NO surface in this app
/// could move a tenant onto a plan. Both writers above take the tenant id from `Claims.aid`, so
/// the operator could only ever write its own workspace (`Swift Admin's Workspace`), and
/// `POST /api/billing/subscription` additionally 409s for any tenant that already holds a row —
/// which every signup seat writes. `admin_actions::router` carried /tenants, /users, /site,
/// /email-config, /modules, /plans/:slug/{modules,features} and /tenants/:id/{overrides,
/// entitlements} and no plan write at all. Measured 2026-10-02: 3 of 14 tenants had a
/// `tenant_plans` row (all `free|active|monthly`, the signup seat's own INSERT) and the rest had
/// no row at all, so every tenant was stuck on its signup plan with no way off it but hand SQL.
///
/// AUTHORITY. The route is mounted on `admin_actions::router`, which layers
/// `require_platform_admin_middleware` over every protected route, and the call below is defence
/// in depth (the same predicate, the same `users.is_platform_admin` column resolved from the DB
/// by `claims.sub`, failing closed). No role string is consulted — `agency_admin` is a role a
/// minted user can carry (measured on t_8d91f70c), which is exactly why the app resolves
/// platform authority from the column instead.
///
/// WHAT IT DELIBERATELY DOES NOT DO.
/// * It never decides pricing or entitlements: no `plans` row, no `plan_modules` /
///   `plan_module_features` assignment and no `tenant_plans.feature_overrides` is written. The
///   per-tenant entitlement instrument stays the platform-gated
///   `POST /api/admin/tenants/:id/overrides`.
/// * It never writes `credit_balance` / `lifetime_credits` — credits are their own contract.
/// * A tenant holding no row is NOT invented: the INSERT arm seats the plan and nothing else, so
///   the resolver sees a real plan instead of the `no_plan` legacy tolerance (which allows every
///   gated module).
///
/// Two arms, one statement. `tenant_plans` carries a real UNIQUE CONSTRAINT on `tenant_id`
/// (`tenant_plans_tenant_id_key`), so `ON CONFLICT (tenant_id)` is a genuine arbiter here — unlike
/// the phantom `ON CONFLICT (aid)` that got WS-15's predecessor deleted. The same shape is what
/// `cancel_subscription` already writes. The UPDATE arm clears `trial_ends_at`: it is read
/// (`monitoring::account_health_handler` queues a "trial expiring" reminder, `ai::engine` reports
/// it), and an `active` row left carrying one would keep advertising a trial that no longer runs.
pub async fn assign_tenant_plan(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(tenant_id): Path<Uuid>,
    Json(r): Json<AssignPlanRequest>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    // The target tenant must exist. Checked before the plan so a typo'd id is a 404 and not a
    // confusing 422 about a slug that was fine.
    let tenant: Option<(Option<String>,)> =
        sqlx::query_as("SELECT name FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_optional(&s.db)
            .await?;
    let Some((tenant_name,)) = tenant else {
        return Err(AppError::NotFound(format!("Tenant {tenant_id} not found")));
    };

    // Resolve the requested plan through the app's own plan vocabulary. A slug is unique in
    // `plans`; both handles are validated against `is_active` so an assignment can never seat a
    // tenant on a retired tier.
    let slug = r
        .plan_slug
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let (plan_id, plan_slug, plan_name): (Uuid, String, String) = match (slug, r.plan_id) {
        (Some(slug), _) => {
            sqlx::query_as("SELECT id, slug, name FROM plans WHERE slug = $1 AND is_active = true")
                .bind(&slug)
                .fetch_optional(&s.db)
                .await?
                .ok_or_else(|| AppError::Validation(format!("No active plan with slug '{slug}'")))?
        }
        (None, Some(pid)) => {
            sqlx::query_as("SELECT id, slug, name FROM plans WHERE id = $1 AND is_active = true")
                .bind(pid)
                .fetch_optional(&s.db)
                .await?
                .ok_or_else(|| AppError::Validation(format!("No active plan with id '{pid}'")))?
        }
        (None, None) => {
            return Err(AppError::Validation(
                "plan_slug or plan_id is required".to_string(),
            ))
        }
    };

    // Cycle: an explicit value must be in the schema's vocabulary, otherwise an existing row
    // keeps its own cycle and a first-time assignment gets the first of the declared pair.
    let existing: Option<TenantPlan> =
        sqlx::query_as::<_, TenantPlan>("SELECT * FROM tenant_plans WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_optional(&s.db)
            .await?;
    let cycle: String = match r.billing_cycle.as_deref() {
        Some(cycle) => {
            validate_billing_cycle(cycle)?;
            cycle.to_string()
        }
        None => existing
            .as_ref()
            .map(|t| t.billing_cycle.clone())
            .unwrap_or_else(|| BILLING_CYCLES[0].to_string()),
    };

    let created_row = existing.is_none();

    let sub = sqlx::query_as::<_, TenantPlan>(
        r#"-- One statement, two arms. The conflict arbiter is the real unique constraint
           -- `tenant_plans_tenant_id_key` on (tenant_id); see the handler doc.
           INSERT INTO tenant_plans
               (id, tenant_id, plan_id, status, billing_cycle,
                current_period_starts_at, current_period_ends_at)
           VALUES ($1, $2, $3, 'active', $4, NOW(),
                   NOW() + CASE WHEN $4 = 'yearly' THEN INTERVAL '1 year' ELSE INTERVAL '1 month' END)
           ON CONFLICT (tenant_id) DO UPDATE SET
               plan_id = EXCLUDED.plan_id,
               status = 'active',
               billing_cycle = EXCLUDED.billing_cycle,
               canceled_at = NULL,
               trial_ends_at = NULL,
               current_period_starts_at = EXCLUDED.current_period_starts_at,
               current_period_ends_at = EXCLUDED.current_period_ends_at,
               updated_at = NOW()
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(plan_id)
    .bind(&cycle)
    .fetch_one(&s.db)
    .await?;

    crate::audit::logger::log_event(
        &s.db,
        tenant_id,
        Uuid::parse_str(&c.sub).ok(),
        "subscription.plan_assigned",
        "subscription",
        Some(sub.id),
        Some(json!({
            "plan_id_old": existing.as_ref().map(|t| t.plan_id),
            "plan_id_new": plan_id,
            "plan_slug": plan_slug,
            "billing_cycle": cycle,
            "created_row": created_row,
            "assigned_by": c.sub,
        })),
        None,
    )
    .await;

    // Same contract as the two writers above: a PAID assignment tells the referring affiliate.
    attribute_plan_upgrade(&s, tenant_id, plan_id).await;

    tracing::info!(
        tenant = %tenant_id,
        plan = %plan_slug,
        cycle = %cycle,
        created_row,
        "Platform operator moved a tenant onto a plan (t_f1ffb865)"
    );

    Ok(Json(json!({
        "message": "Plan assigned",
        "tenant_id": tenant_id,
        "tenant_name": tenant_name,
        "plan": { "id": plan_id, "slug": plan_slug, "name": plan_name },
        "billing_cycle": cycle,
        "created_row": created_row,
        "subscription": sub,
    })))
}

/// May this caller cancel the tenant's own subscription?
///
/// The gate previously tested `role != "client_admin" && role != "agency_admin"`, but NO
/// code path in this schema ever assigns `client_admin` — the tenant-admin role that is
/// actually written is `owner` (38 of 47 users; see `admin_actions` tenant creation).
/// The predicate was therefore unsatisfiable for every real customer: a tenant could not
/// cancel its own subscription at all. The roles admitted here are the schema's own
/// definition of tenant admin (`owner`/`admin`, the pair the tenant handlers already gate
/// on) plus the platform `agency_admin`, who may act on a tenant's behalf.
///
/// `member` and `impersonated` are deliberately excluded — the former is not an
/// administrative role, and an impersonation token is scoped to read a tenant, not to
/// change what it is billed for.
fn is_tenant_billing_owner(role: &str) -> bool {
    matches!(role, "owner" | "admin" | "agency_admin")
}

/// POST /api/billing/subscription/cancel — Cancel subscription (downgrades to free)
pub async fn cancel_subscription(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    if !is_tenant_billing_owner(&c.role) {
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
    if let Some(fp) =
        sqlx::query_scalar::<_, Option<Uuid>>("SELECT id FROM plans WHERE slug = 'free' LIMIT 1")
            .fetch_one(&s.db)
            .await?
    {
        let _ = sqlx::query(
            r#"INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle, current_period_starts_at, current_period_ends_at)
               VALUES ($1, $2, $3, 'active', $4, NOW(), NOW() + INTERVAL '100 years')
               ON CONFLICT (tenant_id) DO UPDATE SET plan_id = $3, status = 'active', billing_cycle = $4,
                                                     canceled_at = NULL, updated_at = NOW()"#
        )
        .bind(Uuid::new_v4()).bind(tid).bind(fp).bind(FREE_PLAN_BILLING_CYCLE)
        .execute(&s.db).await;
    }

    crate::audit::logger::log_event(
        &s.db,
        tid,
        Some(uid),
        "subscription.canceled",
        "subscription",
        Some(existing.id),
        None,
        None,
    )
    .await;

    Ok(Json(
        json!({"message": "Subscription canceled, downgraded to Free plan"}),
    ))
}

/// GET /api/billing/features — Get effective features for current tenant
pub async fn get_features(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
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

    let (
        plan_id,
        plan_slug,
        checkout_url,
        payment_provider,
        thank_you_url,
        plan_features,
        overrides,
    ) = match row {
        Some(r) => r,
        None => {
            return Err(AppError::NotFound(
                "No active subscription — assign a plan first".to_string(),
            ))
        }
    };

    let (features, limits) = merge_features(&plan_features, &overrides);

    Ok(Json(json!(FeaturesResponse {
        plan: PlanSummary {
            id: plan_id,
            name: plan_slug.clone(),
            slug: plan_slug,
            checkout_url,
            payment_provider,
            thank_you_url
        },
        features,
        limits,
    })))
}

/// GET /api/billing/credits/balance — Get available credit balance
pub async fn get_credit_balance(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // `get_credit_summary` now propagates a failed usage query instead of answering with a
    // zeroed summary, so this is a real error path, not decoration (t_5d7f823e).
    let summary = credits::get_credit_summary(&s.db, tid).await?;
    Ok(Json(summary))
}

/// GET /api/billing/credits/usage — Get detailed transaction history
pub async fn get_credit_usage(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(p): Query<Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(
        p.get("page").and_then(|v| v.as_i64()),
        p.get("per_page").and_then(|v| v.as_i64()),
    );
    let offset = (page - 1) * per_page;

    // description is NULLABLE with no default and NULL is real data: the served shell already
    // falls back to '' for it (`String(desc || '')`, www-app/coreswift/index.html), so the element
    // decodes as Option<String> exactly like contacts.email on t_b25a9002 (t_d6eeea96)
    let txns = sqlx::query_as::<_, (Uuid, String, i32, Option<String>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT id, action_type, credits, description, created_at FROM credit_transactions WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;

    let total = count_or_zero(
        sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM credit_transactions WHERE tenant_id = $1",
        )
        .bind(tid)
        .fetch_one(&s.db)
        .await?,
    );

    Ok(Json(
        json!({"transactions": txns, "total": total, "page": page, "per_page": per_page}),
    ))
}

// ──────────────────────────────────────────────
// Stripe / PayPal / Square / Paddle checkout — RETIRED (kanban t_0fe500d4, 2026-09-25)
// ──────────────────────────────────────────────
// POST /api/billing/checkout/create, its four provider helpers (create_{stripe,paypal,square,
// paddle}_session), the two public provider webhooks and the credential-delivery path were
// scaffolding from commit 439590f and are gone.  Measured live (evidence in
// /opt/swift/audits/t_0fe500d4/):
//
//   * no shipped surface ever called the route: no served shell (www, www-app, www-admin) contains
//     the path, and the workspace shell's upgrade modal rendered an <button> with no onclick;
//   * no entitlement was sold: there is no `modules` / `module_features` / `plan_module_features`
//     row for checkout, payments or billing (only `limits.limit_monthly_credits`), so the admin
//     console never offered it, and the four providers are absent from `available_providers` —
//     POST /api/provider-keys answered 422 "Unknown provider 'square'" for each of them, so no
//     tenant could ever configure the key the handler demands (422 "No active square API key
//     configured").  The only working caller needed a `provider_keys` row written BY HAND;
//   * three of the four arms were not implementations: paypal answered "PayPal checkout not yet
//     implemented", square/paddle minted local `sqr-…` / `pdl-…` ids with a URL
//     (/checkout/square/<uuid>) that no route serves — measured: it resolves to the sign-in shell —
//     and only stripe reached a provider API at all;
//   * nothing consumed the result: the single writer of `credit_transactions` binds a NEGATIVE
//     cost, so a completed session granted no credits, and plan changes happen through
//     POST /api/billing/subscription, which credits the referring affiliate through FunnelSwift
//     directly (`notify_funnelswift_upgrade`);
//   * live usage was zero: `checkout_sessions` held no row for any tenant and no tenant ever held
//     a checkout provider key.
//
// The two public receivers accepted ANY unsigned body — no signature, no shared secret — and on a
// session match would create a tenant + user and mail credentials for it.  That is the
// unverified-receiver class, and they existed only to complete a checkout session, so they are
// retired with the route: leaving them in place would have put a 42P01 back into the container log,
// the exact defect migration 096 had just closed.
//
// Migration 097 drops `checkout_sessions`, created by 096 for the INSERT these helpers owned.
// Payment collection, if wanted, is a deliberate build rather than a wire-up: seed the four
// `available_providers` rows, implement square/paddle (or answer 501 for them — never a minted URL
// that does not exist upstream), verify provider webhook signatures, grant the purchase (credits
// or `tenant_plans`), and give it a caller in the shell.
