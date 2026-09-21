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
        // NOTE: POST /credits/buy was removed. It inserted a 'credit_purchase' row for the
        // caller's tenant with no payment, no role check and no provider session — i.e. any
        // authenticated user could mint credits. Real purchases go through /checkout/create
        // (provider session) + the provider webhooks below.
        // Stripe/PayPal checkout
        .route(
            "/checkout/create",
            axum::routing::post(handlers::create_checkout_session),
        )
        // NOTE: GET /checkout/sessions was removed 2026-09-21 (dead-endpoint triage
        // t_14f5514f). It queried a `checkout_sessions` table that does not exist, so every
        // call was a 500, and no shipped surface called it. See billing/handlers.rs.
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
