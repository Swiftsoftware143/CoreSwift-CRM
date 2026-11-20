use axum::{
    extract::{Extension, Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use super::models::*;
use super::opportunity::{opp_cols, OpportunityFull};
use crate::audit;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn list_pipelines(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipelines = sqlx::query_as::<_, Pipeline>(
        "SELECT * FROM pipelines WHERE tenant_id = $1 AND is_active = true ORDER BY name",
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    let mut result = Vec::new();
    for pipeline in pipelines {
        let stages = sqlx::query_as::<_, PipelineStage>(
            "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position",
        )
        .bind(pipeline.id)
        .fetch_all(&state.db)
        .await?;
        result.push(PipelineWithStages { pipeline, stages });
    }
    Ok(Json(json!({ "pipelines": result })))
}

pub async fn create_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreatePipelineRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if req.name.is_empty() {
        return Err(AppError::Validation(
            "Pipeline name is required".to_string(),
        ));
    }

    // Usage ceiling (`limit_pipelines`): checked on the route that ADDS a pipeline and nowhere else
    // — its GET/PATCH/DELETE siblings must keep working at the ceiling, or the tenant cannot even
    // free a slot (kanban t_f49e4299).
    let pipeline_usage = crate::features::count_pipelines(&state.db, tenant_id).await;
    crate::features::enforce_usage_limit(
        &state.db,
        tenant_id,
        "limit_pipelines",
        "Pipeline",
        "pipelines",
        pipeline_usage,
    )
    .await?;

    let pipeline = sqlx::query_as::<_, Pipeline>(
        r#"INSERT INTO pipelines (id, tenant_id, name, description, is_default) VALUES ($1,$2,$3,$4,$5) RETURNING *"#
    ).bind(Uuid::new_v4()).bind(tenant_id).bind(&req.name).bind(&req.description)
    .bind(req.is_default.unwrap_or(false)).fetch_one(&state.db).await
    .map_err(|e| match e {
        // `idx_pipelines_tenant_name_unique` (migration 110). The guard is what stops a re-seed creating a
        // duplicate "Sales Pipeline" (measured live: one tenant had two, 3 days apart), but a unique
        // violation on its own surfaces as a raw 500 for a name the operator can plainly see is taken.
        // The constraint is right; the answer has to say WHY — the same rule the other write paths follow.
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => {
            AppError::Validation(format!(
                "A pipeline named '{}' already exists on this account",
                req.name.trim()
            ))
        }
        other => other.into(),
    })?;
    Ok((StatusCode::CREATED, Json(json!(pipeline))))
}

pub async fn get_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline =
        sqlx::query_as::<_, Pipeline>("SELECT * FROM pipelines WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound(format!("Pipeline {} not found", id)))?;
    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!(PipelineWithStages { pipeline, stages })))
}

pub async fn update_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdatePipelineRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline = sqlx::query_as::<_, Pipeline>(
        r#"UPDATE pipelines SET name = COALESCE($1,name), description = COALESCE($2,description),
            is_default = COALESCE($3,is_default), is_active = COALESCE($4,is_active), updated_at = NOW()
           WHERE id = $5 AND tenant_id = $6 RETURNING *"#
    ).bind(&req.name).bind(&req.description).bind(req.is_default).bind(req.is_active)
    .bind(id).bind(tenant_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Pipeline {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        tenant_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "pipeline.updated",
        "pipeline",
        Some(id),
        Some(json!({"updated": true})),
        None,
    )
    .await;

    Ok(Json(json!(pipeline)))
}

pub async fn delete_pipeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    // The name, read before the row goes (see contacts::delete).
    let gone: Option<String> =
        sqlx::query_scalar("SELECT name FROM pipelines WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await?;
    let r = sqlx::query("DELETE FROM pipelines WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Pipeline {} not found", id)));
    }
    if let Some(name) = gone {
        audit::logger::log_event(
            &state.db,
            tenant_id,
            Some(user_id),
            "pipeline.deleted",
            "pipeline",
            Some(id),
            Some(json!({"name": name})),
            None,
        )
        .await;
    }
    Ok(Json(json!({"message": "Pipeline deleted successfully"})))
}

pub async fn list_stages(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND tenant_id = $2")
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!(
            "Pipeline {} not found",
            pipeline_id
        )))?;
    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position",
    )
    .bind(pipeline_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({ "stages": stages })))
}

pub async fn create_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
    Json(req): Json<CreateStageRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND tenant_id = $2")
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!(
            "Pipeline {} not found",
            pipeline_id
        )))?;
    let stage = sqlx::query_as::<_, PipelineStage>(
        r#"INSERT INTO pipeline_stages (id, pipeline_id, name, color, position, probability)
           VALUES ($1,$2,$3,$4,$5,$6) RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(pipeline_id)
    .bind(&req.name)
    .bind(&req.color)
    .bind(req.position.unwrap_or(0))
    .bind(req.probability)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(stage))))
}

