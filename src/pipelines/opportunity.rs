//! Opportunities (deals) — repaired against the LIVE `opportunities` schema.
//!
//! The previous version queried `account_id`, `description`, `status` and `is_active`,
//! none of which exist on `opportunities`: every route in this module answered HTTP 500.
//! Live columns: tenant_id, pipeline_id, stage_id, contact_id, company_id, name, value
//! (numeric), currency, probability, expected_close_date, source, notes, metadata,
//! is_won, is_lost, lost_reason, created_at, updated_at.
//!
//! `value` is cast to float8 in every read path: sqlx cannot decode NUMERIC into f64,
//! and the old code swallowed that decode error with `unwrap_or(0.0)` (silently 0 deals).

use axum::{
    extract::{Extension, Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::models::*;
use crate::audit;
use crate::auth::models::Claims;
use crate::errors::{validate_pagination, ApiResult, AppError};
use crate::AppState;

/// Column list for reads: `value::float8` is required for f64 decoding.
pub(crate) const OPP_COLS: &str =
    "id, tenant_id, pipeline_id, stage_id, contact_id, company_id, name, \
     notes, value::float8 AS value, currency, probability, expected_close_date, source, metadata, \
     is_won, is_lost, lost_reason, created_at, updated_at";

/// Full opportunity representation used internally.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct OpportunityFull {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub pipeline_id: Uuid,
    pub stage_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub name: String,
    pub notes: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_won: bool,
    pub is_lost: bool,
    pub lost_reason: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOpportunityRequest {
    pub name: String,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub notes: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateOpportunityRequest {
    pub name: Option<String>,
    pub notes: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub contact_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<chrono::NaiveDate>,
    pub metadata: Option<serde_json::Value>,
    pub is_won: Option<bool>,
    pub is_lost: Option<bool>,
    pub lost_reason: Option<String>,
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
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;
    let status = params.status.clone();
    if let Some(s) = status.as_deref() {
        if s != "won" && s != "lost" && s != "open" {
            return Err(AppError::Validation(
                "status must be one of won/lost/open".to_string(),
            ));
        }
    }
    let sql = format!(
        "SELECT {OPP_COLS} FROM opportunities \
         WHERE pipeline_id = $1 AND tenant_id = $2 \
           AND ($3::uuid IS NULL OR stage_id = $3) \
           AND ($4::uuid IS NULL OR contact_id = $4) \
           AND ($5::text IS NULL \
                OR ($5 = 'won' AND is_won) \
                OR ($5 = 'lost' AND is_lost) \
                OR ($5 = 'open' AND NOT is_won AND NOT is_lost)) \
         ORDER BY created_at DESC LIMIT $6 OFFSET $7"
    );
    let opps = sqlx::query_as::<_, OpportunityFull>(&sql)
        .bind(pipeline_id)
        .bind(tenant_id)
        .bind(params.stage_id)
        .bind(params.contact_id)
        .bind(status)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND tenant_id = $2",
    )
    .bind(pipeline_id)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(
        json!({ "opportunities": opps, "page": page, "per_page": per_page, "total": total }),
    ))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
    Json(req): Json<CreateOpportunityRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if req.name.is_empty() {
        return Err(AppError::Validation(
            "Opportunity name is required".to_string(),
        ));
    }
    // The pipeline must belong to this tenant (otherwise a deal could be planted in
    // another tenant's pipeline and then vanish from its own board).
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND tenant_id = $2")
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!(
            "Pipeline {} not found",
            pipeline_id
        )))?;

    let first_stage = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position LIMIT 1",
    )
    .bind(pipeline_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::BadRequest("Pipeline has no stages".to_string()))?;

    let sql = format!(
        "INSERT INTO opportunities (id, tenant_id, pipeline_id, stage_id, contact_id, company_id, \
            name, notes, value, currency, probability, expected_close_date, source, metadata) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::float8,$10,$11,$12,$13,$14) RETURNING {OPP_COLS}"
    );
    let opp = sqlx::query_as::<_, OpportunityFull>(&sql)
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(pipeline_id)
        .bind(first_stage.id)
        .bind(req.contact_id)
        .bind(req.company_id)
        .bind(&req.name)
        .bind(&req.notes)
        .bind(req.value)
        .bind(&req.currency)
        .bind(req.probability)
        .bind(req.expected_close_date)
        .bind(&req.source)
        .bind(&req.metadata)
        .fetch_one(&state.db)
        .await?;

    // Log initial stage entry
    sqlx::query(
        "INSERT INTO opportunity_stage_history (id, opportunity_id, to_stage_id) VALUES ($1, $2, $3)",
    )
    .bind(Uuid::new_v4())
    .bind(opp.id)
    .bind(first_stage.id)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(opp))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let sql = format!(
        "SELECT {OPP_COLS} FROM opportunities WHERE id = $1 AND pipeline_id = $2 AND tenant_id = $3"
    );
    let opp = sqlx::query_as::<_, OpportunityFull>(&sql)
        .bind(id)
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!("Opportunity {} not found", id)))?;
    Ok(Json(json!(opp)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateOpportunityRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let sql = format!(
        r#"UPDATE opportunities SET name = COALESCE($1,name), notes = COALESCE($2,notes),
            value = COALESCE($3::float8,value), currency = COALESCE($4,currency),
            contact_id = COALESCE($5,contact_id), company_id = COALESCE($6,company_id),
            probability = COALESCE($7,probability), expected_close_date = COALESCE($8,expected_close_date),
            metadata = COALESCE($9,metadata), is_won = COALESCE($10,is_won),
            is_lost = COALESCE($11,is_lost), lost_reason = COALESCE($12,lost_reason),
            updated_at = NOW()
           WHERE id = $13 AND pipeline_id = $14 AND tenant_id = $15 RETURNING {OPP_COLS}"#
    );
    let opp = sqlx::query_as::<_, OpportunityFull>(&sql)
        .bind(&req.name)
        .bind(&req.notes)
        .bind(req.value)
        .bind(&req.currency)
        .bind(req.contact_id)
        .bind(req.company_id)
        .bind(req.probability)
        .bind(req.expected_close_date)
        .bind(&req.metadata)
        .bind(req.is_won)
        .bind(req.is_lost)
        .bind(&req.lost_reason)
        .bind(id)
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!("Opportunity {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        tenant_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "opportunity.updated",
        "opportunity",
        Some(id),
        Some(json!({"updated": true})),
        None,
    )
    .await;

    Ok(Json(json!(opp)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query(
        "DELETE FROM opportunities WHERE id = $1 AND pipeline_id = $2 AND tenant_id = $3",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(tenant_id)
    .execute(&state.db)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Opportunity {} not found", id)));
    }
    Ok(Json(json!({"message": "Opportunity deleted successfully"})))
}
