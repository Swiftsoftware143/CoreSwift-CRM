use axum::{extract::{State,Path,Json,Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use super::models::*;

pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"integrations": sqlx::query_as::<_,Integration>("SELECT * FROM integrations WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}
pub async fn create(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateIntegrationRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.provider.is_empty() { return Err(AppError::Validation("Name and provider required".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,Integration>("INSERT INTO integrations(id,tenant_id,name,provider,config) VALUES($1,$2,$3,$4::integration_provider,$5) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.provider).bind(&r.config).fetch_one(&s.db).await?))))
}
pub async fn get(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,Integration>("SELECT * FROM integrations WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Integration {id} not found")))?)))
}
pub async fn update(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateIntegrationRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,Integration>("UPDATE integrations SET name=COALESCE($1,name), config=COALESCE($2,config), is_active=COALESCE($3,is_active), updated_at=NOW() WHERE id=$4 AND tenant_id=$5 RETURNING *")
        .bind(&r.name).bind(&r.config).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Integration {id} not found")))?)))
}
pub async fn delete(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM integrations WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Integration {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
pub async fn list_mappings(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(iid): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"mappings": sqlx::query_as::<_,TagMapping>("SELECT * FROM tag_mappings WHERE integration_id=$1 AND tenant_id=$2").bind(iid).bind(t).fetch_all(&s.db).await?})))
}
pub async fn create_mapping(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(iid): Path<Uuid>, Json(r): Json<CreateMappingRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let dir = r.direction.unwrap_or_else(|| "bidirectional".into());
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,TagMapping>("INSERT INTO tag_mappings(id,tenant_id,integration_id,local_tag_id,external_system,external_id,direction) VALUES($1,$2,$3,$4,$5,$6,$7::mapping_direction) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(iid).bind(r.local_tag_id).bind(&r.external_system).bind(&r.external_id).bind(&dir).fetch_one(&s.db).await?))))
}
pub async fn delete_mapping(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tag_mappings WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Mapping {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
pub async fn list_webhooks(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"webhooks": sqlx::query_as::<_,Webhook>("SELECT * FROM webhooks WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}
pub async fn create_webhook(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateWebhookRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.url.is_empty() { return Err(AppError::Validation("Name and url required".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,Webhook>("INSERT INTO webhooks(id,tenant_id,name,url,secret,events,retry_count,timeout_seconds) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.url).bind(&r.secret).bind(&r.events).bind(r.retry_count.unwrap_or(3)).bind(r.timeout_seconds.unwrap_or(30)).fetch_one(&s.db).await?))))
}
pub async fn update_webhook(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateWebhookRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,Webhook>("UPDATE webhooks SET name=COALESCE($1,name), url=COALESCE($2,url), secret=COALESCE($3,secret), events=COALESCE($4,events), retry_count=COALESCE($5,retry_count), timeout_seconds=COALESCE($6,timeout_seconds), is_active=COALESCE($7,is_active), updated_at=NOW() WHERE id=$8 AND tenant_id=$9 RETURNING *")
        .bind(&r.name).bind(&r.url).bind(&r.secret).bind(&r.events).bind(r.retry_count).bind(r.timeout_seconds).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Webhook {id} not found")))?)))
}
pub async fn delete_webhook(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM webhooks WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Webhook {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
