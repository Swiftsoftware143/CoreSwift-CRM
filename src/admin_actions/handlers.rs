//! Chat action handlers — single endpoint to orchestrate multi-app flows

use axum::{
    extract::{Extension, Json, Query, State},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Claims;
use crate::errors::{validate_pagination, ApiResult, AppError};
use crate::security::email_addr;
use crate::sql_json::row_json_dml;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ChatActionRequest {
    pub intent: String,
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct ChatActionResult {
    pub intent: String,
    pub success: bool,
    pub message: String,
    pub results: serde_json::Value,
    pub next_steps: Vec<StepPrompt>,
    pub missing_fields: Vec<FieldRequest>,
}

#[derive(Debug, Serialize)]
pub struct StepPrompt {
    pub step: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct FieldRequest {
    pub field: String,
    pub label: String,
    pub field_type: String, // "text" | "email" | "number" | "select"
    pub required: bool,
    pub options: Option<Vec<String>>, // for select fields
}

/// POST /api/admin/chat-action — Execute a business action from chat
pub async fn execute_chat_action(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<ChatActionRequest>,
) -> ApiResult<impl IntoResponse> {
    // Every route in this router is gated by `auth::platform_admin` (see `super::router`), so a
    // caller that reaches this handler has already been resolved, from the database, as a platform
    // operator. The line that used to sit here —
    // `let _is_admin = c.role == "owner" || c.role == "admin";` — was bound to `_` and never read,
    // so the check it appeared to make never ran at all (kanban t_d5cf6cad).
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let result: Result<axum::response::Response, AppError> = match r.intent.as_str() {
        "create_tenant_account" => handle_create_tenant_account(&s, r.params)
            .await
            .map(IntoResponse::into_response),
        "build_campaign" => handle_build_campaign(&s, tenant_id, r.params)
            .await
            .map(IntoResponse::into_response),
        "sync_funnelswift_tag" => handle_sync_funnelswift_tag(&s, tenant_id, r.params)
            .await
            .map(IntoResponse::into_response),
        _ => Err(AppError::NotFound(format!("Unknown intent: {}", r.intent))),
    };
    result
}

/// GET /api/admin/chat-action/intents — List all available chat actions
pub async fn list_intents() -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({
        "intents": [
            {
                "name": "create_tenant_account",
                "description": "Create a basic plan tenant account in CRM Swift",
                "required_fields": ["name", "email"],
                "optional_fields": ["password", "tenant_name", "plan_slug"],
                "auto_triggers": [
                    "Creates tenant",
                    "Creates user with owner role",
                    "Subscribes to basic/free plan",
                    "Auto-generates webhook token",
                    "Optionally triggers Ada welcome campaign"
                ],
                "example": {
                    "intent": "create_tenant_account",
                    "params": { "name": "Acme Agency", "email": "admin@acme.com" }
                }
            },
            {
                "name": "build_campaign",
                "description": "Build an email campaign with sequenced emails and FunnelSwift tag sync",
                "required_fields": ["name", "steps"],
                "optional_fields": ["funnelswift_tag", "funnelswift_sync", "description"],
                "auto_triggers": [
                    "Creates email campaign",
                    "Creates email steps (email1, email2, etc.)",
                    "Creates tag in CRM Swift",
                    "Creates campaign->tag trigger",
                    "Syncs tag to FunnelSwift",
                    "Activates campaign automatically"
                ],
                "example": {
                    "intent": "build_campaign",
                    "params": {
                        "name": "Florida Business Expo",
                        "funnelswift_tag": "Florida Business Expo",
                        "funnelswift_sync": true,
                        "steps": [
                            { "template_name": "email1", "subject": "Great meeting you!", "body": "Thanks for stopping by our booth...", "delay_days": 0 },
                            { "template_name": "email2", "subject": "Here's our offer", "body": "As promised, here's the details...", "delay_days": 3 },
                            { "template_name": "email3", "subject": "Last chance", "body": "Don't miss out on this...", "delay_days": 7 }
                        ]
                    }
                }
            },
            {
                "name": "sync_funnelswift_tag",
                "description": "Sync a tag from CRM Swift to FunnelSwift",
                "required_fields": ["tag_name", "action"],
                "optional_fields": ["source"],
                "example": {
                    "intent": "sync_funnelswift_tag",
                    "params": { "tag_name": "Florida Business Expo", "action": "create" }
                }
            }
        ]
    })))
}

// ── Handler: create_tenant_account ──