pub async fn update_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((pipeline_id, stage_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateStageRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("SELECT 1 FROM pipelines WHERE id = $1 AND tenant_id = $2")
        .bind(pipeline_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!(
            "Pipeline {} not found",
            pipeline_id
        )))?;
    let stage = sqlx::query_as::<_, PipelineStage>(
        r#"UPDATE pipeline_stages SET name = COALESCE($1,name),
            color = COALESCE($2,color), position = COALESCE($3,position),
            is_won = COALESCE($4,is_won), is_lost = COALESCE($5,is_lost),
            probability = COALESCE($6,probability), updated_at = NOW()
           WHERE id = $7 AND pipeline_id = $8 RETURNING *"#,
    )
    .bind(&req.name)
    .bind(&req.color)
    .bind(req.position)
    .bind(req.is_won)
    .bind(req.is_lost)
    .bind(req.probability)
    .bind(stage_id)
    .bind(pipeline_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Stage {} not found", stage_id)))?;
    Ok(Json(json!(stage)))
}

pub async fn delete_stage(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((_pipeline_id, stage_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query(
        "DELETE FROM pipeline_stages WHERE id = $1 AND pipeline_id IN (SELECT id FROM pipelines WHERE tenant_id = $2)"
    ).bind(stage_id).bind(tenant_id).execute(&state.db).await?;
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
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let stage = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE id = $1 AND pipeline_id = $2",
    )
    .bind(stage_id)
    .bind(pipeline_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Stage {} not found", stage_id)))?;

    // `value` is NUMERIC in Postgres: decode it as float8 (see OPP_COLS) or sqlx rejects the row.
    let opp_sql = concat!(
        "SELECT ",
        opp_cols!(),
        " FROM opportunities WHERE id = $1 AND tenant_id = $2"
    );
    let opp = sqlx::query_as::<_, OpportunityFull>(opp_sql)
        .bind(opportunity_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound(format!(
            "Opportunity {} not found",
            opportunity_id
        )))?;

    let prev_stage = opp.stage_id;
    let status = if stage.is_won {
        "won"
    } else if stage.is_lost {
        "lost"
    } else {
        "open"
    };

    // opportunities has no `status` column: a won/lost stage flips the boolean flags.
    sqlx::query(
        "UPDATE opportunities SET stage_id = $1, is_won = $2, is_lost = $3, probability = $4, updated_at = NOW() WHERE id = $5"
    ).bind(stage_id).bind(stage.is_won).bind(stage.is_lost).bind(stage.probability).bind(opportunity_id).execute(&state.db).await?;

    sqlx::query(
        "INSERT INTO opportunity_stage_history (id, opportunity_id, from_stage_id, to_stage_id, moved_by) VALUES ($1,$2,$3,$4,$5)"
    ).bind(Uuid::new_v4()).bind(opportunity_id).bind(prev_stage).bind(stage_id).bind(user_id).execute(&state.db).await?;

    Ok(Json(
        json!({"message": "Opportunity moved", "from_stage_id": prev_stage, "to_stage_id": stage_id, "status": status}),
    ))
}

pub async fn pipeline_analytics(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(pipeline_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let pipeline =
        sqlx::query_as::<_, Pipeline>("SELECT * FROM pipelines WHERE id = $1 AND tenant_id = $2")
            .bind(pipeline_id)
            .bind(tenant_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound(format!(
                "Pipeline {} not found",
                pipeline_id
            )))?;

    let stages = sqlx::query_as::<_, PipelineStage>(
        "SELECT * FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position",
    )
    .bind(pipeline_id)
    .fetch_all(&state.db)
    .await?;

    let total_opps: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND tenant_id = $2",
    )
    .bind(pipeline_id)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    let won_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND tenant_id = $2 AND is_won = true",
    )
    .bind(pipeline_id)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    let lost_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM opportunities WHERE pipeline_id = $1 AND tenant_id = $2 AND is_lost = true",
    )
    .bind(pipeline_id)
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    // MONEY IS PER CURRENCY. There is deliberately no "total_value" here: this app has no
    // exchange rate for a deal that never stored one, and adding EUR to USD would produce a
    // figure that is not a sum of anything (David, 2026-11-20 — multi-currency is core).
    let by_currency = money_by_currency(&state.db, tenant_id, pipeline_id, None).await?;

    let closed = won_count + lost_count;
    let win_rate = if closed > 0 {
        won_count as f64 / closed as f64
    } else {
        0.0
    };

    let mut stage_analytics = Vec::new();
    for stage in &stages {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM opportunities WHERE stage_id = $1 AND tenant_id = $2",
        )
        .bind(stage.id)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;

        let avg_time: f64 = sqlx::query_scalar(
            r#"SELECT COALESCE(AVG(EXTRACT(EPOCH FROM (sh2.moved_at - sh1.moved_at)) / 86400.0), 0)::float8
               FROM opportunity_stage_history sh1
               JOIN opportunity_stage_history sh2 ON sh2.opportunity_id = sh1.opportunity_id
               WHERE sh1.to_stage_id = $1 AND sh2.from_stage_id = $1 AND sh2.moved_at > sh1.moved_at"#,
        )
        .bind(stage.id)
        .fetch_one(&state.db)
        .await?;

        let conv = if total_opps > 0 {
            (won_count as f64 / total_opps as f64) * 100.0
        } else {
            0.0
        };

        stage_analytics.push(StageAnalytics {
            stage_id: stage.id,
            stage_name: stage.name.clone(),
            count,
            avg_time_in_days: (avg_time * 100.0).round() / 100.0,
            conversion_rate: (conv * 100.0).round() / 100.0,
            by_currency: money_by_currency(&state.db, tenant_id, pipeline_id, Some(stage.id))
                .await?,
            reporting: reporting_money(&state.db, tenant_id, pipeline_id, Some(stage.id)).await?,
        });
    }

    Ok(Json(json!(PipelineAnalytics {
        pipeline_id,
        pipeline_name: pipeline.name,
        total_opportunities: total_opps,
        won_count,
        lost_count,
        win_rate: (win_rate * 1000.0).round() / 1000.0,
        by_currency,
        reporting: reporting_money(&state.db, tenant_id, pipeline_id, None).await?,
        stages: stage_analytics,
    })))
}

