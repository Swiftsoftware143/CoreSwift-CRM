use axum::{extract::{State, Json, Extension}, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::ai::models::*;
use crate::ai::engine;

/// POST /api/ai/prioritize — Score and rank all contacts by priority
pub async fn prioritize(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<PrioritizeRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Pull contacts joined with health scores, last activity, and risk level
    let contacts = sqlx::query_as::<_, (Uuid, String, String, i32, Option<i32>, Option<String>, Option<chrono::DateTime<chrono::Utc>>)>(
        r#"SELECT c.id, c.name, c.email,
                  COALESCE(cs.total_score, 0) as score,
                  ah.score as health_score,
                  ah.risk_level,
                  ah.last_active_at
           FROM contacts c
           LEFT JOIN contact_scores cs ON cs.contact_id = c.id
           LEFT JOIN account_health ah ON ah.entity_id = c.id AND ah.entity_type = 'contact'
           WHERE c.tenant_id = $1
           ORDER BY ah.score ASC NULLS LAST, cs.total_score DESC NULLS LAST
           LIMIT $2"#
    ).bind(tid).bind(r.limit.unwrap_or(50)).fetch_all(&s.db).await?;

    let mut prioritized: Vec<serde_json::Value> = contacts.into_iter().map(|(id, name, email, score, health, risk, last)| {
        let health_score = health.unwrap_or(100);
        let days_inactive = last.map(|t| (chrono::Utc::now() - t).num_days() as i32).unwrap_or(0);
        let risk_level = risk.unwrap_or_else(|| "healthy".to_string());
        let priority = (score as f64 * 0.4) + ((100 - health_score) as f64 * 0.3) + (days_inactive as f64 * 0.2);

        let action = if risk_level == "critical" { "human_callback" }
                     else if risk_level == "at_risk" { "re_engage_email" }
                     else if days_inactive > 0 { "checklist_stage" }
                     else { "monitor" };

        json!({
            "contact_id": id,
            "name": name,
            "email": email,
            "score": score,
            "health_score": health_score,
            "risk_level": risk_level,
            "days_inactive": days_inactive,
            "priority_score": (priority * 100.0).round() / 100.0,
            "recommended_action": action,
        })
    }).collect();

    // Sort by priority descending
    prioritized.sort_by(|a, b| {
        let pa = a["priority_score"].as_f64().unwrap_or(0.0);
        let pb = b["priority_score"].as_f64().unwrap_or(0.0);
        pb.partial_cmp(&pa).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(Json(json!({"prioritized": prioritized, "count": prioritized.len()})))
}

/// POST /api/ai/predict — Predict win probability for a specific contact
pub async fn predict(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<PredictRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Gather signals
    let positive = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND entity_id = $2
         AND event_type IN ('login', 'feature_used', 'email.opened', 'email.clicked', 'api_call')"
    ).bind(tid).bind(r.contact_id).fetch_one(&s.db).await?.unwrap_or(0);

    let negative = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND entity_id = $2
         AND event_type IN ('failed_action', 'complaint', 'unsubscribe', 'days_inactive')"
    ).bind(tid).bind(r.contact_id).fetch_one(&s.db).await?.unwrap_or(0);

    let health = sqlx::query_as::<_, (Option<i32>, Option<String>)>(
        "SELECT score, risk_level FROM account_health WHERE tenant_id = $1 AND entity_type = 'contact' AND entity_id = $2"
    ).bind(tid).bind(r.contact_id).fetch_optional(&s.db).await?;

    let (score, risk) = health.unwrap_or((Some(50), Some("unknown".to_string())));

    // Simple weighted prediction
    let win_prob = ((positive as f64 * 0.15) + (score.unwrap_or(50) as f64 * 0.01) - (negative as f64 * 0.1)).clamp(0.0, 1.0);

    let rec = if risk.as_deref() == Some("critical") { "escalate" }
              else if win_prob < 0.3 { "nurture" }
              else if win_prob < 0.7 { "discount" }
              else { "paused" };

    Ok(Json(json!({
        "contact_id": r.contact_id,
        "win_probability": win_prob,
        "key_signals": json!({"positive_signals": positive, "negative_signals": negative}),
        "recommendation": rec
    })))
}

/// POST /api/ai/recommend — Get segmentation and campaign recommendations
pub async fn recommend(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CampaignRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rec = engine::recommend_campaign(&s.db, tid, &r.campaign_goal).await;
    Ok(Json(json!(rec)))
}

/// POST /api/ai/campaign — Legacy alias for recommend
pub async fn campaign(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CampaignRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rec = engine::recommend_campaign(&s.db, tid, &r.campaign_goal).await;
    Ok(Json(json!(rec)))
}

/// POST /api/ai/message — AI-compose a follow-up message for a specific context
pub async fn compose_message(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<ComposeMessageRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Get contact info for personalization
    let contact = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT CONCAT(first_name, ' ', last_name) AS name, email, phone FROM contacts WHERE id = $1 AND tenant_id = $2"
    ).bind(r.contact_id).bind(tid).fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Contact not found".to_string()))?;

    let (name, _email, _phone) = contact;

    // Get health context
    let assessment = engine::assess_churn_risk(&s.db, tid, r.contact_id).await;
    let template_slug = engine::select_template(&r.context, assessment.churn_probability);

    // Try AI-powered composition via DeepSeek (with OpenAI/Anthropic fallback)
    let mut api_keys = std::collections::HashMap::new();
    if let Ok(Some(keys)) = sqlx::query_scalar::<_, Option<serde_json::Value>>(
        "SELECT settings->'ai'->'providers' FROM tenants WHERE id = $1"
    ).bind(tid).fetch_one(&s.db).await {
        if let Some(obj) = keys.as_object() {
            for (k, v) in obj {
                if let Some(val) = v.as_str() {
                    api_keys.insert(k.clone(), val.to_string());
                }
            }
        }
    }

    let body = if !api_keys.is_empty() {
        crate::ai::router::ai_compose_follow_up(
            &api_keys,
            &name,
            &name,  // business_name same as contact name for now
            &r.context,
            (assessment.churn_probability * 100.0) as i32,
            assessment.signals_count,
        ).await
    } else {
        compose_body(&r.context, &name, &r.tone.unwrap_or_else(|| "friendly".to_string()), &assessment)
    };

    let subject = compose_subject(&r.context, &name, &assessment);

    let msg_id = Uuid::new_v4();

    // Save the composed message as a template
    let _ = sqlx::query(
        r#"INSERT INTO message_templates (id, tenant_id, name, channel, subject, body, variables)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#
    )
    .bind(msg_id).bind(tid)
    .bind(format!("ai_composed_{}_{}", r.context, r.contact_id))
    .bind(&r.channel)
    .bind(&subject).bind(&body)
    .bind(json!({"ai_generated": true, "context": &r.context, "template": template_slug}))
    .execute(&s.db).await;

    Ok(Json(json!({
        "subject": subject,
        "body": body,
        "message_id": msg_id,
        "template_slug": template_slug,
        "churn_probability": assessment.churn_probability
    })))
}