async fn handle_create_tenant_account(
    s: &AppState,
    params: serde_json::Value,
) -> ApiResult<impl IntoResponse> {
    let name = params.get("name").and_then(|v| v.as_str());
    let email = params.get("email").and_then(|v| v.as_str());

    let mut missing = Vec::new();
    if name.is_none() {
        missing.push(FieldRequest {
            field: "name".into(),
            label: "Account/agency name".into(),
            field_type: "text".into(),
            required: true,
            options: None,
        });
    }
    if email.is_none() {
        missing.push(FieldRequest {
            field: "email".into(),
            label: "Admin email address".into(),
            field_type: "email".into(),
            required: true,
            options: None,
        });
    }

    if !missing.is_empty() {
        return Ok(Json(json!(ChatActionResult {
            intent: "create_tenant_account".into(),
            success: false,
            message: "Please provide the missing details:".into(),
            results: json!({}),
            next_steps: vec![],
            missing_fields: missing,
        })));
    }

    let name = name.ok_or_else(|| AppError::BadRequest("missing field: name".into()))?;
    // ── Address boundary (kanban t_9252c512) ────────────────────────────────────────────────
    // Normalise + validate BEFORE this email reaches a SELECT or the users INSERT below. This
    // admin chat action mints a tenant owner: whatever is stored becomes that owner's login
    // identity and the only address their credentials mail can ever reach.
    let email = email.ok_or_else(|| AppError::BadRequest("missing field: email".into()))?;
    let email = email_addr::normalize(email).map_err(AppError::Validation)?;

    // Create tenant
    let slug = format!(
        "{}-{}",
        name.to_lowercase().replace(' ', "-"),
        &Uuid::new_v4().to_string()[..8]
    );
    let tenant_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(name)
        .bind(&slug)
        .execute(&s.db)
        .await?;

    // Create admin user
    let user_id = Uuid::new_v4();
    use argon2::password_hash::{PasswordHasher, SaltString};
    let salt = SaltString::generate(&mut rand::thread_rng());
    let temp_password = format!("temp-{}", &Uuid::new_v4().to_string()[..8]);
    let password_hash = argon2::Argon2::default()
        .hash_password(temp_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    sqlx::query(
        "INSERT INTO users (id, tenant_id, email, password_hash, name, role) VALUES ($1, $2, $3, $4, $5, 'owner')"
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(name)
    .execute(&s.db)
    .await?;

    // Subscribe to free plan
    let free_plan =
        sqlx::query_as::<_, (Uuid,)>("SELECT id FROM plans WHERE slug = 'free' LIMIT 1")
            .fetch_optional(&s.db)
            .await?;

    if let Some((plan_id,)) = free_plan {
        let _ = sqlx::query(
            "INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle) VALUES ($1, $2, $3, 'active', 'monthly') ON CONFLICT (tenant_id) DO NOTHING"
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(plan_id)
        .execute(&s.db).await;
    }

    // Webhook token auto-generated by trigger in migration 027

    Ok(Json(json!(ChatActionResult {
        intent: "create_tenant_account".into(),
        success: true,
        message: format!(
            "Account '{}' created. Login: {}. A password reset link has been sent.",
            name, email
        ),
        results: json!({
            "tenant_id": tenant_id,
            "user_id": user_id,
            "login_email": email,
            "user_created": true,
            "plan": "free",
            "slug": slug,
        }),
        next_steps: vec![
            StepPrompt {
                step: "Login".into(),
                description: "Admin logs in with provided credentials".into()
            },
            StepPrompt {
                step: "Connect apps".into(),
                description: "Admin connects FunnelSwift, WorkflowSwift, MissedCall Responder"
                    .into()
            },
            StepPrompt {
                step: "Set up".into(),
                description: "Create pipelines, invite team, configure automations".into()
            },
        ],
        missing_fields: vec![],
    })))
}

// ── Handler: build_campaign ──

async fn handle_build_campaign(
    s: &AppState,
    tenant_id: Uuid,
    params: serde_json::Value,
) -> ApiResult<impl IntoResponse> {
    let name = params.get("name").and_then(|v| v.as_str());
    let description = params.get("description").and_then(|v| v.as_str());
    let funnelswift_tag = params.get("funnelswift_tag").and_then(|v| v.as_str());
    let funnelswift_sync = params
        .get("funnelswift_sync")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let steps_val = params.get("steps");

    let mut missing = Vec::new();
    if name.is_none() {
        missing.push(FieldRequest {
            field: "name".into(),
            label: "Campaign name".into(),
            field_type: "text".into(),
            required: true,
            options: None,
        });
    }
    if steps_val
        .as_ref()
        .is_none_or(|v| !v.is_array() || v.as_array().is_none_or(|a| a.is_empty()))
    {
        missing.push(FieldRequest {
            field: "steps".into(),
            label: "Email steps (array of {template_name, subject, body, delay_days})".into(),
            field_type: "text".into(),
            required: true,
            options: None,
        });
    }

    if !missing.is_empty() {
        return Ok(Json(json!(ChatActionResult {
            intent: "build_campaign".into(),
            success: false,
            message: "Missing required fields:".into(),
            results: json!({}),
            next_steps: vec![],
            missing_fields: missing,
        })));
    }

    let campaign_name = name.ok_or_else(|| AppError::BadRequest("missing field: name".into()))?;
    let steps_val = steps_val.ok_or_else(|| AppError::BadRequest("missing field: steps".into()))?;
    let steps_arr = steps_val
        .as_array()
        .ok_or_else(|| AppError::BadRequest("steps must be an array".into()))?;

    for (i, step) in steps_arr.iter().enumerate() {
        let tn = step.get("template_name").and_then(|v| v.as_str());
        let body = step.get("body").and_then(|v| v.as_str());
        if tn.is_none() || tn.unwrap_or_default().is_empty() {
            return Ok(Json(json!(ChatActionResult {
                intent: "build_campaign".into(),
                success: false,
                message: format!("Step {} is missing 'template_name'", i + 1),
                results: json!({}),
                next_steps: vec![],
                missing_fields: vec![],
            })));
        }
        if body.is_none() || body.unwrap_or_default().is_empty() {
            return Ok(Json(json!(ChatActionResult {
                intent: "build_campaign".into(),
                success: false,
                message: format!("Step '{}' is missing 'body'", tn.unwrap_or("unknown")),
                results: json!({}),
                next_steps: vec![],
                missing_fields: vec![],
            })));
        }
    }

    // `tags.name` is varchar(100) and this value is caller-supplied: an over-long tag used to reach
    // the tag INSERT AFTER the campaign and its steps were committed, answering 500 with an orphan
    // draft behind it. Refuse before the first write (kanban t_4be4ae4c).
    let funnelswift_tag = match funnelswift_tag {
        Some(raw) => Some(crate::security::tag_name::normalize_or_validation(raw)?),
        None => None,
    };
    let funnelswift_tag = funnelswift_tag.as_deref();

    // 1. Create campaign
    let campaign = sqlx::query_scalar::<_, serde_json::Value>(&row_json_dml!(
        r#"INSERT INTO email_campaigns (id, tenant_id, name, description, status, created_by)
           VALUES ($1, $2, $3, $4, 'draft', NULL) RETURNING *"#,
    ))
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(campaign_name)
    .bind(description)
    .fetch_one(&s.db)
    .await?;

    let campaign_id_str = campaign
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let campaign_id = Uuid::parse_str(&campaign_id_str)
        .map_err(|_| AppError::BadRequest("invalid campaign id".into()))?;

    // 2. Create steps
    let mut created_steps = Vec::new();
    for (i, step) in steps_arr.iter().enumerate() {
        let tn = step
            .get("template_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subj = step.get("subject").and_then(|v| v.as_str());
        let body = step.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let delay = step.get("delay_days").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

        let s = sqlx::query_scalar::<_, serde_json::Value>(
            &row_json_dml!(r#"INSERT INTO email_campaign_steps (id, campaign_id, step_order, template_name, subject, body, delay_days)
               VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#)
        )
        .bind(Uuid::new_v4()).bind(campaign_id).bind(i as i32 + 1)
        .bind(tn).bind(subj).bind(body).bind(delay)
        .fetch_one(&s.db).await?;
        created_steps.push(s);
    }

    // 3. Handle FunnelSwift tag
    let mut tag_result: Option<serde_json::Value> = None;
    let mut funnelswift_result: Option<String> = None;

    if let Some(tag_name) = funnelswift_tag {
        let tag = sqlx::query_scalar::<_, serde_json::Value>(&row_json_dml!(
            r#"INSERT INTO tags (id, tenant_id, name, color, is_active)
               VALUES ($1, $2, $3, '#3B82F6', true)
               ON CONFLICT (tenant_id, name) DO UPDATE SET is_active = true
               RETURNING id, name"#,
        ))
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(tag_name)
        .fetch_one(&s.db)
        .await?;
        tag_result = Some(tag);

        if let Some(tid_val) = tag_result
            .as_ref()
            .and_then(|t| t.get("id"))
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            let _ = sqlx::query(
                r#"INSERT INTO email_campaign_triggers (id, campaign_id, tag_id, trigger_type)
                   VALUES ($1, $2, $3, 'tag_assigned')
                   ON CONFLICT (campaign_id, tag_id) DO NOTHING"#,
            )
            .bind(Uuid::new_v4())
            .bind(campaign_id)
            .bind(tid_val)
            .execute(&s.db)
            .await;
        }

        if funnelswift_sync {
            match sync_tag_to_funnelswift_internal(&s.db, tenant_id, tag_name, "create").await {
                Ok(msg) => funnelswift_result = Some(msg),
                Err(e) => funnelswift_result = Some(format!("Failed: {}", e)),
            }
        }
    }

    // 4. Auto-activate
    let _ = sqlx::query(
        r#"UPDATE email_campaigns SET status = 'active', updated_at = NOW()
           WHERE id = $1 AND status = 'draft'"#,
    )
    .bind(campaign_id)
    .execute(&s.db)
    .await;

    let step_summary: Vec<String> = created_steps
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let tn = s
                .get("template_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let subj = s
                .get("subject")
                .and_then(|v| v.as_str())
                .unwrap_or("(no subject)");
            let delay = s.get("delay_days").and_then(|v| v.as_i64()).unwrap_or(0);
            format!("  {}. {} — '{}' — {} day(s) delay", i + 1, tn, subj, delay)
        })
        .collect();

    let tag_msg = match (funnelswift_tag, &funnelswift_result) {
        (Some(tn), Some(res)) => format!("\n\nTag '{}': {}", tn, res),
        (Some(tn), None) => format!(
            "\n\nTag '{}': created locally (FunnelSwift sync not configured)",
            tn
        ),
        (None, _) => String::new(),
    };

    Ok(Json(json!(ChatActionResult {
        intent: "build_campaign".into(),
        success: true,
        message: format!(
            "Campaign '{}' built and activated!\n\nSteps:\n{}{}",
            campaign_name,
            step_summary.join("\n"),
            tag_msg
        ),
        results: json!({
            "campaign": campaign,
            "steps": created_steps,
            "tag": tag_result,
            "funnelswift_sync": funnelswift_result,
        }),
        next_steps: vec![
            StepPrompt {
                step: "Add contacts".into(),
                description: format!(
                    "Campaign auto-starts when contacts get tag '{}'",
                    funnelswift_tag.unwrap_or("the linked tag")
                )
            },
            StepPrompt {
                step: "FunnelSwift captures".into(),
                description: "Leads captured in FunnelSwift with matching tag auto-enroll".into()
            },
        ],
        missing_fields: vec![],
    })))
}

// ── Handler: sync_funnelswift_tag ──

async fn handle_sync_funnelswift_tag(
    s: &AppState,
    tenant_id: Uuid,
    params: serde_json::Value,
) -> ApiResult<impl IntoResponse> {
    let tag_name = params.get("tag_name").and_then(|v| v.as_str());
    let action = params.get("action").and_then(|v| v.as_str());

    let mut missing = Vec::new();
    if tag_name.is_none() {
        missing.push(FieldRequest {
            field: "tag_name".into(),
            label: "Tag name to sync".into(),
            field_type: "text".into(),
            required: true,
            options: None,
        });
    }
    if action.is_none() || !["create", "delete"].contains(&action.unwrap_or("")) {
        missing.push(FieldRequest {
            field: "action".into(),
            label: "Sync action (create/delete)".into(),
            field_type: "select".into(),
            required: true,
            options: Some(vec!["create".into(), "delete".into()]),
        });
    }

    if !missing.is_empty() {
        return Ok(Json(json!(ChatActionResult {
            intent: "sync_funnelswift_tag".into(),
            success: false,
            message: "Missing required fields:".into(),
            results: json!({}),
            next_steps: vec![],
            missing_fields: missing,
        })));
    }

    let tag = tag_name.ok_or_else(|| AppError::BadRequest("missing field: tag_name".into()))?;
    let act = action.ok_or_else(|| AppError::BadRequest("missing field: action".into()))?;

    match sync_tag_to_funnelswift_internal(&s.db, tenant_id, tag, act).await {
        Ok(msg) => Ok(Json(json!(ChatActionResult {
            intent: "sync_funnelswift_tag".into(),
            success: true,
            message: format!("Tag '{}' {} synced to FunnelSwift: {}", tag, act, msg),
            results: json!({"tag_name": tag, "action": act, "status": "synced"}),
            next_steps: vec![],
            missing_fields: vec![],
        }))),
        Err(e) => Ok(Json(json!(ChatActionResult {
            intent: "sync_funnelswift_tag".into(),
            success: false,
            message: format!("Failed to sync tag '{}': {}", tag, e),
            results: json!({"tag_name": tag, "action": act, "status": "failed"}),
            next_steps: vec![StepPrompt {
                step: "Connect FunnelSwift".into(),
                description:
                    "Go to Settings > Integrations > FunnelSwift to add your API key and URL".into(),
            }],
            missing_fields: vec![],
        }))),
    }
}

// ── Internal FunnelSwift tag sync helper ──

async fn sync_tag_to_funnelswift_internal(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    tag_name: &str,
    action: &str,
) -> Result<String, String> {
    use crate::native_apps::connectors::funnelswift;

    let creds: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT integration_config->'funnelswift' FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("DB error: {}", e))?
            .flatten();

    let credentials = match creds {
        Some(c) if c.is_object() && !c.as_object().map(|o| o.is_empty()).unwrap_or(true) => c,
        _ => {
            let _ = sqlx::query(
                r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, error_message)
                   VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, 'failed', 'No FunnelSwift integration configured')"#
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action)
            .execute(db).await;
            return Err("FunnelSwift not connected — go to Settings > Integrations to connect your FunnelSwift API key".into());
        }
    };

    match action {
        "create" => {
            let payload = serde_json::json!({ "name": tag_name, "color": "#3B82F6" });
            match funnelswift::push_entity(&credentials, "tag", &payload).await {
                Ok(_) => {
                    let _ = sqlx::query(
                        r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, synced_at)
                           VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, 'synced', NOW())"#
                    )
                    .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action)
                    .execute(db).await;
                    Ok("Synced successfully".into())
                }
                Err(e) => {
                    let _ = sqlx::query(
                        r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, error_message)
                           VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, 'failed', $5)"#
                    )
                    .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action).bind(&e)
                    .execute(db).await;
                    Err(format!("FunnelSwift API error: {}", e))
                }
            }
        }
        "delete" => {
            let _ = sqlx::query(
                r#"INSERT INTO tag_sync_log (id, tenant_id, source, target, tag_name, action, status, synced_at)
                   VALUES ($1, $2, 'crm-swift', 'funnelswift', $3, $4, 'synced', NOW())"#
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name).bind(action)
            .execute(db).await;
            Ok("Delete logged (FunnelSwift delete via webhook)".into())
        }
        _ => Err(format!("Unknown sync action: {}", action)),
    }
}

