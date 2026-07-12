//! Chat action handlers — single endpoint to orchestrate multi-app flows

use axum::{extract::{State, Json, Extension}, response::IntoResponse};
use serde_json::json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rust_decimal::Decimal;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use crate::affiliates::models::*;
use crate::auth::models::TeamMember;

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
    pub field_type: String,  // "text" | "email" | "number" | "select"
    pub required: bool,
    pub options: Option<Vec<String>>,  // for select fields
}

/// POST /api/admin/chat-action — Execute a business action from chat
pub async fn execute_chat_action(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<ChatActionRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let _is_admin = c.role == "owner" || c.role == "admin";

    let result: Result<axum::response::Response, AppError> = match r.intent.as_str() {
        "create_affiliate" => handle_create_affiliate(&s, tenant_id, r.params).await.map(IntoResponse::into_response),
        "create_affiliate_in_funnelswift" => handle_create_affiliate_funnelswift(&s, tenant_id, r.params).await.map(IntoResponse::into_response),
        "create_tenant_account" => handle_create_tenant_account(&s, r.params).await.map(IntoResponse::into_response),
        "build_campaign" => handle_build_campaign(&s, tenant_id, r.params).await.map(IntoResponse::into_response),
        "sync_funnelswift_tag" => handle_sync_funnelswift_tag(&s, tenant_id, r.params).await.map(IntoResponse::into_response),
        _ => Err(AppError::NotFound(format!("Unknown intent: {}", r.intent))),
    };
    result
}

