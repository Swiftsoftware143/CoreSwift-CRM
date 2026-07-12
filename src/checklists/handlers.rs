//! Checklist handlers: CRUD for templates, instance management, progress tracking.

use axum::{
    extract::{State, Path, Query, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;
use super::models::*;

#[derive(Debug, Deserialize)]
pub struct ListTemplatesParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub trigger_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListInstancesParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
}

/// GET /api/checklists/templates — List checklist templates.
pub async fn list_templates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListTemplatesParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    let templates = if let Some(ref trigger_type) = params.trigger_type {
        sqlx::query_as::<_, ChecklistTemplate>(
            r#"SELECT * FROM checklist_templates
               WHERE tenant_id = $1 AND trigger_type = $2
               ORDER BY created_at DESC
               LIMIT $3 OFFSET $4"#,
        )
        .bind(tenant_id)
        .bind(trigger_type)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ChecklistTemplate>(
            r#"SELECT * FROM checklist_templates
               WHERE tenant_id = $1
               ORDER BY created_at DESC
               LIMIT $2 OFFSET $3"#,
        )
        .bind(tenant_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(json!({ "templates": templates, "page": page, "per_page": per_page })))
}

/// POST /api/checklists/templates — Create a new checklist template.
pub async fn create_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    if req.name.is_empty() {
        return Err(AppError::Validation("Template name is required".to_string()));
    }

    if req.trigger_type.is_empty() {
        return Err(AppError::Validation("Trigger type is required".to_string()));
    }

    let template = sqlx::query_as::<_, ChecklistTemplate>(
        r#"INSERT INTO checklist_templates (id, tenant_id, name, description, trigger_type, stage_count, days_per_stage)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.trigger_type)
    .bind(req.stage_count.unwrap_or(4))
    .bind(req.days_per_stage.unwrap_or(2))
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(template))))
}

/// GET /api/checklists/templates/{id} — Get a single template with stages.
pub async fn get_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let template = sqlx::query_as::<_, ChecklistTemplate>(
        "SELECT * FROM checklist_templates WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Template {} not found", id)))?;

    let stages = sqlx::query_as::<_, ChecklistStage>(
        "SELECT * FROM checklist_stages WHERE template_id = $1 ORDER BY stage_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "template": template, "stages": stages })))
}

/// PATCH /api/checklists/templates/{id} — Update a template.
pub async fn update_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateTemplateRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let template = sqlx::query_as::<_, ChecklistTemplate>(
        r#"UPDATE checklist_templates SET
            name = COALESCE($1, name),
            description = COALESCE($2, description),
            trigger_type = COALESCE($3, trigger_type),
            stage_count = COALESCE($4, stage_count),
            days_per_stage = COALESCE($5, days_per_stage),
            is_active = COALESCE($6, is_active),
            updated_at = NOW()
           WHERE id = $7 AND tenant_id = $8
           RETURNING *"#,
    )
    .bind(&req.name)
    .bind(&req.description)
    .bind(&req.trigger_type)
    .bind(req.stage_count)
    .bind(req.days_per_stage)
    .bind(req.is_active)
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Template {} not found", id)))?;

    Ok(Json(json!(template)))
}

/// DELETE /api/checklists/templates/{id} — Delete a template.
pub async fn delete_template(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM checklist_templates WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Template {} not found", id)));
    }

    Ok(Json(json!({ "message": "Template deleted successfully" })))
}

// ====== Instances ======

