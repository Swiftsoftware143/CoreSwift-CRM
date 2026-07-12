use axum::{
    extract::{State, Path, Json, Extension},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::audit;
use super::models::*;
use super::opportunity::OpportunityFull;

pub async fn list_pipelines(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipelines = sqlx::query_as::<_, Pipeline>(
        "SELECT * FROM pipelines WHERE tenant_id = $1 AND is_active = true ORDER BY name"
    ).bind(account_id).fetch_all(&state.db).await?;

    let mut result = Vec::new();
    for pipeline in pipelines {
        let stages = sqlx::query_as::<_, PipelineStage>(
            "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position"
        ).bind(pipeline.id).fetch_all(&state.db).await?;
        result.push(PipelineWithStages { pipeline, stages });
    }
    Ok(Json(json!({ "pipelines": result })))
}

pub async fn create_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreatePipelineRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if req.name.is_empty() {
        return Err(AppError::Validation("Pipeline name is required".to_string()));
    }
    let pipeline = sqlx::query_as::<_, Pipeline>(
        r#"INSERT INTO pipelines (id, account_id, name, description, is_default) VALUES ($1,$2,$3,$4,$5) RETURNING *"#
    ).bind(Uuid::new_v4()).bind(account_id).bind(&req.name).bind(&req.description)
    .bind(req.is_default.unwrap_or(false)).fetch_one(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!(pipeline))))
}

pub async fn get_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline = sqlx::query_as::<_, Pipeline>(
        "SELECT * FROM pipelines WHERE id = $1 AND account_id = $2"
    ).bind(id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Pipeline {} not found", id)))?;
    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position"
    ).bind(id).fetch_all(&state.db).await?;
    Ok(Json(json!(PipelineWithStages { pipeline, stages })))
}

pub async fn update_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePipelineRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline = sqlx::query_as::<_, Pipeline>(
        r#"UPDATE pipelines SET name = COALESCE($1,name), description = COALESCE($2,description),
            is_default = COALESCE($3,is_default), is_active = COALESCE($4,is_active), updated_at = NOW()
           WHERE id = $5 AND account_id = $6 RETURNING *"#
    ).bind(&req.name).bind(&req.description).bind(req.is_default).bind(req.is_active)
    .bind(id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Pipeline {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        account_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "pipeline.updated",
        "pipeline",
        Some(id),
        Some(json!({"updated": true})),
        None,
    ).await;

    Ok(Json(json!(pipeline)))
}

pub async fn delete_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM pipelines WHERE id = $1 AND account_id = $2")
        .bind(id).bind(account_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Pipeline {} not found", id)));
    }
    Ok(Json(json!({"message": "Pipeline deleted successfully"})))
}

pub async fn list_stages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND account_id = $2")
        .bind(pipeline_id).bind(account_id).fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound(format!("Pipeline {} not found", pipeline_id)))?;
    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position"
    ).bind(pipeline_id).fetch_all(&state.db).await?;
    Ok(Json(json!({ "stages": stages })))
}

pub async fn create_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
    Json(req): Json<CreateStageRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND account_id = $2")
        .bind(pipeline_id).bind(account_id).fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound(format!("Pipeline {} not found", pipeline_id)))?;
    let stage = sqlx::query_as::<_, PipelineStage>(
        r#"INSERT INTO pipeline_stages (id, pipeline_id, name, description, color, position, probability)
           VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *"#
    ).bind(Uuid::new_v4()).bind(pipeline_id).bind(&req.name).bind(&req.description)
    .bind(&req.color).bind(req.position.unwrap_or(0)).bind(req.probability)
    .fetch_one(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!(stage))))
}

pub async fn update_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateStageRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND account_id = $2")
        .bind(pipeline_id).bind(account_id).fetch_optional(&state.db).await?
        .ok_or(AppError::NotFound(format!("Pipeline {} not found", pipeline_id)))?;
    let stage = sqlx::query_as::<_, PipelineStage>(
        r#"UPDATE pipeline_stages SET name = COALESCE($1,name), description = COALESCE($2,description),
            color = COALESCE($3,color), position = COALESCE($4,position),
            is_won_stage = COALESCE($5,is_won_stage), is_lost_stage = COALESCE($6,is_lost_stage),
            probability = COALESCE($7,probability), updated_at = NOW()
           WHERE id = $8 AND pipeline_id = $9 RETURNING *"#
    ).bind(&req.name).bind(&req.description).bind(&req.color).bind(req.position)
    .bind(req.is_won_stage).bind(req.is_lost_stage).bind(req.probability)
    .bind(stage_id).bind(pipeline_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Stage {} not found", stage_id)))?;
    Ok(Json(json!(stage)))
}