// ═══════════════════════════════════════════════════════════════════
// Legacy API admin handlers — impersonation, health check, admin CRUD
// ═══════════════════════════════════════════════════════════════════

/// GET /api/admin/health — simple health check (no auth)
pub async fn health_check(State(s): State<AppState>) -> ApiResult<impl IntoResponse> {
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&s.db)
        .await
        .is_ok();

    Ok(Json(json!({
        "status": if db_ok { "healthy" } else { "degraded" },
        "database": if db_ok { "connected" } else { "error" },
        "service": "crm-swift"
    })))
}

/// Platform authority is resolved from the database by token subject (`crate::auth::platform_admin`),
/// never from `Claims.role`: that claim is ACCOUNT-scoped ("Role within their account") and every
/// registration path writes `owner` for the first user of a tenant — 38 of 47 production users on
/// 2026-09-22. The handlers below assert it individually ON TOP of the router-level gate in
/// `super::router`, so a surface that is ever mounted outside that router cannot ship open.

/// POST /api/admin/impersonate — create a JWT for a different tenant (agency_admin only)
pub async fn impersonate(
    Extension(c): Extension<Claims>,
    State(s): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    let target_tenant_id = req
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("tenant_id is required".into()))?;

    // `aid` is copied straight into the token below, so an id that names no workspace would mint a
    // credential that is refused on every route (t_524bfbb9). Refuse at the mint site, with the
    // real reason, instead of handing back a dead token the caller cannot diagnose.
    if !crate::auth::middleware::workspace_exists(&s.db, target_tenant_id).await? {
        return Err(AppError::NotFound("Workspace not found".to_string()));
    }

    use crate::auth::middleware;
    let now = chrono::Utc::now().timestamp() as usize;
    let imp_claims = Claims {
        sub: c.sub.clone(),
        aid: target_tenant_id.to_string(),
        role: "impersonated".to_string(),
        exp: now + 900, // 15 minutes
        iat: now,
        aud: None,
        iss: None,
        wsid: None,
    };

    let token = middleware::create_access_token(&imp_claims, &s.config.jwt_secret)?;

    Ok(Json(json!({
        "impersonation_token": token,
        "expires_in": 900,
        "token_type": "Bearer",
        "message": "Full tenant switch"
    })))
}

