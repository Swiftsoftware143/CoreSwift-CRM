pub mod models;
pub mod handlers;
pub mod engine;

use axum::{Router, middleware};
use crate::AppState;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/teams", axum::routing::get(handlers::list_teams).post(handlers::create_team))
        .route("/teams/:id", axum::routing::get(handlers::get_team).patch(handlers::update_team).delete(handlers::delete_team))
        .route("/teams/:id/members", axum::routing::get(handlers::list_members).post(handlers::add_member))
        .route("/teams/:id/members/:member_id", axum::routing::delete(handlers::remove_member))
        .route("/teams/:id/assignments", axum::routing::get(handlers::list_assignments))
        .route("/assign", axum::routing::post(handlers::trigger_assignment))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
