//! Onboarding checklists module.
//!
//! Staged checklist templates and per-entity progress tracking.
//! Checklists are triggered by events (signup, payment, contact creation)
//! and walk users through onboarding steps with delayed follow-ups.

pub mod engine;
pub mod handlers;
pub mod models;

use crate::AppState;
use axum::{middleware, Router};
use sqlx::PgPool;
use uuid::Uuid;

/// Build the checklists router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/templates", axum::routing::get(handlers::list_templates))
        .route("/templates", axum::routing::post(handlers::create_template))
        .route("/templates/:id", axum::routing::get(handlers::get_template))
        .route(
            "/templates/:id",
            axum::routing::patch(handlers::update_template),
        )
        .route(
            "/templates/:id",
            axum::routing::delete(handlers::delete_template),
        )
        .route("/instances", axum::routing::get(handlers::list_instances))
        .route(
            "/instances/start/:entity_type/:entity_id",
            axum::routing::post(handlers::start_checklist),
        )
        .route(
            "/instances/:id/progress",
            axum::routing::patch(handlers::update_progress),
        )
        .route("/instances/:id", axum::routing::get(handlers::get_instance))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "checklists", "Checklists"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// Outcome of resolving a checklist instance's target entity against the table that owns it.
///
/// There is ONE predicate for this because there is more than one door into `checklist_instances`:
/// the HTTP start route AND the event-driven engine (`/api/events/ingest/:source` reaches
/// `engine::trigger_checklist` with a caller-supplied `entity_type`/`entity_id`). A rule enforced
/// on one door must not leak through the other (t_e73ea76a).
pub(crate) enum EntityLookup {
    /// The row exists in this tenant.
    Found,
    /// `entity_type` is one this app stores, but no such row belongs to this tenant.
    Missing,
    /// `entity_type` is not part of this app's entity vocabulary.
    Unknown,
}

/// Resolve `entity_id` against the table that owns `entity_type`, scoped to `tenant_id`.
///
/// A row that belongs to ANOTHER tenant answers `Missing` — never a refusal that would confirm it
/// exists elsewhere. The SQL is literal per arm: an identifier cannot be a bind parameter, and the
/// pre-build gate (rule 5d) refuses a statement built at run time.
pub(crate) async fn lookup_entity(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
) -> Result<EntityLookup, sqlx::Error> {
    let exists: bool = match entity_type {
        "contact" => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM contacts WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_one(db)
            .await?
        }
        "company" => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM companies WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_one(db)
            .await?
        }
        "opportunity" => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM opportunities WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_one(db)
            .await?
        }
        "list" => {
            sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM lists WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_one(db)
            .await?
        }
        "tag" => {
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tags WHERE id = $1 AND tenant_id = $2)")
                .bind(entity_id)
                .bind(tenant_id)
                .fetch_one(db)
                .await?
        }
        _ => return Ok(EntityLookup::Unknown),
    };

    Ok(if exists {
        EntityLookup::Found
    } else {
        EntityLookup::Missing
    })
}
