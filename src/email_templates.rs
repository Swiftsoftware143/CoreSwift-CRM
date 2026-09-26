//! Email Templates handler — CRUD for the PLATFORM email-template registry.
//!
//! `email_templates` is a PLATFORM asset, not a tenant one. Its rows are the transactional
//! templates the platform itself sends (welcome / password_reset / purchase_confirmed), `create`
//! below can only ever write a platform row (`aid`/`tenant_id` NULL), and the tenant-facing
//! template store is a different module entirely: `/api/comms/templates` over `message_templates`,
//! which is what the tenant SPA's "Email Templates" tab calls.
//!
//! Authority therefore comes from `users.is_platform_admin`, resolved from the database by
//! `claims.sub` (see [`crate::auth::platform_admin`]), never from `Claims.role`: `role` is
//! ACCOUNT-scoped and every registration path writes `owner` for the first user of a tenant, so
//! accepting it here let any of the 38 production `owner` rows read and mutate the platform's
//! registry — measured live 2026-09-22, a tenant `owner` reached all five handlers including
//! `DELETE`, which could have removed the platform's only welcome template for every tenant.
//!
//! The row shape below follows the LIVE table, which carries `tenant_id` and `body_text` and has
//! NO `is_html` column. Two consequences, both fixed here:
//!   * `is_html` is DERIVED in the JSON payload (`html_body IS NOT NULL`) so the published admin
//!     guide, which documents the field, stays true — no schema change is needed to honour it;
//!   * the previous shape could not decode a live row at all (a missing `is_html` column and a
//!     NULL `aid` against a non-`Option` field), so every list answered `{"count":1,"items":[]}`
//!     and every write answered 500, because the decode error was swallowed by
//!     `unwrap_or_default()`. Queries now propagate their errors and the column list is explicit,
//!     so the next schema drift is a visible 500 instead of a silent empty list — a gate in front
//!     of a handler that cannot work is theatre.

use axum::{
    extract::{Path, Query, State},
    middleware, Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::auth::platform_admin::require_platform_admin;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Every column of the live table, listed explicitly so a future column cannot change what this
/// module decodes.
const TEMPLATE_COLS: &str = "id, aid, tenant_id, name, subject, body, html_body, body_text, \
                             is_default, template_type, created_at, updated_at";

/// Full email template row, exactly as tolerant as the table: `aid`, `tenant_id`, the bodies and
/// the timestamps are all NULLABLE there (the seeded platform row has `aid`/`tenant_id` NULL), and
/// a row type stricter than its data is a decode error waiting to silently empty a list.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EmailTemplate {
    pub id: Uuid,
    pub aid: Option<Uuid>,
    pub tenant_id: Option<Uuid>,
    pub name: String,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub body_text: Option<String>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// JSON payload for one template: the row, plus the DERIVED `is_html` flag the admin guide
/// documents. `is_html` is true when the template carries an HTML body.
fn payload(item: &EmailTemplate) -> Value {
    let mut value = serde_json::to_value(item).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("is_html".to_string(), json!(item.html_body.is_some()));
    }
    value
}

/// `is_html = false` means a TEXT-ONLY template. The live schema expresses that as a NULL
/// `html_body`; there is no column to record the flag itself.
fn html_body_for(is_html: Option<bool>, html_body: &Option<String>) -> Option<String> {
    if is_html == Some(false) {
        None
    } else {
        html_body.clone()
    }
}

/// A unique-constraint violation is a 409, not a 500: the registry's one structural rule is
/// "one default per (template_type, aid)", enforced by idx_email_templates_unique.
fn map_write_error(e: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(ref db_err) = e {
        if db_err.code().as_deref() == Some("23505") {
            return AppError::Duplicate(
                "a default template of that type already exists for this scope".to_string(),
            );
        }
    }
    AppError::Database(e)
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub template_type: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateInput {
    pub name: String,
    /// REQUIRED: the column is NOT NULL in the live table, so a missing subject used to be a 500.
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    /// REQUIRED: the column is NOT NULL, and email.rs resolves templates by this value.
    pub template_type: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateInput {
    pub name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
}

/// GET /api/email-templates — platform admin only.
pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&state.db, &claims.sub).await?;

    let limit = query.per_page.unwrap_or(50).clamp(1, 100);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let items = if let Some(tt) = &query.template_type {
        sqlx::query_as::<_, EmailTemplate>(&format!(
            "SELECT {TEMPLATE_COLS} FROM email_templates WHERE template_type = $1 \
             ORDER BY name LIMIT $2 OFFSET $3"
        ))
        .bind(tt)
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, EmailTemplate>(&format!(
            "SELECT {TEMPLATE_COLS} FROM email_templates ORDER BY name LIMIT $1 OFFSET $2"
        ))
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_templates")
        .fetch_one(&state.db)
        .await?;

    // Every row goes through `payload` so `is_html` is present (derived) on the list too, not
    // just on single-row reads.
    let items: Vec<Value> = items.iter().map(payload).collect();
    Ok(Json(json!({ "items": items, "count": count })))
}

/// GET /api/email-templates/{id} — platform admin only.
pub async fn get_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&state.db, &claims.sub).await?;

    let item = sqlx::query_as::<_, EmailTemplate>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Email template not found".to_string()))?;

    Ok(Json(json!({ "item": item })))
}

