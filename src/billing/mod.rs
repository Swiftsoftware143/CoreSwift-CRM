//! Billing module — plan tiers, tenant subscriptions, and feature toggles.
//!
//! Provides CRUD for plan definitions, subscription management per tenant,
//! and a computed features endpoint that merges plan defaults with tenant overrides.

pub mod credits;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

/// Build the billing router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/plans", axum::routing::get(handlers::list_plans))
        .route("/plans", axum::routing::post(handlers::create_plan))
        // Feature toggles the admin can set per plan — the admin UI reads its switches
        // from here. NOTE: the `plans` module (src/plans/) defines its own router with
        // this route, but that router is NOT nested in main.rs, so the module is dead
        // code; this mounted path is the live one.
        .route(
            "/plans/registry",
            axum::routing::get(crate::plans::handlers::feature_registry),
        )
        .route("/plans/:id", axum::routing::get(handlers::get_plan))
        .route("/plans/:id", axum::routing::patch(handlers::update_plan))
        .route("/plans/:id", axum::routing::delete(handlers::delete_plan))
        .route(
            "/subscription",
            axum::routing::get(handlers::get_subscription),
        )
        .route(
            "/subscription",
            axum::routing::post(handlers::create_subscription),
        )
        .route(
            "/subscription",
            axum::routing::patch(handlers::update_subscription),
        )
        .route(
            "/subscription/cancel",
            axum::routing::post(handlers::cancel_subscription),
        )
        .route("/features", axum::routing::get(handlers::get_features))
        // Credit-based billing
        .route(
            "/credits/balance",
            axum::routing::get(handlers::get_credit_balance),
        )
        .route(
            "/credits/usage",
            axum::routing::get(handlers::get_credit_usage),
        )
        // NOTE: POST /credits/buy was removed (2026-09-21). It inserted a 'credit_purchase' row for
        // the caller's tenant with no payment, no role check and no provider session — i.e. any
        // authenticated user could mint credits.
        //
        // NOTE: POST /checkout/create — and with it the Stripe/PayPal/Square/Paddle session
        // helpers and the two public provider webhooks — was RETIRED (2026-09-25, kanban
        // t_0fe500d4) as unfinished scaffolding: no shipped surface called it, no plan could gate
        // it, the four providers were absent from `available_providers` (so no tenant could ever
        // configure the key it demands), paypal was unimplemented and square/paddle returned a URL
        // with no implementation behind it. There is therefore no public purchase route in this
        // module today; plan changes go through POST/PATCH /subscription. See the retirement note
        // in handlers.rs and /opt/swift/audits/t_0fe500d4/ for the measurements and for what a
        // deliberate payment build would have to cover.
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

// ────────────────────────────────────────────────────────────────────────────────────────────────
// THE PLATFORM DEFAULT, SEATED AT CREATION (kanban t_6f225dd4)
//
// `module_registry::resolve` treats a tenant with no active `tenant_plans` row as `no_plan`:
// EVERY registered module granted and NO numeric ceiling (`features::usage_ceiling` returns
// `None`). That tolerance is deliberate and is named in the console + admin guide (kanban
// t_e6141896, ARM (a)), but it must not be something a creation path can *produce* — otherwise a
// mint silently manufactures an unlimited workspace, and the population grows.
//
// Measured 2026-10-02 on coreswift_crm: 9 sites `INSERT INTO tenants`; four of them (the public
// signup's two arms, the admin chat action `tenants.create`, and the automation-webhook hub
// `tenants.create`) already seat `free`, and five did not — the two cross-app ingest sinks
// (`tag_provision_handler::handle_tag_provision`, `webhooks::cross_app_tag_sync::handle_tag_sync`),
// the portfolio internal sync, `POST /api/account`, and `admin_actions::cross_app_sync`. All five
// now call this. 14 of 17 live tenants were row-less BECAUSE those paths minted them.
//
// The rule is the signup rule, unchanged: the workspace gets the platform default (`free`), in the
// SAME TRANSACTION as the tenant INSERT, so a mint either lands with a plan or does not land. It
// decides no pricing — `free` is already what every self-serve signup gets, and moving a workspace
// to another tier stays the operator's instrument
// (`billing::handlers::assign_tenant_plan`, PUT /api/admin/tenants/:id/plan).
//
// It cannot drop a captured lead. Every ingest arm that must never refuse a capture is deliberately
// ungated (kanban t_e2364c41): `external_api`'s spoke ingest carries no `limit_max_contacts` guard,
// and both mint paths above write contacts with their own `INSERT INTO contacts` that consults no
// ceiling at all — so a sink seated on `free` (ceiling 100) keeps capturing past its ceiling, and
// `GET /api/auth/me/usage` reports `contacts_over_limit` instead of the lead vanishing. Proven live
// on both arms (audits/t_6f225dd4, leg 6: 120 seeded + 1 captured = 121 on a `free`-seated sink).

/// The platform default plan a freshly created workspace is seated on.
pub const DEFAULT_PLAN_SLUG: &str = "free";

/// Seat the platform default plan on a tenant that was JUST created, on the caller's connection —
/// pass `&mut *tx` from inside the same transaction as the `INSERT INTO tenants`.
///
/// Fails (rolling the caller's transaction back, so no row-less tenant can commit) when the
/// platform default is not configured. The statement is the one the signup seat writes
/// (`auth::handlers::resolve_account`): `status='active'`, `billing_cycle='monthly'`, and
/// `ON CONFLICT (tenant_id) DO NOTHING` — `tenant_plans_tenant_id_key` is a real UNIQUE constraint,
/// so the arbiter is genuine and an existing row is never overwritten.
pub async fn seat_default_plan(
    conn: &mut sqlx::PgConnection,
    tenant_id: uuid::Uuid,
) -> Result<(), crate::errors::AppError> {
    let plan_id: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM plans WHERE slug = $1 AND is_active = true LIMIT 1")
            .bind(DEFAULT_PLAN_SLUG)
            .fetch_optional(&mut *conn)
            .await?;

    let Some(plan_id) = plan_id else {
        return Err(crate::errors::AppError::BadRequest(format!(
            "Platform default plan '{DEFAULT_PLAN_SLUG}' is not configured"
        )));
    };

    sqlx::query(
        r#"INSERT INTO tenant_plans (tenant_id, plan_id, status, billing_cycle)
           VALUES ($1, $2, 'active', 'monthly')
           ON CONFLICT (tenant_id) DO NOTHING"#,
    )
    .bind(tenant_id)
    .bind(plan_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}
