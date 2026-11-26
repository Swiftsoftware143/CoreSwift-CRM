pub mod handlers;
pub mod internal_handler;
pub mod models;
pub mod triggers;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/categories", axum::routing::get(handlers::list_categories))
        .route(
            "/categories",
            axum::routing::post(handlers::create_category),
        )
        .route(
            "/categories/:id",
            axum::routing::patch(handlers::update_category),
        )
        .route(
            "/categories/:id",
            axum::routing::delete(handlers::delete_category),
        )
        .route("/", axum::routing::get(handlers::list_tags))
        .route("/", axum::routing::post(handlers::create_tag))
        .route("/:id", axum::routing::get(handlers::get_tag))
        .route("/:id", axum::routing::patch(handlers::update_tag))
        .route("/:id", axum::routing::delete(handlers::delete_tag))
        .route("/assign", axum::routing::post(handlers::assign_tag))
        .route("/assign/:id", axum::routing::delete(handlers::unassign_tag))
        .route(
            "/entity/:entity_type/:entity_id",
            axum::routing::get(handlers::get_entity_tags),
        )
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// Apply a tag to one record BY NAME, idempotently, with the module's own fan-out.
///
/// Written for the booking path (ZaarHub CEO, 2026-11-25): a booking creates a CRM contact and must land on
/// the "‹City› - Sponsors" list, which is a dynamic list whose rule is that same tag. Doing it with a raw
/// INSERT would put the contact on the list only when somebody pressed Evaluate, because the list refresh
/// and the TagAdded automations hang off the fan-out below.
///
/// Refuses quietly (returns Ok(None)) when the account has no such tag: a booking must never fail because
/// a calendar names a tag that was renamed or deleted, and inventing the tag here would fork the vocabulary
/// the operator maintains by hand.
pub async fn apply_by_name(
    db: &sqlx::PgPool,
    tenant_id: uuid::Uuid,
    entity_type: &str,
    entity_id: uuid::Uuid,
    name: &str,
    actor: Option<uuid::Uuid>,
) -> Result<Option<uuid::Uuid>, crate::errors::AppError> {
    let tag_name = name.trim();
    if tag_name.is_empty() {
        return Ok(None);
    }
    let tag_id: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT id FROM tags WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(tag_name)
    .fetch_optional(db)
    .await?;
    let Some(tag_id) = tag_id else {
        tracing::warn!(tenant = %tenant_id, tag = %tag_name,
            "booking named a tag this account does not have; no tag applied");
        return Ok(None);
    };
    sqlx::query(
        "INSERT INTO tag_assignments(id, tag_id, entity_type, entity_id, tenant_id, assigned_by)
         VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
    )
    .bind(uuid::Uuid::new_v4())
    .bind(tag_id)
    .bind(entity_type)
    .bind(entity_id)
    .bind(tenant_id)
    .bind(actor)
    .execute(db)
    .await?;
    // The fan-out: TagAdded automations, then the dynamic lists (a list whose rule is this tag must gain the
    // member now, not when somebody presses Evaluate).
    crate::automation::engine::fire_tag_trigger(
        db,
        tenant_id,
        entity_type,
        entity_id,
        tag_id,
        "TagAdded",
    )
    .await;
    crate::lists::evaluator::refresh_dynamic_lists(db, tenant_id).await;
    Ok(Some(tag_id))
}

/// Apply a tag by name, CREATING it when the account does not have it yet.
///
/// `apply_by_name` refuses a name it cannot find, which is right for a hand-typed name on a booking form
/// (a typo should not mint a tag) but wrong for a CAPTURE TEMPLATE: `{product} lead` expands to
/// "Full Takeover lead", and an owner cannot pre-create every product-by-temperature combination that
/// will ever be sold. The CRM convention is that assigning an unknown tag creates it, and a template
/// that silently does nothing is the failure David reported: "they will get tagged with the product they
/// purchase" — which cannot work if the tag has to exist first.
///
/// A tag created this way has no category, so it cannot join an exclusive group by accident.
pub async fn apply_or_create_by_name(
    db: &sqlx::PgPool,
    tenant_id: uuid::Uuid,
    entity_type: &str,
    entity_id: uuid::Uuid,
    name: &str,
    actor: Option<uuid::Uuid>,
) -> Result<Option<uuid::Uuid>, crate::errors::AppError> {
    let tag_name = name.trim();
    if tag_name.is_empty() {
        return Ok(None);
    }
    let existing: Option<uuid::Uuid> = sqlx::query_scalar(
        "SELECT id FROM tags WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(tag_name)
    .fetch_optional(db)
    .await?;
    if existing.is_none() {
        sqlx::query(
            "INSERT INTO tags (id, tenant_id, name) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(tenant_id)
        .bind(tag_name)
        .execute(db)
        .await?;
    }
    apply_by_name(db, tenant_id, entity_type, entity_id, tag_name, actor).await
}
