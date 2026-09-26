pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/profile", axum::routing::get(handlers::get_profile))
        .route("/profile", axum::routing::post(handlers::create_profile))
        .route("/profile", axum::routing::patch(handlers::update_profile))
        .route("/referrals", axum::routing::get(handlers::list_referrals))
        .route("/payouts", axum::routing::get(handlers::list_payouts))
        .route("/stats", axum::routing::get(handlers::get_stats))
        .route("/redeem/:code", axum::routing::post(handlers::redeem_code))
        // Affiliate product board (admin)
        .route("/products", axum::routing::get(handlers::list_products))
        .route("/products", axum::routing::post(handlers::create_product))
        .route(
            "/products/tags",
            axum::routing::get(handlers::products_by_tag),
        )
        .route(
            "/products/:id",
            axum::routing::patch(handlers::update_product),
        )
        .route(
            "/products/:id",
            axum::routing::delete(handlers::delete_product),
        )
        // Affiliate self-serve product selection (affiliates pick what to promote)
        .route(
            "/my-products",
            axum::routing::get(handlers::list_my_products),
        )
        .route(
            "/my-products/select",
            axum::routing::post(handlers::select_product),
        )
        .route(
            "/my-products/unselect",
            axum::routing::post(handlers::unselect_product),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "affiliates", "Affiliate system"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
