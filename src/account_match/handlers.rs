//! Account matching — HTTP surface.
//!
//! Two routes, both tenant-scoped from the session's `Claims::aid` and both behind the module's own
//! plan flag (`crate::features::gate_mw`, key `account_match`) and the app's default-deny credential
//! boundary (`crate::auth::route_policy` — these paths are in no allowlist, so an anonymous caller is
//! refused before the handler runs):
//!
//! * `POST /api/account-match/resolve`    — which existing contact/company is this identity?
//! * `GET  /api/account-match/duplicates` — which of my rows look like duplicates of each other?
//!
//! Neither route can name a tenant: the tenant is read from the session and bound into every
//! statement (`account_match::engine`).

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::engine::{self, Identity};
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct DuplicatesQuery {
    /// Clamped to `1..=500` by the engine; defaults to 100.
    pub limit: Option<i64>,
}

/// POST /api/account-match/resolve
///
/// Body: `{ "email"?, "phone"?, "first_name"?, "last_name"?, "company"? }`.
///
/// A body carrying no usable identifier is a 422 rather than a confident `matched: false`: "nothing
/// was asked" and "nobody matched" must not be the same answer, or a caller that forgets a field
/// reads a silent non-match as "this person is new".
pub async fn resolve(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(identity): Json<Identity>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if !identity.has_identifier() {
        return Err(AppError::Validation(
            "Provide at least one identifier: email, or phone, or first_name and last_name."
                .to_string(),
        ));
    }

    let matched = engine::resolve(&state.db, tenant_id, &identity).await?;
    Ok(Json(matched))
}

/// GET /api/account-match/duplicates?limit=100
///
/// The pipeline-cleanliness report for the caller's own tenant: contacts sharing a normalized phone
/// number, and contacts sharing a normalized name. Read-only; nothing is merged here.
pub async fn duplicates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<DuplicatesQuery>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let groups = engine::duplicate_groups(&state.db, tenant_id, params.limit).await?;
    let contacts_in_groups: i64 = groups.iter().map(|g| g.count).sum();

    Ok(Json(json!({
        "groups": groups,
        "total_groups": groups.len(),
        "contacts_in_groups": contacts_in_groups,
    })))
}