pub async fn delete_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((_pipeline_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query(
        "DELETE FROM pipeline_stages WHERE id = $1 AND pipeline_id IN (SELECT id FROM pipelines WHERE account_id = $2)"
    ).bind(stage_id).bind(account_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Stage {} not found", stage_id)));
    }
    Ok(Json(json!({"message": "Stage deleted successfully"})))
}

pub async fn move_opportunity(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, stage_id, opportunity_id)): Path<(Uuid, Uuid, Uuid)>,
    Json(_req): Json<MoveOpportunityRequest>,
) -> ApiResult<impl IntoResponse> {
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let stage = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE id = $1 AND pipeline_id = $2"
    ).bind(stage_id).bind(pipeline_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Stage {} not found", stage_id)))?;

    let opp = sqlx::query_as::<_, OpportunityFull>(
        "SELECT * FROM opportunities WHERE id = $1 AND account_id = $2"
    ).bind(opportunity_id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Opportunity {} not found", opportunity_id)))?;

    let prev_stage = opp.stage_id;
    let status = if stage.is_won_stage { "won" } else if stage.is_lost_stage { "lost" } else { "open" };

    sqlx::query(
        "UPDATE opportunities SET stage_id = $1, status = $2::opportunity_status, probability = $3, updated_at = NOW() WHERE id = $4"
    ).bind(stage_id).bind(status).bind(stage.probability).bind(opportunity_id).execute(&state.db).await?;

    sqlx::query(
        "INSERT INTO stage_history (id, opportunity_id, from_stage_id, to_stage_id, moved_by) VALUES ($1,$2,$3,$4,$5)"
    ).bind(Uuid::new_v4()).bind(opportunity_id).bind(prev_stage).bind(stage_id).bind(user_id).execute(&state.db).await?;

    Ok(Json(json!({"message": "Opportunity moved", "from_stage_id": prev_stage, "to_stage_id": stage_id, "status": status})))
}

pub async fn pipeline_analytics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline = sqlx::query_as::<_, Pipeline>(
        "SELECT * FROM pipelines WHERE id = $1 AND account_id = $2"
    ).bind(pipeline_id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Pipeline {} not found", pipeline_id)))?;

    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position"
    ).bind(pipeline_id).fetch_all(&state.db).await?;

    let total_opps: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0);

    let total_value: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(value), 0) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0.0);

    let won_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2 AND status = 'won'"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0);

    let won_value: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(value), 0) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2 AND status = 'won'"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0.0);

    let lost_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2 AND status = 'lost'"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0);

    let lost_value: f64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(value), 0) FROM opportunities WHERE pipeline_id = $1 AND account_id = $2 AND status = 'lost'"
    ).bind(pipeline_id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0.0);

    let mut stage_analytics = Vec::new();
    for stage in &stages {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM opportunities WHERE stage_id = $1 AND account_id = $2"
        ).bind(stage.id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0);

        let stage_value: f64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(value), 0) FROM opportunities WHERE stage_id = $1 AND account_id = $2"
        ).bind(stage.id).bind(account_id).fetch_one(&state.db).await.unwrap_or(0.0);

        let avg_time: f64 = sqlx::query_scalar(
            r#"SELECT COALESCE(AVG(EXTRACT(EPOCH FROM (sh2.created_at - sh1.created_at)) / 86400.0), 0)
               FROM stage_history sh1
               JOIN stage_history sh2 ON sh2.opportunity_id = sh1.opportunity_id
               WHERE sh1.to_stage_id = $1 AND sh2.from_stage_id = $1 AND sh2.id > sh1.id"#
        ).bind(stage.id).fetch_one(&state.db).await.unwrap_or(0.0);

        let conv = if total_opps > 0 { (won_count as f64 / total_opps as f64) * 100.0 } else { 0.0 };

        stage_analytics.push(StageAnalytics {
            stage_id: stage.id,
            stage_name: stage.name.clone(),
            count,
            total_value: stage_value,
            avg_time_in_days: avg_time,
            conversion_rate: (conv * 100.0).round() / 100.0,
        });
    }

    Ok(Json(json!(PipelineAnalytics {
        pipeline_id,
        pipeline_name: pipeline.name,
        total_opportunities: total_opps,
        total_value,
        won_count,
        won_value,
        lost_count,
        lost_value,
        stages: stage_analytics,
    })))
}