/// POST /api/email-templates — publish a platform template (platform admin only).
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateInput>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&state.db, &claims.sub).await?;

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation("name is required".to_string()));
    }
    let subject = body
        .subject
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::Validation("subject is required".to_string()))?;
    let template_type = body
        .template_type
        .as_ref()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| AppError::Validation("template_type is required".to_string()))?;

    // A platform row: `aid`/`tenant_id` stay NULL, which is also the convention the seeded
    // welcome template and idx_email_templates_unique are written against.
    let id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO email_templates
               (id, aid, tenant_id, name, subject, body, html_body, is_default, template_type)
           VALUES ($1, NULL, NULL, $2, $3, $4, $5, $6, $7)"#,
    )
    .bind(id)
    .bind(&name)
    .bind(&subject)
    .bind(&body.body)
    .bind(html_body_for(body.is_html, &body.html_body))
    .bind(body.is_default.unwrap_or(false))
    .bind(&template_type)
    .execute(&state.db)
    .await
    .map_err(map_write_error)?;

    let item = sqlx::query_as::<_, EmailTemplate>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({ "item": payload(&item) })))
}

/// PUT /api/email-templates/{id} — update a platform template (platform admin only).
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInput>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&state.db, &claims.sub).await?;

    // `is_html = false` clears html_body; anything else leaves the stored HTML body alone.
    let clear_html = body.is_html == Some(false);
    let result = sqlx::query(
        r#"UPDATE email_templates SET
            name = COALESCE($1, name),
            subject = COALESCE($2, subject),
            body = COALESCE($3, body),
            html_body = CASE WHEN $4::bool THEN NULL ELSE COALESCE($5, html_body) END,
            is_default = COALESCE($6, is_default),
            template_type = COALESCE($7, template_type),
            updated_at = NOW()
           WHERE id = $8"#,
    )
    .bind(&body.name)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(clear_html)
    .bind(&body.html_body)
    .bind(body.is_default)
    .bind(&body.template_type)
    .bind(id)
    .execute(&state.db)
    .await
    .map_err(map_write_error)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    let item = sqlx::query_as::<_, EmailTemplate>(&format!(
        "SELECT {TEMPLATE_COLS} FROM email_templates WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    Ok(Json(json!({ "item": payload(&item) })))
}

/// DELETE /api/email-templates/{id} — platform admin only.
pub async fn delete_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&state.db, &claims.sub).await?;

    let result = sqlx::query("DELETE FROM email_templates WHERE id = $1")
        .bind(id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Email template not found".to_string()));
    }

    Ok(Json(json!({ "status": "deleted", "id": id })))
}

/// GET /api/email-templates/merge-fields
/// Returns available merge fields, optionally filtered by template_type.
///
/// Deliberately authenticated-only rather than platform-admin: this returns static merge-field
/// NAMES from [`crate::email::get_merge_fields`], reads no row and names no tenant, so a
/// platform gate here would add friction without withholding anything.
#[derive(Deserialize)]
pub struct MergeFieldsQuery {
    pub template_type: Option<String>,
}

pub async fn get_merge_fields_handler(
    Query(query): Query<MergeFieldsQuery>,
) -> ApiResult<Json<Value>> {
    let fields = match &query.template_type {
        Some(tt) => crate::email::get_merge_fields(tt),
        None => crate::email::get_merge_fields("default"),
    };

    Ok(Json(json!({
        "fields": fields,
        "template_type": query.template_type.unwrap_or_else(|| "all".to_string()),
    })))
}

/// Build an Axum router for email-templates endpoints
pub fn router(state: AppState) -> Router<AppState> {
    use axum::routing;

    Router::new()
        .route("/", routing::get(list).post(create))
        .route(
            "/:id",
            routing::get(get_handler)
                .put(update)
                .delete(delete_template),
        )
        .route("/merge-fields", routing::get(get_merge_fields_handler))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(html_body: Option<&str>) -> EmailTemplate {
        EmailTemplate {
            id: Uuid::nil(),
            aid: None,
            tenant_id: None,
            name: "Default Welcome Email".to_string(),
            subject: Some("Welcome".to_string()),
            body: Some("text".to_string()),
            html_body: html_body.map(str::to_string),
            body_text: None,
            is_default: Some(true),
            template_type: Some("welcome".to_string()),
            created_at: None,
            updated_at: None,
        }
    }

    /// `is_html` is not a column; it must be derived, or the published guide documents a lie.
    #[test]
    fn is_html_is_derived_from_the_html_body() {
        let html = payload(&row(Some("<p>hi</p>")));
        assert_eq!(html.get("is_html"), Some(&serde_json::json!(true)));
        let text = payload(&row(None));
        assert_eq!(text.get("is_html"), Some(&serde_json::json!(false)));
    }

    /// The seeded platform row has NULL aid/tenant_id and NULL timestamps: it must serialize,
    /// not fail a decode (which is what silently emptied this endpoint's list).
    #[test]
    fn a_platform_row_with_null_scope_fields_serializes() {
        let v = payload(&row(Some("<p>hi</p>")));
        assert!(v.get("aid").is_some_and(Value::is_null));
        assert!(v.get("tenant_id").is_some_and(Value::is_null));
        assert_eq!(
            v.get("name").and_then(Value::as_str),
            Some("Default Welcome Email")
        );
    }

    /// is_html=false means text-only, which the live schema can only express as html_body = NULL.
    #[test]
    fn text_only_means_no_html_body() {
        assert_eq!(
            html_body_for(Some(false), &Some("<p>hi</p>".to_string())),
            None
        );
        assert_eq!(
            html_body_for(Some(true), &Some("<p>hi</p>".to_string())),
            Some("<p>hi</p>".to_string())
        );
        assert_eq!(html_body_for(None, &None), None);
        assert_eq!(
            html_body_for(None, &Some("x".to_string())),
            Some("x".to_string())
        );
    }
}