/// POST /api/admin/stop-impersonation — instruction to restore admin token
pub async fn stop_impersonation(Extension(_c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({
        "status": "impersonation_stopped",
        "note": "Drop impersonation token and restore original admin token"
    })))
}

/// GET /api/admin/portfolio-companies — list ALL portfolio companies (agency_admin)
pub async fn list_all_portfolio_companies(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    #[derive(sqlx::FromRow, serde::Serialize)]
    struct PortfolioRow {
        id: Uuid,
        tenant_id: Option<Uuid>,
        name: String,
        slug: Option<String>,
        email: Option<String>,
        description: Option<String>,
        is_active: bool,
        created_at: chrono::DateTime<chrono::Utc>,
    }

    let companies = sqlx::query_as::<_, PortfolioRow>(
        r#"SELECT id, tenant_id, name, slug, email, description, is_active, created_at
           FROM portfolio_companies ORDER BY name ASC"#,
    )
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "portfolio_companies": companies })))
}

/// A decode miss on the tenants panel used to be invisible: the cell just rendered empty and
/// nothing was logged (SILENT). Keep the fallback - the panel must still render - but say why.
fn warn_decode(col: &'static str, e: &sqlx::Error) {
    tracing::warn!(column = col, error = %e, "admin tenants: column did not decode");
}

