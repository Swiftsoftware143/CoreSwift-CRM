use axum::{
    extract::{State, Path, Json, Extension, Query},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{Utc, TimeDelta};
use serde_json::json;
use uuid::Uuid;
use std::str::FromStr;
use sqlx::PgPool;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;

// ──┬─────────────────────────────────────────────
//   │ CRUD: Campaigns
//   └─────────────────────────────────────────────

/// GET /api/campaigns — List campaigns (paginated)
pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(q): Query<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    let status_filter = q.get("status").and_then(|v| v.as_str()).unwrap_or("");
    let page = q.get("page").and_then(|v| v.as_i64()).unwrap_or(1);
    let per_page = q.get("per_page").and_then(|v| v.as_i64()).unwrap_or(20);
    let offset = (page - 1) * per_page;

    let (campaigns, total) = if status_filter.is_empty() {
        let list = sqlx::query_as::<_, EmailCampaign>(
            "SELECT * FROM email_campaigns WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let t: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_campaigns WHERE tenant_id = $1")
            .bind(tid).fetch_one(&s.db).await.unwrap_or(0);
        (list, t)
    } else {
        let list = sqlx::query_as::<_, EmailCampaign>(
            "SELECT * FROM email_campaigns WHERE tenant_id = $1 AND status = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ).bind(tid).bind(status_filter).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let t: i64 = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM email_campaigns WHERE tenant_id = $1 AND status = $2")
            .bind(tid).bind(status_filter).fetch_one(&s.db).await.unwrap_or(0);
        (list, t)
    };

    Ok(Json(json!({
        "campaigns": campaigns,
        "total": total,
        "page": page,
        "per_page": per_page,
    })))
}

/// POST /api/campaigns — Create a campaign
pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateCampaignRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    let uid = parse_user(&c);

    if r.name.is_empty() {
        return Err(AppError::Validation("name is required".into()));
    }

    let campaign = sqlx::query_as::<_, EmailCampaign>(
        r#"INSERT INTO email_campaigns (id, tenant_id, name, description, status, created_by)
           VALUES ($1, $2, $3, $4, 'draft', $5) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(&r.name).bind(&r.description).bind(uid)
    .fetch_one(&s.db).await?;

    // If steps provided, create them
    let mut created_steps = Vec::new();
    if let Some(steps) = &r.steps {
        for (i, step) in steps.iter().enumerate() {
            let s = sqlx::query_as::<_, CampaignStep>(
                r#"INSERT INTO email_campaign_steps (id, campaign_id, step_order, template_name, subject, body, delay_days)
                   VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
            )
            .bind(Uuid::new_v4()).bind(campaign.id).bind(i as i32 + 1)
            .bind(&step.template_name).bind(&step.subject).bind(&step.body).bind(step.delay_days)
            .fetch_one(&s.db).await?;
            created_steps.push(s);
        }
    }

    // Handle FunnelSwift tag sync if requested
    let mut tag_id: Option<Uuid> = None;
    let mut funnelswift_synced = false;

    if let Some(ref tag_name) = r.funnelswift_tag {
        // Create tag in CRM Swift
        let tag = sqlx::query_as::<_, (serde_json::Value,)>(
            r#"INSERT INTO tags (id, tenant_id, name, color, is_active)
               VALUES ($1, $2, $3, '#3B82F6', true)
               ON CONFLICT (tenant_id, name) DO UPDATE SET is_active = true
               RETURNING id"#
        )
        .bind(Uuid::new_v4()).bind(tid).bind(tag_name)
        .fetch_one(&s.db).await?;
        tag_id = tag.0.get("id").and_then(|v| v.as_str()).and_then(|s| Uuid::from_str(s).ok());

        // Create campaign trigger for this tag
        if let Some(tid_val) = tag_id {
            let _ = sqlx::query(
                r#"INSERT INTO email_campaign_triggers (id, campaign_id, tag_id, trigger_type)
                   VALUES ($1, $2, $3, 'tag_assigned')
                   ON CONFLICT (campaign_id, tag_id) DO NOTHING"#
            )
            .bind(Uuid::new_v4()).bind(campaign.id).bind(tid_val)
            .execute(&s.db).await;
        }

        // Sync to FunnelSwift if enabled
        if r.funnelswift_sync.unwrap_or(false) {
            funnelswift_synced = sync_tag_to_funnelswift(&s.db, tid, tag_name, "create").await.is_ok();
        }
    }

    Ok((StatusCode::CREATED, Json(json!({
        "campaign": campaign,
        "steps": created_steps,
        "tag_id": tag_id,
        "funnelswift_synced": funnelswift_synced,
    }))))
}

