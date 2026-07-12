use axum::{
    extract::{State, Path, Json, Extension, Query},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;
use crate::audit;
use super::models::*;

/// Full opportunity representation used internally.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct OpportunityFull {
    pub id: Uuid,
    pub account_id: Uuid,
    pub pipeline_id: Uuid,
    pub stage_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub status: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

use serde::Serialize;

#[derive(Debug, Deserialize)]
pub struct CreateOpportunityRequest {
    pub name: String,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub description: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateOpportunityRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OppListParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub stage_id: Option<Uuid>,
    pub status: Option<String>,
    pub contact_id: Option<Uuid>,
}

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
    Query(params): Query<OppListParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;
    let opps = sqlx::query_as::<_, OpportunityFull>(
        "SELECT * FROM opportunities WHERE pipeline_id = $1 AND tenant_id = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
    ).bind(pipeline_id).bind(account_id).bind(per_page).bind(offset).fetch_all(&state.db).await?;
    Ok(Json(json!({ "opportunities": opps, "page": page, "per_page": per_page })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
    Json(req): Json<CreateOpportunityRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if req.name.is_empty() {
        return Err(AppError::Validation("Opportunity name is required".to_string()));
    }
    let first_stage = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position LIMIT 1"
    ).bind(pipeline_id).fetch_optional(&state.db).await?
    .ok_or(AppError::BadRequest("Pipeline has no stages".to_string()))?;

    let opp = sqlx::query_as::<_, OpportunityFull>(
        r#"INSERT INTO opportunities (id, account_id, pipeline_id, stage_id, contact_id, company_id,
            name, description, value, currency, probability, expected_close_date, metadata)
           VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING *"#
    ).bind(Uuid::new_v4()).bind(account_id).bind(pipeline_id).bind(first_stage.id)
    .bind(req.contact_id).bind(req.company_id).bind(&req.name).bind(&req.description)
    .bind(req.value).bind(&req.currency).bind(req.probability).bind(req.expected_close_date)
    .bind(&req.metadata).fetch_one(&state.db).await?;

    // Log initial stage entry
    sqlx::query(
        "INSERT INTO stage_history (id, opportunity_id, to_stage_id) VALUES ($1, $2, $3)"
    ).bind(Uuid::new_v4()).bind(opp.id).bind(first_stage.id).execute(&state.db).await?;

    Ok((StatusCode::CREATED, Json(json!(opp))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let opp = sqlx::query_as::<_, OpportunityFull>(
        "SELECT * FROM opportunities WHERE id = $1 AND pipeline_id = $2 AND account_id = $3"
    ).bind(id).bind(pipeline_id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Opportunity {} not found", id)))?;
    Ok(Json(json!(opp)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateOpportunityRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let opp = sqlx::query_as::<_, OpportunityFull>(
        r#"UPDATE opportunities SET name = COALESCE($1,name), description = COALESCE($2,description),
            value = COALESCE($3,value), currency = COALESCE($4,currency),
            contact_id = COALESCE($5,contact_id), company_id = COALESCE($6,company_id),
            probability = COALESCE($7,probability), expected_close_date = COALESCE($8,expected_close_date),
            metadata = COALESCE($9,metadata), is_active = COALESCE($10,is_active), updated_at = NOW()
           WHERE id = $11 AND pipeline_id = $12 AND account_id = $13 RETURNING *"#
    ).bind(&req.name).bind(&req.description).bind(req.value).bind(&req.currency)
    .bind(req.contact_id).bind(req.company_id).bind(req.probability).bind(req.expected_close_date)
    .bind(&req.metadata).bind(req.is_active).bind(id).bind(pipeline_id).bind(account_id)
    .fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Opportunity {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        account_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "opportunity.updated",
        "opportunity",
        Some(id),
        Some(json!({"updated": true})),
        None,
    ).await;

    Ok(Json(json!(opp)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM opportunities WHERE id = $1 AND pipeline_id = $2 AND account_id = $3")
        .bind(id).bind(pipeline_id).bind(account_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Opportunity {} not found", id)));
    }
    Ok(Json(json!({"message": "Opportunity deleted successfully"})))
}