/// GET /api/admin/tenants — list ALL tenants (agency_admin)
pub async fn list_all_tenants(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    use sqlx::Row;
    // id / created_at are cast to ::text on purpose (they are decoded as &str below), which is
    // why neither is a uuid/timestamptz decode: with the cast in place sqlx receives TEXT.
    //
    // The owner read (kanban t_25836872): it used to be a hard `u.role = 'owner'` filter, and
    // `role` cannot be the whole vocabulary — the live row set is `owner`, `agency_admin`,
    // `business_user`, `user` and `user_role` itself carries seven labels, so that filter named
    // only 5 of the 16 live workspaces and 11 arrived at the operator's console with no owner at
    // all. The roles that MEAN "this workspace's owner, or a platform operator" are the four
    // `crate::auth::middleware::is_account_admin` lists, plus the two `_admin` labels this app's
    // `user_role` enum carries; anything else falls back to the EARLIEST member, so a workspace is
    // named whenever it has any user row at all. `u.created_at, u.id` make the pick deterministic
    // (a workspace can hold several equally-ranked members).
    //
    // NULL keeps its meaning: a workspace with no user row (a portfolio / copy row) is genuinely
    // unidentified, and the console says so rather than showing a bare dash.
    //
    // `role::text` is cast explicitly: the column is TEXT today (default `'user'::user_role`), and
    // the cast keeps the literal list from ever being resolved against the enum, which would fail
    // the WHOLE read rather than one row.
    let rows = sqlx::query(
        r#"SELECT t.id::text, t.name, t.slug, (SELECT u.email FROM users u WHERE u.tenant_id = t.id ORDER BY CASE WHEN u.role::text IN ('owner', 'account_owner', 'admin', 'agency_admin', 'company_admin', 'client_admin') THEN 0 ELSE 1 END, u.created_at ASC, u.id ASC LIMIT 1) AS owner_email, t.is_active, t.created_at::text as created_at FROM tenants t ORDER BY t.created_at DESC"#,
    )
    .fetch_all(&s.db)
    .await?;

    let tenants: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            // `tenants.is_active` is NULLABLE (boolean, default true, 0 NULLs live) while this read
            // decoded it as a plain `bool` and fell back to `true`: a NULL tenant was therefore
            // reported to the admin console as ACTIVE - an invented answer rather than an error,
            // and the silent half of the class fixed for `contacts.is_active` on t_97b46a98.
            // Decode honestly (Option<bool>, the t_31ae74fa convention): a NULL is unknown and
            // travels as JSON null, and it is logged once so the state is observable.
            let is_active_raw = r.try_get::<Option<bool>, _>("is_active");
            if let Err(e) = &is_active_raw {
                warn_decode("is_active", e);
            }
            if matches!(&is_active_raw, Ok(None)) {
                tracing::warn!(
                    tenant_id = %r.try_get::<&str, _>("id").unwrap_or(""),
                    "admin tenants: tenants.is_active is NULL - reporting unknown, not active (t_ab7492c4)"
                );
            }
            let is_active: Option<bool> = is_active_raw.ok().flatten();
            // ONE subquery answers both spellings: `email` is the field this endpoint has always
            // carried (the console's Owner column and the tenants screen read it), and `owner_email`
            // is the fleet-wide spelling FunnelSwift / missedcallrespondr / WorkflowSwift / ADASwift
            // now use, so a single operator tool can read all the app lists. Same value, same
            // Option<String>, so the two can never disagree (t_25836872).
            let owner_email: Option<String> = r
                .try_get::<Option<String>, _>("owner_email")
                .map_err(|e| warn_decode("owner_email", &e))
                .ok()
                .flatten();
            json!({
                "id": r.try_get::<&str,_>("id")
                    .map_err(|e| warn_decode("id", &e)).unwrap_or(""),
                "name": r.try_get::<Option<String>,_>("name")
                    .map_err(|e| warn_decode("name", &e)).ok().flatten(),
                "slug": r.try_get::<Option<String>,_>("slug")
                    .map_err(|e| warn_decode("slug", &e)).ok().flatten(),
                "email": owner_email.clone(),
                "owner_email": owner_email,
                "is_active": is_active,
                "created_at": r.try_get::<&str,_>("created_at")
                    .map_err(|e| warn_decode("created_at", &e)).unwrap_or(""),
            })
        })
        .collect();

    Ok(Json(json!({ "tenants": tenants })))
}

/// GET /api/admin/users — list users across every tenant (platform admin only).
///
/// The admin shell's "Users" tab has always called `GET /api/users`, which no router ever
/// registered (404) — so the tab could only ever render its own error text. Platform staff list
/// tenants here already, so the honest home for the same view of people is under /api/admin,
/// behind the platform-admin gate that router applies to every route it owns.
///
/// `role` and the two uuid/timestamp columns are cast to ::text so sqlx receives TEXT and the
/// enum/timestamp decoders never enter the picture (the same reasoning as list_all_tenants).
pub async fn list_all_users(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(p): Query<Value>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    let (page, per_page) = validate_pagination(
        p.get("page").and_then(|v| v.as_i64()),
        p.get("per_page").and_then(|v| v.as_i64()),
    );
    let offset = (page - 1) * per_page;

    #[derive(sqlx::FromRow, serde::Serialize)]
    struct AdminUserRow {
        id: String,
        tenant_id: String,
        tenant_name: Option<String>,
        name: String,
        email: String,
        role: String,
        is_active: bool,
        is_platform_admin: bool,
        created_at: String,
    }

    let users = sqlx::query_as::<_, AdminUserRow>(
        r#"SELECT u.id::text AS id, u.tenant_id::text AS tenant_id, t.name AS tenant_name,
                  u.name, u.email, u.role::text AS role, u.is_active, u.is_platform_admin,
                  u.created_at::text AS created_at
           FROM users u
           LEFT JOIN tenants t ON t.id = u.tenant_id
           ORDER BY u.created_at DESC
           LIMIT $1 OFFSET $2"#,
    )
    .bind(per_page)
    .bind(offset)
    .fetch_all(&s.db)
    .await?;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&s.db)
        .await?;

    Ok(Json(json!({
        "users": users,
        "count": total,
        "page": page,
        "per_page": per_page,
    })))
}

