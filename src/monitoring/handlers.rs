//! Account health monitoring handlers.

use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;
use super::engine;

/// GET /api/monitoring/health — Get account health for current tenant
pub async fn get_health(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let entity_type = p.get("entity_type").and_then(|v| v.as_str()).unwrap_or("tenant");
    let entity_id = p.get("entity_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());

    let health = if let Some(eid) = entity_id {
        sqlx::query_as::<_, AccountHealth>(
            "SELECT * FROM account_health WHERE tenant_id = $1 AND entity_type = $2 AND entity_id = $3"
        ).bind(tid).bind(entity_type).bind(eid).fetch_optional(&s.db).await?
    } else {
        sqlx::query_as::<_, AccountHealth>(
            "SELECT * FROM account_health WHERE tenant_id = $1 AND entity_type = $2 ORDER BY updated_at DESC LIMIT 1"
        ).bind(tid).bind(entity_type).fetch_optional(&s.db).await?
    };

    match health {
        Some(h) => Ok(Json(json!(h))),
        None => Ok(Json(json!({"score": 100, "risk_level": "healthy", "message": "No signals recorded yet"}))),
    }
}

/// POST /api/monitoring/health — Record a health signal
pub async fn update_health_signal(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<SignalRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let value = r.value.unwrap_or(1);

    engine::record_signal(&s.db, tid, &r.entity_type, r.entity_id, &r.signal, value).await;

    Ok(Json(json!({"message": "Signal recorded", "signal": r.signal, "entity": r.entity_id})))
}

/// GET /api/monitoring/thresholds
pub async fn list_thresholds(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let thresholds = sqlx::query_as::<_, HealthThreshold>(
        "SELECT * FROM health_thresholds WHERE tenant_id = $1 ORDER BY name ASC"
    ).bind(tid).fetch_all(&s.db).await?;
    Ok(Json(json!({"thresholds": thresholds})))
}

/// POST /api/monitoring/thresholds
pub async fn create_threshold(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateThresholdRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if !["lt","gt","eq","lte","gte"].contains(&r.operator.as_str()) {
        return Err(AppError::Validation("operator must be lt, gt, eq, lte, or gte".to_string()));
    }

    let t = sqlx::query_as::<_, HealthThreshold>(
        r#"INSERT INTO health_thresholds (id, tenant_id, name, entity_type, metric, operator, value, risk_level, intervention_action, intervention_config)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(&r.name).bind(&r.entity_type)
    .bind(&r.metric).bind(&r.operator).bind(r.value).bind(&r.risk_level)
    .bind(r.intervention_action.unwrap_or_else(|| "send_notification".to_string()))
    .bind(r.intervention_config.unwrap_or(json!({})))
    .fetch_one(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(t))))
}

/// PATCH /api/monitoring/thresholds/{id}
pub async fn update_threshold(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateThresholdRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let t = sqlx::query_as::<_, HealthThreshold>(
        r#"UPDATE health_thresholds SET name=COALESCE($1,name), value=COALESCE($2,value), risk_level=COALESCE($3,risk_level), intervention_action=COALESCE($4,intervention_action), is_active=COALESCE($5,is_active) WHERE id=$6 AND tenant_id=$7 RETURNING *"#
    ).bind(&r.name).bind(r.value).bind(&r.risk_level).bind(&r.intervention_action).bind(r.is_active).bind(id).bind(tid)
    .fetch_one(&s.db).await?;
    Ok(Json(json!(t)))
}

/// DELETE /api/monitoring/thresholds/{id}
pub async fn delete_threshold(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("DELETE FROM health_thresholds WHERE id=$1 AND tenant_id=$2").bind(id).bind(tid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Deleted"})))
}
