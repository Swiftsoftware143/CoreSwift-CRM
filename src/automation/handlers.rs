use super::models::*;
use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

pub async fn list_rules(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(
        json!({"rules": sqlx::query_as::<_,AutomationRule>(&format!("SELECT {} FROM automation_rules WHERE tenant_id=$1 ORDER BY name", RULE_COLUMNS)).bind(t).fetch_all(&s.db).await?}),
    ))
}

pub async fn create_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateRuleRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() {
        return Err(AppError::Validation("Name required".into()));
    }
    let valid_t = [
        "TagAdded",
        "TagRemoved",
        "StageChanged",
        "ScoreChanged",
        "ListAdded",
        "ListRemoved",
        "tag.assigned",
        "tag.unassigned",
    ];
    if !valid_t.contains(&r.trigger_type.as_str()) {
        return Err(AppError::Validation("Invalid trigger_type".into()));
    }
    let valid_a = [
        "AddTag",
        "RemoveTag",
        "MovePipeline",
        "AddToList",
        "RemoveFromList",
        "Webhook",
        "NotifyUser",
        "send_email",
        "send_sms",
        "pipeline.move",
        "scoring.update",
    ];
    if !valid_a.contains(&r.action_type.as_str()) {
        return Err(AppError::Validation("Invalid action_type".into()));
    }
    // `trigger_type` / `action_type` are plain `character varying(50)` columns in this database —
    // the removed `::trigger_type` / `::action_type` casts pointed at enum types that do not
    // exist (pg_type has business_unit, channel_type, user_role, user_state — nothing else), which
    // is why this INSERT had never succeeded for anyone.
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,AutomationRule>(&format!("INSERT INTO automation_rules(id,tenant_id,name,description,trigger_type,trigger_config,action_type,action_config) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING {}", RULE_COLUMNS))
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.description).bind(&r.trigger_type).bind(&r.trigger_config).bind(&r.action_type).bind(&r.action_config).fetch_one(&s.db).await?))))
}

pub async fn get_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_, AutomationRule>(&format!(
        "SELECT {} FROM automation_rules WHERE id=$1 AND tenant_id=$2",
        RULE_COLUMNS
    ))
    .bind(id)
    .bind(t)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound(format!("Rule {id} not found")))?)))
}

pub async fn update_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateRuleRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // `is_active` is the real column (nullable). COALESCE keeps the card's semantics:
    // a NULL request value means "leave as is"; the read path reports NULL as enabled.
    Ok(Json(json!(sqlx::query_as::<_,AutomationRule>(&format!("UPDATE automation_rules SET name=COALESCE($1,name), description=COALESCE($2,description), trigger_type=COALESCE($3,trigger_type), trigger_config=COALESCE($4,trigger_config), action_type=COALESCE($5,action_type), action_config=COALESCE($6,action_config), is_active=COALESCE($7,is_active), updated_at=NOW() WHERE id=$8 AND tenant_id=$9 RETURNING {}", RULE_COLUMNS))
        .bind(&r.name).bind(&r.description).bind(&r.trigger_type).bind(&r.trigger_config).bind(&r.action_type).bind(&r.action_config).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Rule {id} not found")))?)))
}

pub async fn delete_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM automation_rules WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Rule {id} not found")));
    }
    Ok(Json(json!({"message":"Deleted"})))
}