/// POST /api/admin/portfolio-sync — the console's "Sync New Company": give a company a REAL account.
///
/// Two defects lived in this door (kanban t_d4d15d4c), both measured live while Xride Drivers had to
/// be converted through the public signup instead:
///
///   1. **The generated password was thrown away.** The handler argon2-hashed a password into
///      `users.password_hash` and returned ids only, so nothing ever sent it: every account this
///      door minted was a login nobody could use. It now sends the ONE credentials mail
///      (`auth::signup::send_credentials_email`, the same call `register` and `team::mint_member`
///      make) after the commit, latches `users.password_undelivered` when that send fails, and
///      REPORTS `credentials_emailed` so the panel can stop promising an inbox that stays empty.
///   2. **An existing portfolio RECORD could never become an account.** The old idempotency arm read
///      "a row with this id exists" as "this company has an account" and only refreshed
///      name/email/description — so the button on a row created through `POST /api/portfolio` minted
///      nothing, ever. A row is account-less when neither `settings.account_tenant_id` names a live
///      tenant NOR its slug names a `tenants.slug`; such a row is CONVERTED (mint + link), and a row
///      that DOES have an account keeps the refresh arm (a repeat push from another app stays
///      idempotent).
///
/// Three further drifts went with them, because a minted account has to be the account the rest of
/// the app already recognises:
///   * the owner is `role='owner'` — the role `auth::signup` writes for the first user of a
///     workspace. `company_admin` is a legal `user_role`, but the tenant shell and the tenant routes
///     test `owner | admin` (team invites, tenant settings, private-email admin), so a
///     `company_admin` owner could sign in and then be refused on its own workspace's controls;
///   * the portfolio row is written under the OPERATOR's tenant (`Claims.aid`) with the account named
///     in `settings.account_tenant_id` — the shape the row set already has (Giraudy / SwiftImpact /
///     ZaarHub / Xride all sit under the operator's portfolio page). The old arm wrote the NEW tenant
///     into `tenant_id`, which put the company in its own portfolio page and then reported the
///     operator's OWN tenant as the company's account id;
///   * the workspace is flagged `is_portfolio`, so the panel's bulk retire and `tenant_purge` refuse
///     it (a portfolio account is never deleted), and `plan_slug` lets the operator seat a chosen
///     tier at mint instead of always getting the platform default (`free`).
///
/// Idempotency WITHOUT an id: the console's button posts no sync id, and a fresh uuid per press is
/// what let two presses mint two accounts. The company's own slug (or its exact name) is the key —
/// the same test the account arm uses — so a second press finds the row it made the first time.
pub async fn cross_app_sync(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let name = req
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("Company")
        .to_string();
    // ── Address boundary (kanban t_9252c512) ────────────────────────────────────────────────
    // Normalise + validate BEFORE any SELECT or INSERT below. This is the cross-app sync that mints
    // a tenant owner; a value that is not an address must never become a login identity, whatever
    // the caller sends.
    let email = email_addr::normalize(req.get("email").and_then(|v| v.as_str()).unwrap_or(""))
        .map_err(AppError::Validation)?;
    let description = req
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // ── The tier to seat at mint (optional) ─────────────────────────────────────────────────────
    // Resolved BEFORE any write: an unknown or not-on-sale slug must refuse naming the plan, never
    // mint a workspace first and then fail. `plan_for_write` is the app's ONE plan validator, so a
    // tier this door can seat is exactly a tier `PUT /api/admin/tenants/:id/plan` can seat.
    let requested_plan: Option<String> = req
        .get("plan_slug")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let chosen_plan = match requested_plan.as_deref() {
        Some(slug) => Some(crate::billing::lines::plan_for_write(&s.db, Some(slug), None).await?),
        None => None,
    };
    let seated_plan_slug = chosen_plan
        .as_ref()
        .map(|p| p.slug.clone())
        .unwrap_or_else(|| crate::billing::DEFAULT_PLAN_SLUG.to_string());

    // The operator's own tenant — where the portfolio LISTING lives (see the doc above).
    let portfolio_tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // ── Which company is this push about? ───────────────────────────────────────────────────────
    // A sister-app push carries its own sync `id`; the console's button carries none, so the company
    // is resolved by slug/name. The account behind the row is `settings.account_tenant_id` when that
    // names a live tenant, else the tenant whose slug the row carries — the same test the row set
    // shows (giraudy-capital / swiftimpact-solutions / zaarhub each have a tenant).
    #[derive(sqlx::FromRow)]
    struct ExistingRow {
        id: Uuid,
        slug: String,
        account_tenant_id: Option<Uuid>,
    }

    let requested_id = req
        .get("id")
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok());
    let derived_slug = company_slug(&name);

    // The row-plus-account projection, written ONCE. It is a macro rather than a `const` so the two
    // statements below can be assembled at COMPILE time (`concat!`), which is the fleet's sanctioned
    // shape for a shared statement head (gate rule 5d: a query must not be BUILT at run time). The
    // emitted SQL text is byte-identical to the previous `format!("{ROW_SQL} WHERE …")` — see
    // /opt/swift/audits/t_8457bfef/static-sql-identity.py, which proves it.
    macro_rules! row_sql {
        () => {
            r#"-- The company row plus the account behind it, if any.
           SELECT pc.id, pc.slug,
                  COALESCE(
                    (SELECT t.id FROM tenants t WHERE t.id::text = pc.settings->>'account_tenant_id'),
                    (SELECT t.id FROM tenants t WHERE t.slug = pc.slug)
                  ) AS account_tenant_id
             FROM portfolio_companies pc"#
        };
    }

    let existing_row: Option<ExistingRow> = match requested_id {
        Some(id) => {
            sqlx::query_as(concat!(row_sql!(), " WHERE pc.id = $1"))
                .bind(id)
                .fetch_optional(&s.db)
                .await?
        }
        // No id: the row this company already holds, by its slug or by its exact name (the latter
        // covers a row created with `POST /api/portfolio`'s own slug derivation).
        None => {
            sqlx::query_as(concat!(
                row_sql!(),
                " WHERE pc.slug = $1 OR lower(btrim(pc.name)) = lower(btrim($2)) \
             ORDER BY (pc.slug = $1) DESC, pc.created_at ASC LIMIT 1"
            ))
            .bind(&derived_slug)
            .bind(&name)
            .fetch_optional(&s.db)
            .await?
        }
    };

    // ── The account already exists: REFRESH, never re-provision ─────────────────────────────────
    // Only the descriptive columns move. NOT `tenant_id` (the listing scope) and NOT `slug` (bound at
    // mint; re-deriving it would manufacture drift). The resolved account is written into
    // `settings.account_tenant_id` so the next push — and the panel — read the truth instead of
    // re-deriving it from a slug.
    if let Some(row) = existing_row.as_ref() {
        if let Some(account_id) = row.account_tenant_id {
            sqlx::query(
                r#"UPDATE portfolio_companies
                      SET name = $2, email = $3, description = $4,
                          settings = jsonb_set(COALESCE(settings, '{}'::jsonb),
                                               '{account_tenant_id}', to_jsonb($5::text), true),
                          updated_at = NOW()
                    WHERE id = $1"#,
            )
            .bind(row.id)
            .bind(&name)
            .bind(&email)
            .bind(&description)
            .bind(account_id.to_string())
            .execute(&s.db)
            .await?;

            return Ok(Json(json!({
                "status": "already_exists",
                "id": row.id.to_string(),
                "name": name,
                "email": email,
                "account_id": account_id.to_string(),
                "tenant_id": account_id.to_string(),
                "credentials_emailed": false,
                "note": "This company already has an account. Its contact details were refreshed and no new login was created, so no credentials email was sent."
            })));
        }
    }

    // From here this is a MINT — a new company, or an existing record that never had an account.
    // A person belongs to one workspace at a time, so an address that already signs in is refused
    // (the users INSERT would 500 on `users_email_key` otherwise).
    let already_a_user: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);
    if already_a_user > 0 {
        return Err(AppError::Duplicate(format!(
            "{email} already signs in to a workspace on CoreSwift. A person can belong to one \
             workspace at a time, so use a different contact address for this company's owner."
        )));
    }

    let sync_id = existing_row
        .as_ref()
        .map(|r| r.id)
        .or(requested_id)
        .unwrap_or_else(Uuid::new_v4);
    // Converting a record: derive the workspace slug from the row's OWN slug so `tenants.slug` keeps
    // naming the company (the invariant the account test relies on).
    let base_slug = existing_row
        .as_ref()
        .map(|r| r.slug.clone())
        .unwrap_or(derived_slug);

    let generated_password = Uuid::new_v4()
        .to_string()
        .replace("-", "")
        .chars()
        .take(12)
        .collect::<String>();
    use argon2::password_hash::rand_core::OsRng;
    use argon2::password_hash::SaltString;
    use argon2::{Argon2, PasswordHasher};
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(generated_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    // ── One unit: workspace + its plan + its owner + the portfolio row ──────────────────────────
    // All four commit together or not at all. Split, a failure past the tenant commit left an
    // account with no row (a retry then minted a second one) or a row pointing at a workspace with
    // no user (a retry took the `already_exists` arm and never created the owner).
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let now = chrono::Utc::now().naive_utc();
    let mut tx = s.db.begin().await?;

    // Slug: derive from the company, then take the first FREE one. `tenants.slug` carries UNIQUE
    // `tenants_slug_key`; `ON CONFLICT (slug) DO NOTHING` + a bounded retry turns a name collision
    // into a real account instead of a 500 straight from the index.
    let mut tenant_slug: Option<String> = None;
    for attempt in 0..6 {
        let candidate = if attempt == 0 {
            base_slug.clone()
        } else {
            format!("{}-{}", base_slug, attempt)
        };
        let inserted = sqlx::query(
            "INSERT INTO tenants (id, name, slug, is_portfolio) VALUES ($1, $2, $3, true) \
             ON CONFLICT (slug) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(&name)
        .bind(&candidate)
        .execute(&mut *tx)
        .await?;
        if inserted.rows_affected() == 1 {
            tenant_slug = Some(candidate);
            break;
        }
    }
    let tenant_slug = tenant_slug.ok_or_else(|| {
        AppError::Internal(format!(
            "portfolio_sync: no free tenants.slug for company '{}'",
            name
        ))
    })?;

    // The mint and its plan row commit TOGETHER (kanban t_6f225dd4): a tenant with no `tenant_plans`
    // row resolves through `module_registry::resolve`'s `no_plan` arm — every module granted and NO
    // numeric ceiling. Omitted `plan_slug` seats the platform default a signup gets; a chosen tier is
    // seated through the same one writer.
    match chosen_plan.as_ref() {
        Some(plan) => crate::billing::seat_plan(&mut tx, tenant_id, plan.id).await?,
        None => crate::billing::seat_default_plan(&mut tx, tenant_id).await?,
    }

    // `role='owner'`: the role `auth::signup` writes for the first user of a workspace, and the one
    // the tenant shell's own gates accept.
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, role, tenant_id, is_active, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, 'owner', $5, true, $6, $7)",
    )
    .bind(user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&name)
    .bind(tenant_id)
    .bind(now)
    .bind(now)
    .execute(&mut *tx)
    .await?;

    // The row: a NEW company lands under the operator's tenant (the portfolio listing); a CONVERTED
    // record keeps its own `tenant_id` and `slug` — the conflict arm deliberately does not touch them
    // — and gains the account link.
    sqlx::query(
        r#"-- One statement, two arms: create the listing, or link the record that already exists.
           INSERT INTO portfolio_companies
               (id, tenant_id, name, slug, email, description, settings, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6,
                   jsonb_build_object(
                       'account_tenant_id', $7::text,
                       'account_email', $5::text,
                       'account_plan', $8::text,
                       'account_login_created_at', to_jsonb(NOW()::text)
                   ),
                   NOW(), NOW())
           ON CONFLICT (id) DO UPDATE SET
               name = EXCLUDED.name,
               email = EXCLUDED.email,
               description = EXCLUDED.description,
               settings = COALESCE(portfolio_companies.settings, '{}'::jsonb) || EXCLUDED.settings,
               updated_at = NOW()"#,
    )
    .bind(sync_id)
    .bind(portfolio_tenant_id)
    .bind(&name)
    .bind(&tenant_slug)
    .bind(&email)
    .bind(&description)
    .bind(tenant_id.to_string())
    .bind(&seated_plan_slug)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    // ── The credentials mail — the ONLY way the owner learns the password ───────────────────────
    // Post-commit and AWAITED, exactly as `auth::handlers::register` and `auth::team::mint_member`
    // do: the account is already real, so a slow or failing provider can never undo it — the worst
    // case is this response saying the mail did not go out, which is true. A failed send latches
    // `users.password_undelivered`, the state `POST /api/auth/initial-password` exists for.
    let credentials_emailed = crate::auth::signup::send_credentials_email(
        &s.db,
        tenant_id,
        &name,
        &email,
        &name,
        &generated_password,
    )
    .await;
    if !credentials_emailed {
        if let Err(e) = sqlx::query("UPDATE users SET password_undelivered = true WHERE id = $1")
            .bind(user_id)
            .execute(&s.db)
            .await
        {
            tracing::warn!(
                error = %e,
                user = %user_id,
                "portfolio_sync: could not mark the undelivered credential"
            );
        }
    }

    Ok(Json(json!({
        "status": "synced",
        "id": sync_id.to_string(),
        "name": name,
        "email": email,
        "tenant_id": tenant_id.to_string(),
        "user_id": user_id.to_string(),
        "user_created": true,
        "plan": seated_plan_slug,
        "credentials_emailed": credentials_emailed,
        "note": if credentials_emailed {
            "Account created and the login details were emailed to the owner."
        } else {
            "The account was created but the login email could not be sent, so the owner still has no password. Use \"Forgot password\" for that address, or send the details again once mail is working."
        }
    })))
}