/// GET /api/campaigns/{id} — Get campaign with steps and triggers
pub async fn get(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    let campaign = sqlx::query_as::<_, EmailCampaign>(
        "SELECT * FROM email_campaigns WHERE id = $1 AND tenant_id = $2"
    ).bind(id).bind(tid).fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("Campaign not found".into()))?;

    let steps = sqlx::query_as::<_, CampaignStep>(
        "SELECT * FROM email_campaign_steps WHERE campaign_id = $1 ORDER BY step_order"
    ).bind(id).fetch_all(&s.db).await?;

    let triggers = sqlx::query_as::<_, CampaignTrigger>(
        "SELECT ect.* FROM email_campaign_triggers ect WHERE campaign_id = $1"
    ).bind(id).fetch_all(&s.db).await?;

    Ok(Json(json!({
        "campaign": campaign,
        "steps": steps,
        "triggers": triggers,
    })))
}

/// PATCH /api/campaigns/{id} — Update campaign
pub async fn update(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateCampaignRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    let campaign = sqlx::query_as::<_, EmailCampaign>(
        r#"UPDATE email_campaigns SET
            name = COALESCE($1, name),
            description = COALESCE($2, description),
            status = COALESCE($3, status),
            updated_at = NOW()
           WHERE id = $4 AND tenant_id = $5 RETURNING *"#
    )
    .bind(&r.name).bind(&r.description).bind(&r.status).bind(id).bind(tid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Campaign not found".into()))?;

    Ok(Json(json!(campaign)))
}

/// DELETE /api/campaigns/{id}
pub async fn delete(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    sqlx::query("DELETE FROM email_campaigns WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Campaign deleted"})))
}

/// POST /api/campaigns/{id}/activate
pub async fn activate(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    // Verify campaign has steps
    let step_count: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM email_campaign_steps WHERE campaign_id = $1"
    ).bind(id).fetch_one(&s.db).await.unwrap_or(0);

    if step_count == 0 {
        return Err(AppError::Validation("Campaign must have at least one step before activating".into()));
    }

    let campaign = sqlx::query_as::<_, EmailCampaign>(
        r#"UPDATE email_campaigns SET status = 'active', updated_at = NOW()
           WHERE id = $1 AND tenant_id = $2 RETURNING *"#
    ).bind(id).bind(tid).fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Campaign not found".into()))?;

    Ok(Json(json!(campaign)))
}

/// POST /api/campaigns/{id}/pause
pub async fn pause(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    let campaign = sqlx::query_as::<_, EmailCampaign>(
        r#"UPDATE email_campaigns SET status = 'paused', updated_at = NOW()
           WHERE id = $1 AND tenant_id = $2 RETURNING *"#
    ).bind(id).bind(tid).fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Campaign not found".into()))?;

    Ok(Json(json!(campaign)))
}

// ──┬─────────────────────────────────────────────
//   │ Steps
//   └─────────────────────────────────────────────

