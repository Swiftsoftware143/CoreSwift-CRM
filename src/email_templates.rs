//! Email Templates handler — CRUD for email templates
//! Supports list, get, create, update, delete with admin auth.

use axum::{
    extract::{Path, State, Query},
    Extension, Json, Router, middleware,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;

/// Full email template row
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct EmailTemplate {
    pub id: Uuid,
    pub aid: Uuid,
    pub name: String,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
    pub template_type: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
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
    pub subject: Option<String>,
    pub body: Option<String>,
    pub html_body: Option<String>,
    pub is_html: Option<bool>,
    pub is_default: Option<bool>,
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

/// Convenience function to require admin access
fn require_admin(claims: &Claims) -> Result<(), AppError> {
    if claims.role != "admin" && claims.role != "owner" && claims.role != "superadmin" && claims.role != "account_owner" {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// GET /api/email-templates
pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;

    let limit = query.per_page.unwrap_or(50).min(100);
    let page = query.page.unwrap_or(1).max(1);
    let offset = (page - 1) * limit;

    let items = if let Some(tt) = &query.template_type {
        sqlx::query_as::<_, EmailTemplate>(
            "SELECT * FROM email_templates WHERE template_type = $1 ORDER BY name LIMIT $2 OFFSET $3"
        )
        .bind(tt).bind(limit).bind(offset)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    } else {
        sqlx::query_as::<_, EmailTemplate>(
            "SELECT * FROM email_templates ORDER BY name LIMIT $1 OFFSET $2"
        )
        .bind(limit).bind(offset)
        .fetch_all(&state.db)
        .await
        .unwrap_or_default()
    };

    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_templates")
        .fetch_one(&state.db)
        .await
        .unwrap_or(0);

    Ok(Json(json!({ "items": items, "count": count })))
}

/// GET /api/email-templates/{id}
pub async fn get_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;

    let item = sqlx::query_as::<_, EmailTemplate>(
        "SELECT * FROM email_templates WHERE id = $1"
    )
    .bind(id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Email template not found".to_string()))?;

    Ok(Json(json!({"item": item})))
}

/// POST /api/email-templates — create a new email template
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<CreateInput>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;

    let id = Uuid::new_v4();
    let aid = Uuid::nil();

    sqlx::query(
        r#"INSERT INTO email_templates (id, aid, name, subject, body, html_body, is_html, is_default, template_type)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#
    )
    .bind(id).bind(aid)
    .bind(&body.name)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(&body.html_body)
    .bind(body.is_html.unwrap_or(true))
    .bind(body.is_default.unwrap_or(false))
    .bind(&body.template_type)
    .execute(&state.db).await?;

    let item = sqlx::query_as::<_, EmailTemplate>(
        "SELECT * FROM email_templates WHERE id = $1"
    )
    .bind(id).fetch_one(&state.db).await?;

    Ok(Json(json!({"item": item})))
}

/// PUT /api/email-templates/{id} — update an existing template
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateInput>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;

    sqlx::query(
        r#"UPDATE email_templates SET
            name = COALESCE($1, name),
            subject = COALESCE($2, subject),
            body = COALESCE($3, body),
            html_body = COALESCE($4, html_body),
            is_html = COALESCE($5, is_html),
            is_default = COALESCE($6, is_default),
            template_type = COALESCE($7, template_type),
            updated_at = NOW()
           WHERE id = $8"#
    )
    .bind(&body.name)
    .bind(&body.subject)
    .bind(&body.body)
    .bind(&body.html_body)
    .bind(body.is_html)
    .bind(body.is_default)
    .bind(&body.template_type)
    .bind(id)
    .execute(&state.db).await?;

    let item = sqlx::query_as::<_, EmailTemplate>(
        "SELECT * FROM email_templates WHERE id = $1"
    )
    .bind(id).fetch_one(&state.db).await?;

    Ok(Json(json!({"item": item})))
}

/// DELETE /api/email-templates/{id}
pub async fn delete_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    require_admin(&claims)?;

    sqlx::query("DELETE FROM email_templates WHERE id = $1")
        .bind(id).execute(&state.db).await?;

    Ok(Json(json!({"status": "deleted"})))
}

/// GET /api/email-templates/merge-fields
/// Returns available merge fields, optionally filtered by template_type
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
        .route("/:id", routing::get(get_handler).put(update).delete(delete_template))
        .route("/merge-fields", routing::get(get_merge_fields_handler))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
