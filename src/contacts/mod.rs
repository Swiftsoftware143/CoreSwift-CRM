//! Contacts management module.
//!
//! Full CRUD with tenant-scoped queries, search, and pagination.

pub mod handlers;
pub mod internal_handler;
pub mod models;

use uuid::Uuid;

/// Is `company_id` a company of `tenant_id`?
///
/// `contacts.company_id` carries the FK `contacts_company_id_fkey` (migration 087,
/// `ON DELETE SET NULL`). A foreign key proves a `companies` row with that id exists; it can not
/// prove the row belongs to the SAME tenant, so every writer of the column asks this first and
/// refuses the write instead of storing a link that resolves to another tenant's company (card
/// t_47698f73): the public `POST /api/contacts` and `PATCH /api/contacts/{id}` answer 404, the
/// two internal sync inserts answer 400.
///
/// Display precedence, because this is one half of a two-column pair and the pair has bitten
/// before: the free-text `contacts.company` is the employer the product renders and is the source
/// of truth (decision on t_fbb30c16). `company_id` is the machine-readable link — it is what
/// `src/webhook/actions.rs` emits in webhook payloads and what the contact score credits — and is
/// deliberately NOT resolved into the shell, so a contact whose `company` is empty shows no
/// employer even when `company_id` is set.
pub async fn company_of_tenant(
    db: &sqlx::PgPool,
    company_id: Uuid,
    tenant_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM companies WHERE id = $1 AND tenant_id = $2")
            .bind(company_id)
            .bind(tenant_id)
            .fetch_optional(db)
            .await?;
    Ok(found.is_some())
}

use crate::AppState;
use axum::{middleware, Router};

/// Build the contacts router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list))
        .route("/", axum::routing::post(handlers::create))
        .route("/search", axum::routing::get(handlers::search))
        .route("/:id", axum::routing::get(handlers::get))
        .route("/:id", axum::routing::patch(handlers::update))
        .route("/:id", axum::routing::delete(handlers::delete))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
