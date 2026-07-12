use axum::{extract::{State, Path, Json, Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;

pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let accounts = if c.role == "agency_admin" {
        sqlx::query_as::<_, Account>("SELECT * FROM tenants WHERE is_active=true ORDER BY name").fetch_all(&s.db).await?
    } else {
        sqlx::query_as::<_, Account>("SELECT * FROM tenants WHERE id=$1 AND is_active=true").bind(aid).fetch_all(&s.db).await?
    };
    Ok(Json(json!({"accounts": accounts})))
}

pub async fn create(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateAccountRequest>) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" { return Err(AppError::Forbidden); }
    if r.name.is_empty() || r.slug.is_empty() { return Err(AppError::Validation("Name and slug required".to_string())); }
    let account = sqlx::query_as::<_, Account>("INSERT INTO tenants(id,name,slug,logo_url,primary_color,accent_color,custom_domain) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
        .bind(Uuid::new_v4()).bind(&r.name).bind(&r.slug).bind(&r.logo_url).bind(&r.primary_color).bind(&r.accent_color).bind(&r.custom_domain)
        .fetch_one(&s.db).await.map_err(|e| { if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("tenants_slug_key") { return AppError::Duplicate(format!("Slug '{}' exists", r.slug)); } } AppError::Database(e) })?;
    Ok((StatusCode::CREATED, Json(json!(account))))
}

pub async fn get(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if c.role != "agency_admin" && aid != id { return Err(AppError::Forbidden); }
    Ok(Json(json!(sqlx::query_as::<_, Account>("SELECT * FROM tenants WHERE id=$1").bind(id).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Account {id} not found")))?)))
}

pub async fn update(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateAccountRequest>) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if c.role != "agency_admin" && aid != id { return Err(AppError::Forbidden); }
    Ok(Json(json!(sqlx::query_as::<_, Account>("UPDATE tenants SET name=COALESCE($1,name), slug=COALESCE($2,slug), logo_url=COALESCE($3,logo_url), primary_color=COALESCE($4,primary_color), accent_color=COALESCE($5,accent_color), custom_domain=COALESCE($6,custom_domain), is_active=COALESCE($7,is_active), updated_at=NOW() WHERE id=$8 RETURNING *")
        .bind(&r.name).bind(&r.slug).bind(&r.logo_url).bind(&r.primary_color).bind(&r.accent_color).bind(&r.custom_domain).bind(r.is_active).bind(id).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Account {id} not found")))?)))
}

pub async fn delete(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    if c.role != "agency_admin" { return Err(AppError::Forbidden); }
    let r = sqlx::query("DELETE FROM tenants WHERE id=$1").bind(id).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Account {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}

pub async fn get_settings(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if c.role != "agency_admin" && aid != id { return Err(AppError::Forbidden); }
    let settings = sqlx::query_scalar::<_, Option<serde_json::Value>>("SELECT settings FROM tenants WHERE id=$1").bind(id).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Account {id} not found")))?;
    Ok(Json(json!({"settings": settings.unwrap_or(serde_json::Value::Object(Default::default()))})))
}

pub async fn update_settings(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(settings): Json<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let aid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if c.role != "agency_admin" && aid != id { return Err(AppError::Forbidden); }
    let r = sqlx::query("UPDATE tenants SET settings=$1, updated_at=NOW() WHERE id=$2").bind(&settings).bind(id).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Account {id} not found"))); }
    Ok(Json(json!({"message":"Settings updated","settings": settings})))
}
