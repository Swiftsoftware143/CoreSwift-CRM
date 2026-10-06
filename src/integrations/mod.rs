pub mod handlers;
pub mod models;
pub mod n8n;
pub mod webhook;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    // The tenant-facing WEBHOOK CONFIG surface (`/api/integrations/webhooks` — outbound webhook
    // endpoints: name, url, signing secret, events) is its own module in the plan catalogue:
    // `webhooks`, sold on Enterprise only (David's Phase-3 table, kanban t_9b2e0c3e). It used to be
    // gated by `integrations` alone, which every plan has, so nothing about it was Enterprise-only.
    //
    // What is NOT here, on purpose:
    //   * `POST /api/webhook/:token/:action` (src/webhook) — the public automation webhook. It is
    //     TOKEN-authenticated by design: the caller is n8n / Hermes, not a signed-in tenant, and it
    //     has to keep working for a workspace whatever its plan (the token is the credential, and it
    //     is revocable per tenant). Gating it would silently break every existing automation.
    //   * `/api/scoring/webhooks` — scoring's own webhook targets, already gated by `ai_enabled`
    //     (Pro and up). Requiring `webhooks` there too would take a Pro feature away from Pro.
    let webhook_config = Router::new()
        .route("/webhooks", axum::routing::get(handlers::list_webhooks))
        .route("/webhooks", axum::routing::post(handlers::create_webhook))
        .route(
            "/webhooks/:id",
            axum::routing::patch(handlers::update_webhook),
        )
        .route(
            "/webhooks/:id",
            axum::routing::delete(handlers::delete_webhook),
        )
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "webhooks", "Outbound webhooks"),
            crate::features::gate_mw,
        ));

    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .route(
            "/:integration_id/mappings",
            axum::routing::get(handlers::list_mappings),
        )
        .route(
            "/:integration_id/mappings",
            axum::routing::post(handlers::create_mapping),
        )
        .route(
            "/mappings/:id",
            axum::routing::delete(handlers::delete_mapping),
        )
        .merge(webhook_config)
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "integrations", "Integrations"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