/// POST /api/checklists/instances/start/{entity_type}/{entity_id} — Start a checklist instance.
pub async fn start_checklist(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((entity_type, entity_id)): Path<(String, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    // Find matching active template
    let template = sqlx::query_as::<_, (Uuid, i32, i32)>(
        "SELECT id, stage_count, days_per_stage FROM checklist_templates WHERE tenant_id = $1 AND is_active = true ORDER BY created_at DESC LIMIT 1"
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("No active checklist templates found".to_string()))?;

    let (template_id, stage_count, _days_per_stage) = template;

    // Create the instance
    let instance = sqlx::query_as::<_, ChecklistInstance>(
        r#"INSERT INTO checklist_instances (id, tenant_id, template_id, entity_type, entity_id, current_stage)
           VALUES ($1, $2, $3, $4, $5, 0)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(template_id)
    .bind(&entity_type)
    .bind(entity_id)
    .fetch_one(&state.db)
    .await?;

    // Create progress rows for each stage
    let stages = sqlx::query_as::<_, ChecklistStage>(
        "SELECT * FROM checklist_stages WHERE template_id = $1 ORDER BY stage_order ASC",
    )
    .bind(template_id)
    .fetch_all(&state.db)
    .await?;

    for stage in &stages {
        let _ = sqlx::query(
            r#"INSERT INTO checklist_progress (id, instance_id, stage_order)
               VALUES ($1, $2, $3)"#,
        )
        .bind(Uuid::new_v4())
        .bind(instance.id)
        .bind(stage.stage_order)
        .execute(&state.db)
        .await;
    }

    Ok((StatusCode::CREATED, Json(json!({
        "instance": instance,
        "stage_count": stage_count,
        "stages_initialized": stages.len(),
    }))))
}

/// PATCH /api/checklists/instances/{id}/progress — Update stage progress.
pub async fn update_progress(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let stage = req.get("stage_order")
        .and_then(|v| v.as_i64())
        .ok_or(AppError::Validation("stage_order required".to_string()))?;

    let action_taken = req.get("action_taken")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");

    // Mark this stage complete
    let _ = sqlx::query(
        r#"UPDATE checklist_progress SET
            completed = true,
            action_taken = $1,
            completed_at = NOW()
           WHERE instance_id IN (SELECT id FROM checklist_instances WHERE tenant_id = $3)
           AND stage_order = $2"#,
    )
    .bind(action_taken)
    .bind(stage)
    .bind(tenant_id)
    .execute(&state.db)
    .await?;

    // Check if all stages are complete
    let (total, done) = sqlx::query_as::<_, (i64, i64)>(
        r#"SELECT
            (SELECT stage_count FROM checklist_templates ct
             JOIN checklist_instances ci ON ci.template_id = ct.id
             WHERE ci.id = $1),
            (SELECT COUNT(*) FROM checklist_progress WHERE instance_id = $1 AND completed = true)"#,
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;

    if done >= total {
        let _ = sqlx::query(
            "UPDATE checklist_instances SET completed = true, completed_at = NOW() WHERE id = $1"
        )
        .bind(id)
        .execute(&state.db)
        .await;
    }

    Ok(Json(json!({
        "message": "Progress updated",
        "stage_completed": done,
        "total_stages": total,
    })))
}

/// GET /api/checklists/instances — List checklist instances.
pub async fn list_instances(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListInstancesParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    let instances = if let (Some(ref entity_type), Some(entity_id)) = (&params.entity_type, params.entity_id) {
        sqlx::query_as::<_, ChecklistInstance>(
            r#"SELECT * FROM checklist_instances
               WHERE tenant_id = $1 AND entity_type = $2 AND entity_id = $3
               ORDER BY created_at DESC
               LIMIT $4 OFFSET $5"#,
        )
        .bind(tenant_id)
        .bind(entity_type)
        .bind(entity_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ChecklistInstance>(
            r#"SELECT * FROM checklist_instances
               WHERE tenant_id = $1
               ORDER BY created_at DESC
               LIMIT $2 OFFSET $3"#,
        )
        .bind(tenant_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(json!({ "instances": instances, "page": page, "per_page": per_page })))
}

/// GET /api/checklists/instances/{id} — Get instance with progress.
pub async fn get_instance(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let instance = sqlx::query_as::<_, ChecklistInstance>(
        "SELECT * FROM checklist_instances WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Instance {} not found", id)))?;

    let progress = sqlx::query_as::<_, ChecklistProgress>(
        "SELECT * FROM checklist_progress WHERE instance_id = $1 ORDER BY stage_order ASC",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "instance": instance, "progress": progress })))
}
