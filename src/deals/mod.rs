//! Deal Writer — the software-wide deal surface.
//!
//! David approved this on 2026-11-20 (kanban t_2df68850). The DEAL RECORD is the existing
//! `opportunities` table (pipeline + stage + value + history); this module is the one place that
//! writes it, and it adds what a writer and a full tab need on top: an external reference that
//! makes an outside system's re-send idempotent, owner + collaborators, per-workspace custom
//! fields, a timeline, follow-ups, attachments, stage hooks, currency captured at write time, and
//! the reporting the tab shows.
//!
//! Nothing industry-specific lives here. The courier fields the Xride account needs are written
//! by that account into `deal_custom_fields` — the same mechanism any other industry uses.

pub mod handlers;
pub mod models;
pub mod reporting;
pub mod writer;

use crate::AppState;
use axum::{
    middleware,
    routing::{delete, get, patch, post, put},
    Router,
};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // ── collection ──────────────────────────────────────────────────────────────────────
        .route("/", get(handlers::list))
        .route("/", post(handlers::create))
        .route("/report", get(handlers::report))
        .route("/team", get(handlers::team))
        // ── workspace currency (multi-currency is core) ─────────────────────────────────────
        .route("/currency", get(handlers::get_currency))
        .route("/currency", put(handlers::set_currency))
        // ── per-workspace custom field definitions (the industry layer) ─────────────────────
        .route("/custom-fields", get(handlers::list_fields))
        .route("/custom-fields", post(handlers::create_field))
        .route("/custom-fields/:id", patch(handlers::update_field))
        .route("/custom-fields/:id", delete(handlers::delete_field))
        // ── configurable lost-reason list ───────────────────────────────────────────────────
        .route("/lost-reasons", get(handlers::list_lost_reasons))
        .route("/lost-reasons", post(handlers::create_lost_reason))
        .route("/lost-reasons/:id", patch(handlers::update_lost_reason))
        .route("/lost-reasons/:id", delete(handlers::delete_lost_reason))
        // ── stage automation hooks ──────────────────────────────────────────────────────────
        .route("/stage-automations", get(handlers::list_stage_automations))
        .route(
            "/stage-automations",
            post(handlers::create_stage_automation),
        )
        .route(
            "/stage-automations/:id",
            delete(handlers::delete_stage_automation),
        )
        // ── one deal ────────────────────────────────────────────────────────────────────────
        .route("/:id", get(handlers::get))
        .route("/:id", patch(handlers::update))
        .route("/:id", delete(handlers::delete))
        .route("/:id/stage", post(handlers::move_deal))
        .route("/:id/status", post(handlers::set_status))
        .route("/:id/activities", post(handlers::add_activity))
        .route("/:id/tasks", post(handlers::add_task))
        .route("/:id/tasks/:task_id", patch(handlers::update_task))
        .route("/:id/tasks/:task_id", delete(handlers::delete_task))
        .route("/:id/attachments", post(handlers::add_attachment))
        .route(
            "/:id/attachments/:att_id",
            get(handlers::download_attachment),
        )
        .route(
            "/:id/attachments/:att_id",
            delete(handlers::delete_attachment),
        )
        // A deal file may be up to 6 MiB decoded; the base64 envelope plus JSON framing needs
        // room above axum's 2 MB default, so the limit is stated here rather than discovered as
        // a confusing 413 on the first upload.
        .layer(axum::extract::DefaultBodyLimit::max(9 * 1024 * 1024))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
