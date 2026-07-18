use axum::{extract::{State, Path, Json, Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use super::models::*;
use super::engine;

pub async fn list_rules(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"rules": sqlx::query_as::<_,ScoreRule>("SELECT * FROM score_rules WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}

pub async fn create_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.event_type.is_empty() { return Err(AppError::Validation("Name and event_type required".into())); }
    let dir = r.direction.unwrap_or_else(|| "add".into());
    if dir != "add" && dir != "subtract" { return Err(AppError::Validation("Direction must be add/subtract".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,ScoreRule>("INSERT INTO score_rules(id,tenant_id,name,event_type,points,direction) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.event_type).bind(r.points).bind(&dir).fetch_one(&s.db).await?))))
}

pub async fn update_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateRuleRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,ScoreRule>("UPDATE score_rules SET name=COALESCE($1,name), event_type=COALESCE($2,event_type), points=COALESCE($3,points), direction=COALESCE($4,direction), is_active=COALESCE($5,is_active), updated_at=NOW() WHERE id=$6 AND tenant_id=$7 RETURNING *")
        .bind(&r.name).bind(&r.event_type).bind(r.points).bind(&r.direction).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Score rule {id} not found")))?)))
}

pub async fn delete_rule(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM score_rules WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Score rule {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}

pub async fn get_score(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(cid): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    engine::ensure_score_record(&s.db, t, cid).await?;
    Ok(Json(json!(sqlx::query_as::<_,Score>("SELECT * FROM contact_scores WHERE tenant_id=$1 AND contact_id=$2").bind(t).bind(cid).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Score for contact {cid} not found")))?)))
}

pub async fn calculate_score(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(cid): Path<Uuid>, Json(r): Json<ScoreEventRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.event_type.is_empty() { return Err(AppError::Validation("event_type required".into())); }
    Ok(Json(json!(engine::calculate_score(&s.db, t, cid, &r.event_type).await?)))
}

pub async fn get_score_history(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(cid): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"history": sqlx::query_as::<_,ScoreHistory>("SELECT * FROM score_history WHERE tenant_id=$1 AND contact_id=$2 ORDER BY created_at DESC").bind(t).bind(cid).fetch_all(&s.db).await?})))
}

pub async fn score_distribution(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let dist = sqlx::query_as::<_,(String,i64)>("SELECT category, COUNT(*) FROM contact_scores WHERE tenant_id=$1 GROUP BY category ORDER BY category").bind(t).fetch_all(&s.db).await?;
    let total: i64 = dist.iter().map(|(_,c)| c).sum();
    Ok(Json(json!({"distribution":dist,"total":total})))
}

pub async fn list_thresholds(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"thresholds": sqlx::query_as::<_,ScoringThreshold>("SELECT * FROM scoring_thresholds WHERE tenant_id=$1 ORDER BY min_score").bind(t).fetch_all(&s.db).await?})))
}

pub async fn create_threshold(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateThresholdRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.min_score < 0 { return Err(AppError::Validation("min_score must be >= 0".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,ScoringThreshold>(
        "INSERT INTO scoring_thresholds(id,tenant_id,pipeline_id,min_score,max_score,target_stage_id,action,action_config) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(t)
    .bind(r.pipeline_id)
    .bind(r.min_score)
    .bind(r.max_score)
    .bind(r.target_stage_id)
    .bind(r.action.unwrap_or_else(|| "move_stage".into()))
    .bind(r.action_config.unwrap_or_else(|| serde_json::json!({})))
    .fetch_one(&s.db)
    .await?))))
}

pub async fn delete_threshold(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM scoring_thresholds WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Scoring threshold {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}

pub async fn list_webhooks(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"webhooks": sqlx::query_as::<_,ScoringWebhook>("SELECT * FROM scoring_webhooks WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}

pub async fn create_webhook(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateWebhookRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.url.is_empty() { return Err(AppError::Validation("Name and url required".into())); }
    if r.min_score < 0 { return Err(AppError::Validation("min_score must be >= 0".into())); }
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,ScoringWebhook>(
        "INSERT INTO scoring_webhooks(id,tenant_id,name,url,min_score,max_score,event_type,headers) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(t)
    .bind(&r.name)
    .bind(&r.url)
    .bind(r.min_score)
    .bind(r.max_score)
    .bind(&r.event_type)
    .bind(r.headers.unwrap_or_else(|| serde_json::json!({})))
    .fetch_one(&s.db)
    .await?))))
}

pub async fn delete_webhook(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM scoring_webhooks WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Scoring webhook {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
