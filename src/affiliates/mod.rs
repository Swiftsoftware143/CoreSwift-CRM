pub mod models;
pub mod handlers;

use axum::{Router, middleware};
use crate::AppState;

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
        .route("/products/tags", axum::routing::get(handlers::products_by_tag))
        .route("/products/:id", axum::routing::patch(handlers::update_product))
        .route("/products/:id", axum::routing::delete(handlers::delete_product))
        // Affiliate self-serve product selection (affiliates pick what to promote)
        .route("/my-products", axum::routing::get(handlers::list_my_products))
        .route("/my-products/select", axum::routing::post(handlers::select_product))
        .route("/my-products/unselect", axum::routing::post(handlers::unselect_product))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
