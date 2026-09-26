pub mod engine;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/teams",
            axum::routing::get(handlers::list_teams).post(handlers::create_team),
        )
        .route(
            "/teams/:id",
            axum::routing::get(handlers::get_team)
                .patch(handlers::update_team)
                .delete(handlers::delete_team),
        )
        .route(
            "/teams/:id/members",
            axum::routing::get(handlers::list_members).post(handlers::add_member),
        )
        .route(
            "/teams/:id/members/:member_id",
            axum::routing::delete(handlers::remove_member),
        )
        .route(
            "/teams/:id/assignments",
            axum::routing::get(handlers::list_assignments),
        )
        .route("/assign", axum::routing::post(handlers::trigger_assignment))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "round_robin",
                "Round-robin routing",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