/// "Acme, Inc." -> "acme-inc": lowercase, every non-alphanumeric ASCII byte becomes `-`, trimmed,
/// then clipped to 48 chars — the derivation `POST /api/portfolio` and this door already used for a
/// company's slug, extracted so both the row lookup and the `tenants.slug` mint agree.
fn company_slug(name: &str) -> String {
    let s: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    let s = s.replace("--", "-");
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "company".to_string()
    } else {
        s.chars().take(48).collect()
    }
}

/// The portfolio accounts are kept by rule (never deleted from any panel): SwiftImpact, ZaarHub
/// and Giraudy Capital each hold an account in EVERY app, plus whatever the platform itself flags.
const PORTFOLIO_TENANT_MARKERS: [&str; 3] = ["swiftimpact", "zaarhub", "giraudy"];

/// POST /api/admin/tenants/bulk-delete — retire several workspaces in one call (kanban t_ac2fe688).
///
/// The counterpart of the same control in the IncentiveSwift and FunnelSwift panels, so an operator
/// no longer has to open a psql session to clear probe/junk workspaces out of the console's own
/// tenant list. Answers PER ID, so one refused or missing id cannot sink the batch — the panel shows
/// the reason next to the row that was kept.
///
/// Two deletes are refused by rule and cannot be forced from the panel:
///   * the workspace the caller is SIGNED IN AS — that is a lockout, not a cleanup;
///   * a portfolio company (`tenants.is_portfolio`, or a name/slug carrying a portfolio marker).
///
/// Everything else behaves as a plain `DELETE FROM tenants`: measured on the live DB (2026-10-10),
/// all 70 foreign keys that reference `tenants` are `ON DELETE CASCADE`, so no hand-rolled child
/// sweep is needed and a delete cannot strand orphan rows.
pub async fn bulk_delete_tenants(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    let raw_ids = req
        .get("ids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let caller_tenant = c.aid.trim().to_string();
    let mut deleted: Vec<String> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();

    for v in raw_ids {
        let raw = v.as_str().unwrap_or_default().trim().to_string();
        let tid = match Uuid::parse_str(&raw) {
            Ok(t) => t,
            Err(_) => {
                failed.push(json!({"id": raw, "error": "not a valid id"}));
                continue;
            }
        };

        if tid.to_string() == caller_tenant {
            failed.push(json!({
                "id": raw,
                "error": "refusing to delete the workspace you are signed in as"
            }));
            continue;
        }

        // Decode honestly: `tenants.is_portfolio` is NULLABLE, so only a real `true` protects a row
        // — a NULL is unknown and is not promotion-worthy (the convention `list_all_tenants` uses).
        let row: Option<(String, String, Option<bool>)> = sqlx::query_as(
            "SELECT COALESCE(name, ''), COALESCE(slug, ''), is_portfolio FROM tenants WHERE id = $1",
        )
        .bind(tid)
        .fetch_optional(&s.db)
        .await?;

        let (name, slug, is_portfolio) = match row {
            Some(r) => r,
            None => {
                failed.push(json!({"id": raw, "error": "workspace not found"}));
                continue;
            }
        };

        if is_portfolio == Some(true) {
            failed.push(json!({
                "id": raw,
                "error": "refusing to delete a portfolio company (is_portfolio)"
            }));
            continue;
        }

        let hay = format!("{} {}", name, slug).to_lowercase();
        if let Some(hit) = PORTFOLIO_TENANT_MARKERS.iter().find(|m| hay.contains(*m)) {
            failed.push(json!({
                "id": raw,
                "error": format!("refusing to delete a portfolio company ({})", hit)
            }));
            continue;
        }

        match sqlx::query("DELETE FROM tenants WHERE id = $1")
            .bind(tid)
            .execute(&s.db)
            .await
        {
            Ok(r) if r.rows_affected() == 0 => {
                failed.push(json!({"id": raw, "error": "workspace not found"}))
            }
            Ok(_) => deleted.push(raw),
            Err(e) => failed.push(json!({"id": raw, "error": e.to_string()})),
        }
    }

    let status = if failed.is_empty() {
        "deleted"
    } else if deleted.is_empty() {
        "failed"
    } else {
        "partial"
    };

    Ok(Json(json!({
        "status": status,
        "deleted": deleted.len(),
        "deleted_ids": deleted,
        "failed": failed,
    })))
}
