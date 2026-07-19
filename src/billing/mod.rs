//! Billing module — plan tiers, tenant subscriptions, and feature toggles.
//!
//! Provides CRUD for plan definitions, subscription management per tenant,
//! and a computed features endpoint that merges plan defaults with tenant overrides.

pub mod models;
pub mod handlers;
pub mod credits;

use axum::{Router, middleware};
use crate::AppState;

/// Build the billing router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/plans", axum::routing::get(handlers::list_plans))
        .route("/plans", axum::routing::post(handlers::create_plan))
        .route("/plans/:id", axum::routing::get(handlers::get_plan))
        .route("/plans/:id", axum::routing::patch(handlers::update_plan))
        .route("/plans/:id", axum::routing::delete(handlers::delete_plan))
        .route("/subscription", axum::routing::get(handlers::get_subscription))
        .route("/subscription", axum::routing::post(handlers::create_subscription))
        .route("/subscription", axum::routing::patch(handlers::update_subscription))
        .route("/subscription/cancel", axum::routing::post(handlers::cancel_subscription))
        .route("/features", axum::routing::get(handlers::get_features))
        // Credit-based billing
        .route("/credits/balance", axum::routing::get(handlers::get_credit_balance))
        .route("/credits/usage", axum::routing::get(handlers::get_credit_usage))
        .route("/credits/buy", axum::routing::post(handlers::buy_credits))
        // Stripe/PayPal checkout
        .route("/checkout/create", axum::routing::post(handlers::create_checkout_session))
        .route("/checkout/sessions", axum::routing::get(handlers::list_checkout_sessions))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
