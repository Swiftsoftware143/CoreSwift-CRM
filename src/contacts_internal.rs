//! Internal contact creation — no JWT, validated by x-internal-key
use axum::response::IntoResponse;
use axum::{extract::State, http::HeaderMap, Json};
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn internal_create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = s.config.internal_sync_key.clone();
    // config.rs defaults INTERNAL_SYNC_KEY to "", and an unset key would then authenticate an
    // empty x-internal-key header. Refuse when this server has no key configured (fail closed).
    if expected.is_empty() || key != expected {
        return Err(AppError::Unauthorized);
    }

    let tenant_id = req
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tenant_id required".into()))?;

    let first_name = req
        .get("first_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let last_name = req
        .get("last_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let email = req
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let phone = req
        .get("phone")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let company_id = req
        .get("company_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok());
    let notes = req
        .get("notes")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let title = req
        .get("title")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    if first_name.is_empty() {
        return Err(AppError::BadRequest("first_name is required".into()));
    }

    // t_47698f73: a reference that resolves to nothing is no longer storable. The FK column
    // (migration 087) rejects a uuid that names no company at all; it cannot see tenancy, so this
    // rejects a company belonging to another tenant. Here (internal sync, no JWT) the answer is a
    // 400 naming the field instead of a 500 raised by the foreign key.
    if let Some(cid) = company_id {
        if !crate::contacts::company_of_tenant(&s.db, cid, tenant_id).await? {
            return Err(AppError::BadRequest(format!(
                "company_id {} is not a company of tenant {}",
                cid, tenant_id
            )));
        }
    }

    // Get-or-create, so a repeat or a concurrent duplicate is not a 500. idx_contacts_tenant_email
    // (partial, WHERE email IS NOT NULL) makes (tenant_id, email) unique, and this route's caller
    // (multi-directory) re-sends the same claimed-business contact; a bare INSERT raised 23505, the
    // `?` mapped AppError::Database to a 500, and the caller — which requires 2xx before it adds the
    // list membership — dropped the rest of its push. DO NOTHING absorbs the duplicate and the
    // re-select returns the row that already owns the key (kanban t_2dfaffa4's rule, applied to the
    // one contacts get-or-create that fix did not cover). A NULL email is outside the partial index,
    // so this conflict target is still inferable and an all-NULL insert always lands.
    let candidate = Uuid::new_v4();
    let created: Option<(Uuid,)> = sqlx::query_as(
        r#"INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, company_id, notes, title)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           ON CONFLICT (tenant_id, email) WHERE email IS NOT NULL DO NOTHING
           RETURNING id"#,
    )
    .bind(candidate)
    .bind(tenant_id)
    .bind(&first_name)
    .bind(&last_name)
    .bind(&email)
    .bind(&phone)
    .bind(company_id)
    .bind(&notes)
    .bind(&title)
    .fetch_optional(&s.db)
    .await?;

    let id = match created {
        Some((id,)) => id,
        None => {
            // Already owned by an existing row (or lost the race): hand back the winner's id so
            // the caller's follow-up (list membership + tag) works exactly as on the create path.
            let existing: (Uuid,) = sqlx::query_as(
                "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
            )
            .bind(tenant_id)
            .bind(&email)
            .fetch_one(&s.db)
            .await?;
            existing.0
        }
    };

    Ok(Json(
        serde_json::json!({"id": id.to_string(), "first_name": first_name, "last_name": last_name}),
    ))
}

/// Router for internal contact endpoints (no auth middleware)
pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new().route("/", post(internal_create))
}
