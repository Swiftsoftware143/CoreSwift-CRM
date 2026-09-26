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