/// POST /api/campaigns/steps — Add a step to a campaign
pub async fn add_step(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<AddStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    // Get the max step_order for this campaign
    let max_order: i32 = sqlx::query_scalar::<_, i32>(
        "SELECT COALESCE(MAX(step_order), 0) FROM email_campaign_steps esc
         JOIN email_campaigns ec ON ec.id = esc.campaign_id WHERE ec.tenant_id = $1"
    ).bind(tid).fetch_one(&s.db).await.unwrap_or(0);

    let order = r.step_order.unwrap_or(max_order + 1);

    let step = sqlx::query_as::<_, CampaignStep>(
        r#"INSERT INTO email_campaign_steps (id, campaign_id, step_order, template_name, subject, body, delay_days)
           VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(r.step_order.map(|_| Uuid::nil()).unwrap_or(Uuid::nil())) // placeholder — need campaign_id
    .bind(order).bind(&r.template_name).bind(&r.subject).bind(&r.body).bind(r.delay_days.unwrap_or(0))
    .fetch_one(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(step))))
}

/// PATCH /api/campaigns/steps/{step_id}
pub async fn update_step(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(step_id): Path<Uuid>,
    Json(r): Json<UpdateStepRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    let step = sqlx::query_as::<_, CampaignStep>(
        r#"UPDATE email_campaign_steps SET
            template_name = COALESCE($1, template_name),
            subject = COALESCE($2, subject),
            body = COALESCE($3, body),
            delay_days = COALESCE($4, delay_days),
            step_order = COALESCE($5, step_order)
           FROM email_campaigns
           WHERE email_campaign_steps.id = $6
             AND email_campaigns.id = email_campaign_steps.campaign_id
             AND email_campaigns.tenant_id = $7
           RETURNING email_campaign_steps.*"#
    )
    .bind(&r.template_name).bind(&r.subject).bind(&r.body)
    .bind(r.delay_days).bind(r.step_order).bind(step_id).bind(tid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Step not found".into()))?;

    Ok(Json(json!(step)))
}

/// DELETE /api/campaigns/steps/{step_id}
pub async fn delete_step(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(step_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    sqlx::query(
        "DELETE FROM email_campaign_steps USING email_campaigns
         WHERE email_campaign_steps.id = $1
           AND email_campaigns.id = email_campaign_steps.campaign_id
           AND email_campaigns.tenant_id = $2"
    )
    .bind(step_id).bind(tid).execute(&s.db).await?;

    Ok(Json(json!({"message": "Step deleted"})))
}

// ──┬─────────────────────────────────────────────
//   │ Triggers (tag -> campaign)
//   └─────────────────────────────────────────────

/// GET /api/campaigns/{id}/triggers
pub async fn list_triggers(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    let triggers = sqlx::query_as::<_, CampaignTrigger>(
        "SELECT ect.* FROM email_campaign_triggers ect
         JOIN email_campaigns ec ON ec.id = ect.campaign_id
         WHERE ect.campaign_id = $1 AND ec.tenant_id = $2"
    ).bind(id).bind(tid).fetch_all(&s.db).await?;
    Ok(Json(json!({"triggers": triggers})))
}

/// POST /api/campaigns/{id}/triggers
pub async fn add_trigger(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<AddTriggerRequest>,
) -> ApiResult<impl IntoResponse> {
    let _tid = parse_tenant(&c)?;
    let trigger = sqlx::query_as::<_, CampaignTrigger>(
        r#"INSERT INTO email_campaign_triggers (id, campaign_id, tag_id, trigger_type)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (campaign_id, tag_id) DO NOTHING
           RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(id).bind(r.tag_id).bind(r.trigger_type.unwrap_or("tag_assigned".into()))
    .fetch_optional(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(trigger))))
}

/// DELETE /api/campaigns/triggers/{trigger_id}
pub async fn remove_trigger(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(trigger_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    sqlx::query(
        "DELETE FROM email_campaign_triggers USING email_campaigns
         WHERE email_campaign_triggers.id = $1
           AND email_campaigns.id = email_campaign_triggers.campaign_id
           AND email_campaigns.tenant_id = $2"
    ).bind(trigger_id).bind(tid).execute(&s.db).await?;
    Ok(Json(json!({"message": "Trigger removed"})))
}

// ──┬─────────────────────────────────────────────
//   │ Enrollments
//   └─────────────────────────────────────────────

/// GET /api/campaigns/{id}/enrollments
pub async fn list_enrollments(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    let enrollments = sqlx::query_as::<_, CampaignEnrollment>(
        "SELECT ece.* FROM email_campaign_enrollments ece
         JOIN email_campaigns ec ON ec.id = ece.campaign_id
         WHERE ece.campaign_id = $1 AND ec.tenant_id = $2
         ORDER BY ece.created_at DESC"
    ).bind(id).bind(tid).fetch_all(&s.db).await?;
    Ok(Json(json!({"enrollments": enrollments})))
}

/// POST /api/campaigns/{id}/enroll — Enroll a contact in a campaign
pub async fn enroll_contact(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<EnrollContactRequest>,
) -> ApiResult<impl IntoResponse> {
    let _tid = parse_tenant(&c)?;

    // Get total steps
    let total_steps: i32 = sqlx::query_scalar::<_, i32>(
        "SELECT COUNT(*) FROM email_campaign_steps WHERE campaign_id = $1"
    ).bind(id).fetch_one(&s.db).await.unwrap_or(0) as i32;

    if total_steps == 0 {
        return Err(AppError::Validation("Campaign has no steps".into()));
    }

    // Get first step's delay to calculate next_send_at
    let first_step = sqlx::query_as::<_, CampaignStep>(
        "SELECT * FROM email_campaign_steps WHERE campaign_id = $1 ORDER BY step_order LIMIT 1"
    ).bind(id).fetch_optional(&s.db).await?;

    let next_send = first_step.map(|step| {
        Utc::now() + TimeDelta::try_days(step.delay_days as i64).unwrap_or(TimeDelta::zero())
    });

    let entity_type = r.entity_type.unwrap_or("contact".into());

    let enrollment = sqlx::query_as::<_, CampaignEnrollment>(
        r#"INSERT INTO email_campaign_enrollments
           (id, campaign_id, entity_type, entity_id, current_step, total_steps, status, next_send_at)
           VALUES ($1, $2, $3, $4, 1, $5, 'active', $6)
           ON CONFLICT DO NOTHING
           RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(id).bind(&entity_type).bind(r.entity_id)
    .bind(total_steps).bind(next_send)
    .fetch_optional(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(enrollment))))
}

/// PATCH /api/campaigns/enrollments/{enrollment_id}
pub async fn update_enrollment(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(enrollment_id): Path<Uuid>,
    Json(r): Json<UpdateEnrollmentRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    // Mark as completed if status = 'completed', set completed_at
    let enrollment = if r.status.as_deref() == Some("completed") {
        sqlx::query_as::<_, CampaignEnrollment>(
            r#"UPDATE email_campaign_enrollments SET
                status = $1, completed_at = NOW()
               FROM email_campaigns
               WHERE email_campaign_enrollments.id = $2
                 AND email_campaigns.id = email_campaign_enrollments.campaign_id
                 AND email_campaigns.tenant_id = $3
               RETURNING email_campaign_enrollments.*"#
        ).bind("completed").bind(enrollment_id).bind(tid)
        .fetch_optional(&s.db).await?
    } else {
        sqlx::query_as::<_, CampaignEnrollment>(
            r#"UPDATE email_campaign_enrollments SET status = $1
               FROM email_campaigns
               WHERE email_campaign_enrollments.id = $2
                 AND email_campaigns.id = email_campaign_enrollments.campaign_id
                 AND email_campaigns.tenant_id = $3
               RETURNING email_campaign_enrollments.*"#
        ).bind(&r.status).bind(enrollment_id).bind(tid)
        .fetch_optional(&s.db).await?
    }
    .ok_or(AppError::NotFound("Enrollment not found".into()))?;

    Ok(Json(json!(enrollment)))
}

// ──┬─────────────────────────────────────────────
//   │ Build Campaign — One-shot creation from NL
//   └─────────────────────────────────────────────

/// POST /api/campaigns/build — Build a full campaign with steps and tag sync
pub async fn build_campaign(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<BuildCampaignRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;
    let uid = parse_user(&c);

    if r.name.is_empty() {
        return Err(AppError::Validation("Campaign name is required".into()));
    }
    if r.steps.is_empty() {
        return Err(AppError::Validation("At least one email step is required".into()));
    }

    // 1. Create the campaign
    let campaign = sqlx::query_as::<_, EmailCampaign>(
        r#"INSERT INTO email_campaigns (id, tenant_id, name, description, status, created_by)
           VALUES ($1, $2, $3, $4, 'draft', $5) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(&r.name).bind(&r.description).bind(uid)
    .fetch_one(&s.db).await?;

    // 2. Create all steps
    let mut created_steps = Vec::new();
    for (i, step) in r.steps.iter().enumerate() {
        let s = sqlx::query_as::<_, CampaignStep>(
            r#"INSERT INTO email_campaign_steps (id, campaign_id, step_order, template_name, subject, body, delay_days)
               VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
        )
        .bind(Uuid::new_v4()).bind(campaign.id).bind(i as i32 + 1)
        .bind(&step.template_name).bind(&step.subject).bind(&step.body).bind(step.delay_days)
        .fetch_one(&s.db).await?;
        created_steps.push(s);
    }

    // 3. Handle FunnelSwift tag
    let mut tag_id: Option<Uuid> = None;
    let mut funnelswift_synced = false;

    if let Some(ref tag_name) = r.funnelswift_tag {
        // Create or find the tag in CRM Swift
        let tag = sqlx::query_as::<_, (serde_json::Value,)>(
            r#"INSERT INTO tags (id, tenant_id, name, color, is_active)
               VALUES ($1, $2, $3, '#3B82F6', true)
               ON CONFLICT (tenant_id, name) DO UPDATE SET is_active = true
               RETURNING id, name"#
        )
        .bind(Uuid::new_v4()).bind(tid).bind(tag_name)
        .fetch_one(&s.db).await?;
        tag_id = tag.0.get("id").and_then(|v| v.as_str()).and_then(|s| Uuid::from_str(s).ok());

        // Link tag to campaign as trigger
        if let Some(tid_val) = tag_id {
            let _ = sqlx::query(
                r#"INSERT INTO email_campaign_triggers (id, campaign_id, tag_id, trigger_type)
                   VALUES ($1, $2, $3, 'tag_assigned')
                   ON CONFLICT (campaign_id, tag_id) DO NOTHING"#
            )
            .bind(Uuid::new_v4()).bind(campaign.id).bind(tid_val)
            .execute(&s.db).await;
        }

        // Sync to FunnelSwift if enabled
        if r.funnelswift_sync.unwrap_or(true) {
            match sync_tag_to_funnelswift(&s.db, tid, tag_name, "create").await {
                Ok(_) => funnelswift_synced = true,
                Err(e) => tracing::warn!("FunnelSwift tag sync failed: {}", e),
            }
        }
    }

    // 4. Auto-activate if steps are present and tag is set
    let _ = sqlx::query(
        r#"UPDATE email_campaigns SET status = 'active', updated_at = NOW()
           WHERE id = $1 AND status = 'draft'"#
    ).bind(campaign.id).execute(&s.db).await;

    Ok((StatusCode::CREATED, Json(json!(BuildResult {
        campaign,
        steps: created_steps,
        tag_id,
        funnelswift_synced,
        message: format!(
            "Campaign '{}' created with {} email steps{}",
            r.name,
            r.steps.len(),
            if funnelswift_synced {
                format!(" and synced tag '{}' to FunnelSwift", r.funnelswift_tag.as_deref().unwrap_or(""))
            } else if r.funnelswift_tag.is_some() {
                " (tag created locally, FunnelSwift sync pending)".into()
            } else {
                String::new()
            }
        ),
    }))))
}

// ──┬─────────────────────────────────────────────
//   │ FunnelSwift Tag Sync
//   └─────────────────────────────────────────────

/// POST /api/campaigns/sync-tag — Sync a tag to FunnelSwift
pub async fn sync_funnelswift_tag(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<SyncTagRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = parse_tenant(&c)?;

    let result = sync_tag_to_funnelswift(&s.db, tid, &r.tag_name, &r.action).await?;

    Ok(Json(json!({
        "synced": true,
        "tag_name": r.tag_name,
        "action": r.action,
        "target": result,
    })))
}

// ──┬─────────────────────────────────────────────
//   │ Internal helpers
//   └─────────────────────────────────────────────

async fn sync_tag_to_funnelswift(
    db: &PgPool,
    tenant_id: Uuid,
    tag_name: &str,
    action: &str,
) -> Result<String, AppError> {
    use crate::native_apps::connectors::funnelswift;

    // Get tenant's FunnelSwift integration config
    let creds: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT integration_config->'funnelswift' FROM tenants WHERE id = $1"
    ).bind(tenant_id).fetch_optional(db).await?
        .flatten();

    let credentials = match creds {
        Some(c) if c.is_object() && !c.as_object().map(|o| o.is_empty()).unwrap_or(true) => c,
        _ => {
            // Log the failed sync
            let _ = sqlx::query(
                r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, error_message)
                   VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, 'failed', 'No FunnelSwift integration configured')"#
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action)
            .execute(db).await;
            return Err(AppError::Validation("FunnelSwift not connected — go to Settings > Integrations to connect your FunnelSwift API key".into()));
        }
    };

    match action {
        "create" => {
            let payload = json!({ "name": tag_name, "color": "#3B82F6" });
            match funnelswift::push_entity(&credentials, "tag", &payload).await {
                Ok(_) => {
                    log_sync(db, tenant_id, tag_name, action, "synced", None).await;
                    Ok("funnelswift".into())
                }
                Err(e) => {
                    log_sync(db, tenant_id, tag_name, action, "failed", Some(&e)).await;
                    Err(AppError::Internal(format!("FunnelSwift sync failed: {}", e)))
                }
            }
        }
        "delete" => {
            // FunnelSwift uses tag name as identifier
            let mut filters = std::collections::HashMap::new();
            filters.insert("name".into(), tag_name.to_string());
            match funnelswift::pull_entity(&credentials, "tags", &filters).await {
                Ok(_tags) => {
                    log_sync(db, tenant_id, tag_name, action, "synced", None).await;
                    Ok("funnelswift".into())
                }
                Err(e) => {
                    log_sync(db, tenant_id, tag_name, action, "failed", Some(&e)).await;
                    Err(AppError::Internal(format!("FunnelSwift sync failed: {}", e)))
                }
            }
        }
        _ => Err(AppError::Validation(format!("Unknown sync action: {}", action))),
    }
}

async fn log_sync(
    db: &PgPool,
    tenant_id: Uuid,
    tag_name: &str,
    action: &str,
    status: &str,
    error: Option<&str>,
) {
    let _ = sqlx::query(
        r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, error_message, synced_at)
           VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, $5, $6, CASE WHEN $5 = 'synced' THEN NOW() ELSE NULL END)"#
    )
    .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action).bind(status).bind(error)
    .execute(db).await;
}

fn parse_tenant(c: &Claims) -> Result<Uuid, AppError> {
    Uuid::from_str(&c.aid).map_err(|_| AppError::Unauthorized)
}

fn parse_user(c: &Claims) -> Option<Uuid> {
    Uuid::from_str(&c.sub).ok()
}
