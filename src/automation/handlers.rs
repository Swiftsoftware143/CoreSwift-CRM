use axum::{extract::{State,Path,Json,Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use super::models::*;

pub async fn list_rules(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"rules": sqlx::query_as::<_,AutomationRule>("SELECT * FROM automation_rules WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}

pub async fn create_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() { return Err(AppError::Validation("Name required".into())); }
    let valid_t = ["TagAdded","TagRemoved","StageChanged","ScoreChanged","ListAdded","ListRemoved","tag.assigned","tag.unassigned"];
    if !valid_t.contains(&r.trigger_type.as_str()) { return Err(AppError::Validation("Invalid trigger_type".into())); }
    let valid_a = ["AddTag","RemoveTag","MovePipeline","AddToList","RemoveFromList","Webhook","NotifyUser","send_email","send_sms","pipeline.move","scoring.update"];
    if !valid_a.contains(&r.action_type.as_str()) { return Err(AppError::Validation("Invalid action_type".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,AutomationRule>("INSERT INTO automation_rules(id,tenant_id,name,description,trigger_type,trigger_config,action_type,action_config) VALUES($1,$2,$3,$4,$5::trigger_type,$6,$7::action_type,$8) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.description).bind(&r.trigger_type).bind(&r.trigger_config).bind(&r.action_type).bind(&r.action_config).fetch_one(&s.db).await?))))
}

pub async fn get_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,AutomationRule>("SELECT * FROM automation_rules WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Rule {id} not found")))?)))
}

pub async fn update_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,AutomationRule>("UPDATE automation_rules SET name=COALESCE($1,name), description=COALESCE($2,description), trigger_type=COALESCE($3::text::trigger_type,trigger_type), trigger_config=COALESCE($4,trigger_config), action_type=COALESCE($5::text::action_type,action_type), action_config=COALESCE($6,action_config), is_enabled=COALESCE($7,is_enabled), updated_at=NOW() WHERE id=$8 AND tenant_id=$9 RETURNING *")
        .bind(&r.name).bind(&r.description).bind(&r.trigger_type).bind(&r.trigger_config).bind(&r.action_type).bind(&r.action_config).bind(r.is_enabled).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Rule {id} not found")))?)))
}

pub async fn delete_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM automation_rules WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Rule {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