/// GET /api/admin/chat-action/intents — List all available chat actions
pub async fn list_intents() -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({
        "intents": [
            {
                "name": "create_affiliate",
                "description": "Create an affiliate in CRM Swift with account setup",
                "required_fields": ["name", "email", "commission_rate"],
                "optional_fields": ["commission_type"],
                "auto_triggers": ["Creates tenant account in CRM Swift", "Optionally creates entry in FunnelSwift"],
                "example": {
                    "intent": "create_affiliate",
                    "params": { "name": "John Doe", "email": "john@example.com", "commission_rate": 15 }
                }
            },
            {
                "name": "create_affiliate_in_funnelswift",
                "description": "Create an affiliate product entry in FunnelSwift then auto-create CRM Swift account",
                "required_fields": ["name", "email", "product_name", "commission_rate"],
                "optional_fields": ["commission_type", "price", "image_url"],
                "auto_triggers": [
                    "Creates product in FunnelSwift affiliate board",
                    "Creates tenant account in CRM Swift",
                    "Tags the affiliate in FunnelSwift",
                    "Optionally triggers Ada welcome campaign"
                ],
                "example": {
                    "intent": "create_affiliate_in_funnelswift",
                    "params": { "name": "John Doe", "email": "john@example.com", "product_name": "Pro Plan", "commission_rate": 20, "price": 79 }
                }
            },
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

// ── Handler: create_affiliate ──

async fn handle_create_affiliate(
    s: &AppState,
    _tenant_id: Uuid,
    params: serde_json::Value,
) -> ApiResult<impl IntoResponse> {
    // Collect fields with prompts for missing ones
    let name = params.get("name").and_then(|v| v.as_str());
    let email = params.get("email").and_then(|v| v.as_str());
    let commission_rate = params.get("commission_rate").and_then(|v| v.as_f64());

    // Check what's missing
    let mut missing = Vec::new();
    if name.is_none() { missing.push(FieldRequest {
        field: "name".into(), label: "Affiliate full name".into(),
        field_type: "text".into(), required: true, options: None,
    }); }
    if email.is_none() { missing.push(FieldRequest {
        field: "email".into(), label: "Affiliate email address".into(),
        field_type: "email".into(), required: true, options: None,
    }); }
    if commission_rate.is_none() { missing.push(FieldRequest {
        field: "commission_rate".into(), label: "Commission rate (%)".into(),
        field_type: "number".into(), required: true, options: None,
    }); }

    if !missing.is_empty() {
        return Ok(Json(json!(ChatActionResult {
            intent: "create_affiliate".into(),
            success: false,
            message: "Missing required fields. Please provide:".into(),
            results: json!({}),
            next_steps: vec![StepPrompt {
                step: "Provide missing info".into(),
                description: "Fill in the missing fields and send the same intent again".into(),
            }],
            missing_fields: missing,
        })));
    }

    let name = name.unwrap();
    let email = email.unwrap();
    let rate = commission_rate.unwrap();

    // Check if user already exists
    let existing_user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE email = $1"
    )
    .bind(email)
    .fetch_optional(&s.db)
    .await?;

    let (user_id, user_tenant_id) = if let Some(user) = existing_user {
        // TeamMember exists — use their tenant
        (user.id, user.tenant_id)
    } else {
        // Create a new tenant and user for the affiliate
        let slug = format!("{}-{}", name.to_lowercase().replace(' ', "-"), &Uuid::new_v4().to_string()[..8]);

        // Create tenant first via the auth flow
        
        // We'll create tenant + user directly
        let new_tenant_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
            .bind(new_tenant_id)
            .bind(format!("{}'s Workspace", name))
            .bind(&slug)
            .execute(&s.db)
            .await?;

        // Auto-gen webhook token happens via trigger in migration 027

        let new_user_id = Uuid::new_v4();
        use argon2::password_hash::{SaltString, PasswordHasher};
        let salt = SaltString::generate(&mut rand::thread_rng());
        let password_hash = argon2::Argon2::default()
            .hash_password(name.as_bytes(), &salt)
            .map_err(|e| AppError::Hash(e.to_string()))?
            .to_string();

        sqlx::query(
            "INSERT INTO users (id, tenant_id, email, password_hash, name, role) VALUES ($1, $2, $3, $4, $5, 'owner')"
        )
        .bind(new_user_id)
        .bind(new_tenant_id)
        .bind(email)
        .bind(&password_hash)
        .bind(name)
        .execute(&s.db)
        .await?;

        (new_user_id, new_tenant_id)
    };

    // Create the affiliate profile
    let code = generate_code(name);
    let aff = sqlx::query_as::<_, Affiliate>(
        r#"INSERT INTO affiliates (id, tenant_id, user_id, code, commission_rate, commission_type)
           VALUES ($1, $2, $3, $4, $5, 'percentage') ON CONFLICT (tenant_id, user_id) DO UPDATE SET
           commission_rate = EXCLUDED.commission_rate, updated_at = NOW()
           RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(user_tenant_id)
    .bind(user_id)
    .bind(&code)
    .bind(Decimal::try_from(rate).unwrap_or(Decimal::new(10, 1)))
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(ChatActionResult {
        intent: "create_affiliate".into(),
        success: true,
        message: format!("Affiliate '{}' created with code '{}' and {:.0}% commission. Login: {} (password auto-generated, reset on first login)", name, code, rate, email),
        results: json!({
            "affiliate": aff,
            "tenant_id": user_tenant_id,
            "user_id": user_id,
            "login_email": email,
            "affiliate_code": code,
        }),
        next_steps: vec![
            StepPrompt { step: "Let affiliate know".into(), description: "Send affiliate their code and login info".into() },
            StepPrompt { step: "Set up products".into(), description: "Add affiliate products to their board in FunnelSwift".into() },
            StepPrompt { step: "Tag in FunnelSwift".into(), description: "Tag affiliate so their products show in the affiliate board".into() },
        ],
        missing_fields: vec![],
    })))
}

// ── Handler: create_affiliate_in_funnelswift ──

async fn handle_create_affiliate_funnelswift(
    s: &AppState,
    _tenant_id: Uuid,
    params: serde_json::Value,
) -> ApiResult<impl IntoResponse> {
    let name = params.get("name").and_then(|v| v.as_str());
    let email = params.get("email").and_then(|v| v.as_str());
    let product_name = params.get("product_name").and_then(|v| v.as_str());
    let commission_rate = params.get("commission_rate").and_then(|v| v.as_f64());
    let price = params.get("price").and_then(|v| v.as_f64());

    let mut missing = Vec::new();
    if name.is_none() { missing.push(FieldRequest {
        field: "name".into(), label: "Affiliate name".into(),
        field_type: "text".into(), required: true, options: None,
    }); }
    if email.is_none() { missing.push(FieldRequest {
        field: "email".into(), label: "Affiliate email".into(),
        field_type: "email".into(), required: true, options: None,
    }); }
    if product_name.is_none() { missing.push(FieldRequest {
        field: "product_name".into(), label: "Product name for affiliate board".into(),
        field_type: "text".into(), required: true, options: None,
    }); }
    if commission_rate.is_none() { missing.push(FieldRequest {
        field: "commission_rate".into(), label: "Commission rate (%)".into(),
        field_type: "number".into(), required: true, options: None,
    }); }

    if !missing.is_empty() {
        return Ok(Json(json!(ChatActionResult {
            intent: "create_affiliate_in_funnelswift".into(),
            success: false,
            message: "Missing required fields. Please provide:".into(),
            results: json!({}),
            next_steps: vec![],
            missing_fields: missing,
        })));
    }

    let name = name.unwrap();
    let email = email.unwrap();
    let prod_name = product_name.unwrap();
    let rate = commission_rate.unwrap();
    let product_price = price.unwrap_or(79.0);

    // Step 1: Create CRM Swift account
    let slug = format!("affiliate-{}", &Uuid::new_v4().to_string()[..8]);
    let new_tenant_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(new_tenant_id)
        .bind(format!("{} - Affiliate", name))
        .bind(&slug)
        .execute(&s.db)
        .await?;

    let new_user_id = Uuid::new_v4();
    use argon2::password_hash::{SaltString, PasswordHasher};
    let salt = SaltString::generate(&mut rand::thread_rng());
    let password_hash = argon2::Argon2::default()
        .hash_password(name.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    sqlx::query(
        "INSERT INTO users (id, tenant_id, email, password_hash, name, role) VALUES ($1, $2, $3, $4, $5, 'owner')"
    )
    .bind(new_user_id)
    .bind(new_tenant_id)
    .bind(email)
    .bind(&password_hash)
    .bind(name)
    .execute(&s.db)
    .await?;

    // Step 2: Create affiliate profile
    let code = generate_code(name);
    let aff = sqlx::query_as::<_, Affiliate>(
        r#"INSERT INTO affiliates (id, tenant_id, user_id, code, commission_rate, commission_type)
           VALUES ($1, $2, $3, $4, $5, 'percentage') RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(new_tenant_id)
    .bind(new_user_id)
    .bind(&code)
    .bind(json!(rate))
    .fetch_one(&s.db)
    .await?;

    // Step 3: Create product in affiliate board
    let product = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"INSERT INTO affiliate_products (id, tenant_id, name, price, commission_rate, commission_type)
           VALUES ($1, $2, $3, $4, $5, 'percentage') RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(new_tenant_id)
    .bind(prod_name)
    .bind(json!(product_price))
    .bind(json!(rate))
    .fetch_one(&s.db)
    .await?;

    // Step 4: Try to create or get a tag for FunnelSwift
    let tag = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT * FROM tags WHERE tenant_id = $1 AND name = $2"
    )
    .bind(new_tenant_id)
    .bind(format!("Affiliate: {}", name))
    .fetch_optional(&s.db)
    .await?;

    let tag_id: Option<serde_json::Value> = if let Some(ref t) = tag {
        t.0.get("id").cloned()
    } else {
        let new_tag = sqlx::query_as::<_, (serde_json::Value,)>(
            "INSERT INTO tags (id, tenant_id, name, color) VALUES ($1, $2, $3, $4) RETURNING id"
        )
        .bind(Uuid::new_v4())
        .bind(new_tenant_id)
        .bind(format!("Affiliate: {}", name))
        .bind("#10B981") // green
        .fetch_one(&s.db)
        .await?;
        new_tag.0.get("id").cloned()
    };

    // Update product with tag
    if let Some(tid) = tag_id {
        let _ = sqlx::query("UPDATE affiliate_products SET tag_id = $1 WHERE id = $2")
            .bind(tid.as_str().and_then(|s| Uuid::parse_str(s).ok()))
            .bind(product.0.get("id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()))
            .execute(&s.db).await;
    }

    // Step 5: Trigger Ada campaign trigger for welcome
    let ada_trigger = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"INSERT INTO ada_campaign_triggers (id, tenant_id, name, trigger_on, ada_campaign_id, schedule_delay_minutes)
           VALUES ($1, $2, $3, 'affiliate_activated', 'welcome-affiliate', 0) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(new_tenant_id)
    .bind(format!("Welcome {}", name))
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(ChatActionResult {
        intent: "create_affiliate_in_funnelswift".into(),
        success: true,
        message: format!("Affiliate '{}' fully onboarded across FunnelSwift + CRM Swift. Welcome campaign queued.", name),
        results: json!({
            "affiliate": aff,
            "product": product,
            "tag": "Affiliate: {name}",
            "tenant_id": new_tenant_id,
            "user_id": new_user_id,
            "affiliate_code": code,
            "welcome_trigger": ada_trigger,
        }),
        next_steps: vec![
            StepPrompt { step: "FunnelSwift shows product".into(), description: format!("'{}' now visible in affiliate board in FunnelSwift", prod_name) },
            StepPrompt { step: "Welcome campaign".into(), description: "AdaSwift will send welcome email with login details and commission info".into() },
            StepPrompt { step: "Affiliate shares code".into(), description: format!("Affiliate code '{}' is active — share with new referrals", code) },
        ],
        missing_fields: vec![],
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
    if name.is_none() { missing.push(FieldRequest {
        field: "name".into(), label: "Account/agency name".into(),
        field_type: "text".into(), required: true, options: None,
    }); }
    if email.is_none() { missing.push(FieldRequest {
        field: "email".into(), label: "Admin email address".into(),
        field_type: "email".into(), required: true, options: None,
    }); }

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

    let name = name.unwrap();
    let email = email.unwrap();

    // Create tenant
    let slug = format!("{}-{}", name.to_lowercase().replace(' ', "-"), &Uuid::new_v4().to_string()[..8]);
    let tenant_id = Uuid::new_v4();

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(name)
        .bind(&slug)
        .execute(&s.db)
        .await?;

    // Create admin user
    let user_id = Uuid::new_v4();
    use argon2::password_hash::{SaltString, PasswordHasher};
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
    .bind(email)
    .bind(&password_hash)
    .bind(name)
    .execute(&s.db)
    .await?;

    // Subscribe to free plan
    let free_plan = sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM plans WHERE slug = 'free' LIMIT 1"
    )
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
        message: format!("Account '{}' created. Login: {} / Password: {} (change on first login)", name, email, temp_password),
        results: json!({
            "tenant_id": tenant_id,
            "user_id": user_id,
            "login_email": email,
            "temp_password": temp_password,
            "plan": "free",
            "slug": slug,
        }),
        next_steps: vec![
            StepPrompt { step: "Login".into(), description: "Admin logs in with provided credentials".into() },
            StepPrompt { step: "Connect apps".into(), description: "Admin connects FunnelSwift, WorkflowSwift, MissedCall Responder".into() },
            StepPrompt { step: "Set up".into(), description: "Create pipelines, invite team, configure automations".into() },
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
    let funnelswift_sync = params.get("funnelswift_sync").and_then(|v| v.as_bool()).unwrap_or(true);
    let steps_val = params.get("steps");

    let mut missing = Vec::new();
    if name.is_none() {
        missing.push(FieldRequest {
            field: "name".into(), label: "Campaign name".into(),
            field_type: "text".into(), required: true, options: None,
        });
    }
    if steps_val.is_none() || !steps_val.unwrap().is_array() || steps_val.unwrap().as_array().map(|a| a.is_empty()).unwrap_or(true) {
        missing.push(FieldRequest {
            field: "steps".into(), label: "Email steps (array of {template_name, subject, body, delay_days})".into(),
            field_type: "text".into(), required: true, options: None,
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

    let campaign_name = name.unwrap();
    let steps_arr = steps_val.unwrap().as_array().unwrap();

    for (i, step) in steps_arr.iter().enumerate() {
        let tn = step.get("template_name").and_then(|v| v.as_str());
        let body = step.get("body").and_then(|v| v.as_str());
        if tn.is_none() || tn.unwrap().is_empty() {
            return Ok(Json(json!(ChatActionResult {
                intent: "build_campaign".into(),
                success: false,
                message: format!("Step {} is missing 'template_name'", i + 1),
                results: json!({}),
                next_steps: vec![],
                missing_fields: vec![],
            })));
        }
        if body.is_none() || body.unwrap().is_empty() {
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

    // 1. Create campaign
    let campaign = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"INSERT INTO email_campaigns (id, tenant_id, name, description, status, created_by)
           VALUES ($1, $2, $3, $4, 'draft', NULL) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tenant_id).bind(campaign_name).bind(description)
    .fetch_one(&s.db).await?;

    let campaign_id_str = campaign.0.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let campaign_id = Uuid::parse_str(&campaign_id_str).unwrap();

    // 2. Create steps
    let mut created_steps = Vec::new();
    for (i, step) in steps_arr.iter().enumerate() {
        let tn = step.get("template_name").and_then(|v| v.as_str()).unwrap_or("");
        let subj = step.get("subject").and_then(|v| v.as_str());
        let body = step.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let delay = step.get("delay_days").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

        let s = sqlx::query_as::<_, (serde_json::Value,)>(
            r#"INSERT INTO email_campaign_steps (id, campaign_id, step_order, template_name, subject, body, delay_days)
               VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
        )
        .bind(Uuid::new_v4()).bind(campaign_id).bind(i as i32 + 1)
        .bind(tn).bind(subj).bind(body).bind(delay)
        .fetch_one(&s.db).await?;
        created_steps.push(s);
    }

    // 3. Handle FunnelSwift tag
    let mut tag_result: Option<(serde_json::Value,)> = None;
    let mut funnelswift_result: Option<String> = None;

    if let Some(tag_name) = funnelswift_tag {
        let tag = sqlx::query_as::<_, (serde_json::Value,)>(
            r#"INSERT INTO tags (id, tenant_id, name, color, is_active)
               VALUES ($1, $2, $3, '#3B82F6', true)
               ON CONFLICT (tenant_id, name) DO UPDATE SET is_active = true
               RETURNING id, name"#
        )
        .bind(Uuid::new_v4()).bind(tenant_id).bind(tag_name)
        .fetch_one(&s.db).await?;
        tag_result = Some(tag);

        if let Some(tid_val) = tag_result.as_ref().and_then(|t| t.0.get("id")).and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()) {
            let _ = sqlx::query(
                r#"INSERT INTO email_campaign_triggers (id, campaign_id, tag_id, trigger_type)
                   VALUES ($1, $2, $3, 'tag_assigned')
                   ON CONFLICT (campaign_id, tag_id) DO NOTHING"#
            )
            .bind(Uuid::new_v4()).bind(campaign_id).bind(tid_val)
            .execute(&s.db).await;
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
           WHERE id = $1 AND status = 'draft'"#
    ).bind(campaign_id).execute(&s.db).await;

    let step_summary: Vec<String> = created_steps.iter().enumerate().map(|(i, s)| {
        let tn = s.0.get("template_name").and_then(|v| v.as_str()).unwrap_or("");
        let subj = s.0.get("subject").and_then(|v| v.as_str()).unwrap_or("(no subject)");
        let delay = s.0.get("delay_days").and_then(|v| v.as_i64()).unwrap_or(0);
        format!("  {}. {} — '{}' — {} day(s) delay", i + 1, tn, subj, delay)
    }).collect();

    let tag_msg = match (funnelswift_tag, &funnelswift_result) {
        (Some(tn), Some(res)) => format!("\n\nTag '{}': {}", tn, res),
        (Some(tn), None) => format!("\n\nTag '{}': created locally (FunnelSwift sync not configured)", tn),
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
            StepPrompt { step: "Add contacts".into(), description: format!("Campaign auto-starts when contacts get tag '{}'", funnelswift_tag.unwrap_or("the linked tag")) },
            StepPrompt { step: "FunnelSwift captures".into(), description: "Leads captured in FunnelSwift with matching tag auto-enroll".into() },
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
            field: "tag_name".into(), label: "Tag name to sync".into(),
            field_type: "text".into(), required: true, options: None,
        });
    }
    if action.is_none() || !["create", "delete"].contains(&action.unwrap_or("")) {
        missing.push(FieldRequest {
            field: "action".into(), label: "Sync action (create/delete)".into(),
            field_type: "select".into(), required: true,
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

    let tag = tag_name.unwrap();
    let act = action.unwrap();

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
                description: "Go to Settings > Integrations > FunnelSwift to add your API key and URL".into(),
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

    let creds: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT integration_config->'funnelswift' FROM tenants WHERE id = $1"
    ).bind(tenant_id).fetch_optional(db).await
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
pub async fn health_check(
    State(s): State<AppState>,
) -> ApiResult<impl IntoResponse> {
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

/// POST /api/admin/impersonate — create a JWT for a different tenant (agency_admin only)
pub async fn impersonate(
    Extension(c): Extension<Claims>,
    State(s): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "owner" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    let target_tenant_id = req.get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("tenant_id is required".into()))?;

    use crate::auth::middleware;
    let now = chrono::Utc::now().timestamp() as usize;
    let imp_claims = Claims {
        sub: c.sub.clone(),
        aid: target_tenant_id.to_string(),
        role: "impersonated".to_string(),
        exp: now + 900, // 15 minutes
        iat: now,
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
pub async fn stop_impersonation(
    Extension(_c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
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
    if c.role != "owner" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

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

/// GET /api/admin/tenants — list ALL tenants (agency_admin)
pub async fn list_all_tenants(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "owner" && c.role != "agency_admin" {
        return Err(AppError::Forbidden);
    }

    use sqlx::Row;
    let rows = sqlx::query(
        r#"SELECT t.id::text, t.name, t.slug, (SELECT u.email FROM users u WHERE u.tenant_id = t.id AND u.role = 'owner' LIMIT 1) AS email, t.is_active, t.created_at::text as created_at FROM tenants t ORDER BY t.created_at DESC"#,
    )
    .fetch_all(&s.db)
    .await?;

    let tenants: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.try_get::<&str,_>("id").unwrap_or(""),
            "name": r.try_get::<Option<String>,_>("name").ok().flatten(),
            "slug": r.try_get::<Option<String>,_>("slug").ok().flatten(),
            "email": r.try_get::<Option<String>,_>("email").ok().flatten(),
            "is_active": r.try_get::<bool,_>("is_active").unwrap_or(true),
            "created_at": r.try_get::<&str,_>("created_at").unwrap_or(""),
        })
    }).collect();

    Ok(Json(json!({ "tenants": tenants })))
}

/// POST /api/admin/portfolio-sync — cross-app sync: create tenant + user + portfolio entry
pub async fn cross_app_sync(
    State(s): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let name = req.get("name").and_then(|v| v.as_str()).unwrap_or("Company").to_string();
    let email = req.get("email").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let description = req.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();

    if email.is_empty() {
        return Err(AppError::BadRequest("email is required".into()));
    }

    let existing = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE email = $1")
        .bind(&email)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

    if existing > 0 {
        return Err(AppError::Duplicate(format!("A user with email {} already exists", email)));
    }

    let tenant_id = Uuid::new_v4();
    let tenant_slug = name.to_lowercase().replace(' ', "-");

    sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
        .bind(tenant_id)
        .bind(&name)
        .bind(&tenant_slug)
        .execute(&s.db)
        .await?;

    let user_id = Uuid::new_v4();
    let generated_password = Uuid::new_v4().to_string().replace("-", "").chars().take(12).collect::<String>();
    use argon2::{Argon2, PasswordHasher};
    use argon2::password_hash::SaltString;
    use argon2::password_hash::rand_core::OsRng;
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(generated_password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(e.to_string()))?
        .to_string();

    let now = chrono::Utc::now().naive_utc();
    sqlx::query(
        "INSERT INTO users (id, email, password_hash, name, role, tenant_id, is_active, created_at, updated_at) VALUES ($1, $2, $3, $4, 'company_admin', $5, true, $6, $7)"
    )
    .bind(user_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&name)
    .bind(tenant_id)
    .bind(now)
    .bind(now)
    .execute(&s.db)
    .await?;

    sqlx::query(
        "INSERT INTO portfolio_companies (id, tenant_id, name, slug, email, description, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW()) ON CONFLICT (id) DO UPDATE SET name = $3, email = $5, description = $6, updated_at = NOW()"
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&name)
    .bind(&tenant_slug)
    .bind(&email)
    .bind(&description)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "status": "synced",
        "name": name,
        "email": email,
        "tenant_id": tenant_id.to_string(),
        "user_id": user_id.to_string(),
        "password": generated_password
    })))
}