/// POST /api/ai/channel — AI-suggest the best channel for follow-up
pub async fn suggest_channel(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<ChannelRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let suggestion = engine::suggest_channel(&s.db, tid, r.contact_id, &r.context).await;
    Ok(Json(json!(suggestion)))
}

/// POST /api/ai/timing — AI-suggest optimal send time
pub async fn suggest_timing(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<TimingRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let suggestion = engine::suggest_timing(&s.db, tid, r.contact_id).await;
    Ok(Json(json!(suggestion)))
}

/// POST /api/ai/risk — Assess churn risk for a single contact
pub async fn assess_churn_risk(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<ChurnRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let assessment = engine::assess_churn_risk(&s.db, tid, r.contact_id).await;
    Ok(Json(json!(assessment)))
}

fn compose_body(context: &str, name: &str, tone: &str, assessment: &ChurnAssessment) -> String {
    match (context, tone) {
        ("abandoned_signup", "urgent") => format!(
            "Hi {name},\n\nYour directory profile is almost complete! Don't let your listing go live without the final details — prospects are searching for businesses like yours right now.\n\nComplete your profile in 2 minutes: {{checklist_link}}\n\nBest,\nThe CRM Swift Team"
        ),
        ("abandoned_signup", _) => format!(
            "Hi {name},\n\nThanks for starting your directory profile! Just a few quick details to finish setting up your listing:\n- Add your business hours\n- Upload your logo\n- Choose your keywords\n\nIt takes 2 minutes: {{checklist_link}}\n\nBest,\nThe CRM Swift Team"
        ),
        ("inactive_trial", "urgent") => format!(
            "Hi {name},\n\nYour trial is expiring soon and we noticed you haven't had a chance to explore the platform yet. I'd love to schedule a 10-minute walkthrough to show you how businesses like yours are getting results.\n\nBook a call: {{booking_link}}\n\nBest,\nThe CRM Swift Team"
        ),
        ("inactive_trial", _) => format!(
            "Hi {name},\n\nJust checking in! Your CRM Swift trial is active and we want to make sure you're getting the most out of it. Here are a few things you can try:\n- Import your contacts\n- Set up your first pipeline\n- Create an automation rule\n\nNeed help? Just reply to this email.\n\nBest,\nThe CRM Swift Team"
        ),
        ("checklist_stage_2", _) => format!(
            "Hi {name},\n\nGreat progress! You've completed the first onboarding step. Next up:\n- Add your business hours to your profile\n\nThis helps prospects know when to reach you: {{checklist_link}}\n\nBest,\nThe CRM Swift Team"
        ),
        ("checklist_stage_3", _) => format!(
            "Hi {name},\n\nAlmost there! The last step is adding your keywords so prospects can find you in search results.\n\nAdd your keywords: {{checklist_link}}\n\nBest,\nThe CRM Swift Team"
        ),
        ("churn_risk", _) if assessment.churn_probability >= 0.7 => format!(
            "Hi {name},\n\nWe noticed you haven't been active recently and we want to make sure everything is working for you. As a valued CRM Swift user, I'd like to offer a personal check-in call to help you get the most out of the platform.\n\nSchedule a 15-minute call: {{booking_link}}\n\nYour account health score is {} — let's get that back up.\n\nBest,\nThe CRM Swift Team",
            (assessment.churn_probability * 100.0) as i32
        ),
        ("churn_risk", _) => format!(
            "Hi {name},\n\nJust wanted to check in! We noticed it's been a little while since you last logged in. Is there anything we can help with?\n\nReply to this email and a real person will get back to you.\n\nBest,\nThe CRM Swift Team"
        ),
        ("renewal", _) => format!(
            "Hi {name},\n\nYour CRM Swift subscription is up for renewal soon. To keep enjoying uninterrupted access to all features, simply click below to renew:\n\nRenew now: {{checkout_url}}\n\nThanks for being a valued customer!\nThe CRM Swift Team"
        ),
        _ => format!(
            "Hi {name},\n\nJust a quick update from CRM Swift. We're here to help you grow your business.\n\nBest,\nThe CRM Swift Team"
        ),
    }
}

fn compose_subject(context: &str, name: &str, assessment: &ChurnAssessment) -> String {
    match context {
        "abandoned_signup" => format!("{name}, finish setting up your profile"),
        "inactive_trial" if assessment.churn_probability >= 0.5 => format!("{name}, your trial needs attention"),
        "inactive_trial" => format!("{name}, want a hand getting started?"),
        "checklist_stage_2" => "Next step: Add your business hours".to_string(),
        "checklist_stage_3" => "Final step: Add your keywords".to_string(),
        "churn_risk" if assessment.churn_probability >= 0.7 => format!("{name}, can we help?"),
        "churn_risk" => format!("{name}, checking in"),
        "renewal" => "Your CRM Swift renewal is coming up".to_string(),
        _ => "Update from CRM Swift".to_string(),
    }
}
