//! Internal tenant operations — no JWT, validated by `x-internal-key`.
//!
//! `POST /lookup`   — resolve a workspace by slug (the cross-app read path).
//! `DELETE /:id`    — DEPROVISION a workspace (kanban t_c88f907f): the door Multi-Directory calls
//!                    when the directory/network that provisioned it is deleted.
//!
//! Both routes are named in `auth::route_policy::INTERNAL_ROUTES`, so the shared key is demanded
//! at the credential boundary as well as here.
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::tenant_purge::{self, Door};
use crate::AppState;

/// The shared-key check both handlers share. Fail closed: `config.rs` defaults
/// `INTERNAL_SYNC_KEY` to `""`, and an unset key would otherwise authenticate an empty header.
fn require_internal_key(s: &AppState, headers: &HeaderMap) -> ApiResult<()> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = s.config.internal_sync_key.as_str();
    if expected.is_empty() || key != expected {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

pub async fn internal_lookup_tenant(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    require_internal_key(&s, &headers)?;

    let slug = req
        .get("slug")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if slug.is_empty() {
        return Err(AppError::BadRequest("slug is required".into()));
    }

    let tenant = sqlx::query_as::<_, (Uuid, String, String)>(
        "SELECT id, name, slug FROM tenants WHERE slug = $1",
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?;

    match tenant {
        Some((id, name, slug)) => Ok(Json(json!({
            "id": id.to_string(),
            "name": name,
            "slug": slug,
        }))),
        None => Err(AppError::NotFound(format!(
            "Tenant with slug '{slug}' not found"
        ))),
    }
}

/// `?slug=` is the caller's confirmation of WHICH workspace it means. Required.
#[derive(Debug, serde::Deserialize)]
pub struct DeleteTenantQuery {
    pub slug: Option<String>,
}

/// `DELETE /api/internal/tenants/:id?slug=<slug>`
///
/// Deprovisions the workspace: the tenant row and everything its 70 cascading foreign keys reach
/// (users, contacts, lists, tags, pipelines, messages, phone numbers, bookings, plan rows, audit
/// rows) are destroyed in ONE statement, and the caller is told what went with it.
///
/// Answer 200:
/// `{"status":"deleted","mode":"hard","tenant_id":"…","name":"…","slug":"…",
///   "users_removed":1,"contacts_removed":0,"lists_removed":3}`
///
/// Refusals (all `{"error":true,"message":"…","code":N}`, the message naming the reason):
///   401 — missing/wrong `x-internal-key`
///   404 — no workspace with that id
///   409 — `slug` missing or not that workspace's slug
///   409 — a portfolio/owner workspace, or one no fleet-harness user signs in to
///   409 — a REAL user signs in to it (their CRM would go with it)
///
/// The shared key can therefore never destroy a customer's or the owner's workspace: it only
/// reaches a machine-provisioned mirror, and only when the caller names it and it is empty of
/// human logins.
pub async fn internal_delete_tenant(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteTenantQuery>,
) -> ApiResult<impl IntoResponse> {
    require_internal_key(&s, &headers)?;
    let report = tenant_purge::delete_tenant(&s.db, id, Door::Fleet, q.slug.as_deref()).await?;
    Ok(Json(report.to_json()))
}

pub fn router() -> axum::Router<AppState> {
    use axum::routing::{delete, post};
    axum::Router::new()
        .route("/lookup", post(internal_lookup_tenant))
        .route("/:id", delete(internal_delete_tenant))
}