/// The same money, converted into the workspace's reporting currency from each deal's STORED
/// rate. This is the number a mixed-currency workspace reads when it wants ONE figure — and it is
/// derived from facts the deal kept, not from today's market, so a past period does not move.
async fn reporting_money(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    pipeline_id: Uuid,
    stage_id: Option<Uuid>,
) -> Result<ReportingMoney, AppError> {
    let reporting = crate::deals::fx::workspace_reporting_currency(db, tenant_id)
        .await?
        .unwrap_or_default();
    if reporting.is_empty() {
        return Ok(ReportingMoney {
            currency: String::new(),
            total_value: 0.0,
            weighted_value: 0.0,
            unconverted_count: 0,
        });
    }
    let row: (f64, f64, i64) = sqlx::query_as(
        "SELECT COALESCE(SUM(x.conv),0)::float8,
                COALESCE(SUM(x.conv * COALESCE(x.probability,0) / 100.0),0)::float8,
                COUNT(*) FILTER (WHERE x.conv IS NULL) AS unconverted
           FROM (
             SELECT o.probability,
                    CASE
                      WHEN upper(COALESCE(o.currency,'')) = upper($4) THEN o.value
                      WHEN o.exchange_rate IS NOT NULL
                           AND upper(COALESCE(o.reporting_currency, o.currency)) = upper($4)
                          THEN o.value * o.exchange_rate
                    END AS conv
               FROM opportunities o
              WHERE o.tenant_id = $1 AND o.pipeline_id = $2
                AND ($3::uuid IS NULL OR o.stage_id = $3)
           ) x",
    )
    .bind(tenant_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .bind(&reporting)
    .fetch_one(db)
    .await?;
    Ok(ReportingMoney {
        currency: reporting,
        total_value: row.0,
        weighted_value: row.1,
        unconverted_count: row.2,
    })
}

/// Deal value grouped by currency, for a whole pipeline or one stage of it.
///
/// One statement, one grouping key: currency is the dimension that makes the number meaningful,
/// and `weighted_value` (value x probability) is reported beside it so the board can show what is
/// actually expected rather than what is hoped for.
async fn money_by_currency(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    pipeline_id: Uuid,
    stage_id: Option<Uuid>,
) -> Result<Vec<CurrencyAmount>, AppError> {
    let rows: Vec<CurrencyAmount> = sqlx::query_as(
        "SELECT COALESCE(currency,'') AS currency,
                COUNT(*) AS deal_count,
                COALESCE(SUM(value),0)::float8 AS total_value,
                COALESCE(SUM(value * COALESCE(probability,0) / 100.0),0)::float8 AS weighted_value
           FROM opportunities
          WHERE tenant_id = $1 AND pipeline_id = $2
            AND ($3::uuid IS NULL OR stage_id = $3)
          GROUP BY COALESCE(currency,'')
          ORDER BY 1",
    )
    .bind(tenant_id)
    .bind(pipeline_id)
    .bind(stage_id)
    .fetch_all(db)
    .await?;
    Ok(rows)
}
