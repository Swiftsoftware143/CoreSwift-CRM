use sqlx::PgPool;
use uuid::Uuid;
use crate::errors::AppError;
use super::models::AutomationRule;

pub async fn execute_action(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    match rule.action_type.as_str() {
        "AddTag" => exec_add_tag(db, rule, tenant_id, entity_type, entity_id).await,
        "RemoveTag" => exec_remove_tag(db, rule, entity_type, entity_id).await,
        "MovePipeline" | "pipeline.move" => exec_move_pipeline(db, rule, tenant_id, entity_id).await,
        "AddToList" => exec_add_to_list(db, rule, tenant_id, entity_id).await,
        "RemoveFromList" => exec_remove_from_list(db, rule, entity_id).await,
        "Webhook" => exec_webhook(db, rule, tenant_id, entity_type, entity_id).await,
        "NotifyUser" => exec_notify(db, rule, tenant_id).await,
        "send_email" => exec_send_email(db, rule, tenant_id, entity_type, entity_id).await,
        "send_sms" => exec_send_sms(db, rule, tenant_id, entity_type, entity_id).await,
        "scoring.update" => exec_scoring_update(db, rule, tenant_id, entity_id).await,
        _ => Ok(()),
    }
}

async fn exec_add_tag(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let tid_str = rule.action_config.get("tag_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing tag_id".into()))?;
    let tag_id = Uuid::parse_str(tid_str).map_err(|_| AppError::Validation("Invalid tag_id".into()))?;
    let exists: bool = sqlx::query_scalar("SELECT COUNT(*) FROM tag_assignments WHERE tag_id=$1 AND entity_type=$2::entity_type AND entity_id=$3 AND tenant_id=$4").bind(tag_id).bind(entity_type).bind(entity_id).bind(tenant_id).fetch_one(db).await.unwrap_or(0) > 0;
    if !exists { sqlx::query("INSERT INTO tag_assignments(id,tag_id,entity_type,entity_id,tenant_id) VALUES($1,$2,$3,$4,$5)").bind(Uuid::new_v4()).bind(tag_id).bind(entity_type).bind(entity_id).bind(tenant_id).execute(db).await?; }
    Ok(())
}

async fn exec_remove_tag(db: &PgPool, rule: &AutomationRule, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let tid_str = rule.action_config.get("tag_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing tag_id".into()))?;
    let tag_id = Uuid::parse_str(tid_str).map_err(|_| AppError::Validation("Invalid tag_id".into()))?;
    sqlx::query("DELETE FROM tag_assignments WHERE tag_id=$1 AND entity_type=$2::entity_type AND entity_id=$3").bind(tag_id).bind(entity_type).bind(entity_id).execute(db).await?;
    Ok(())
}

async fn exec_move_pipeline(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_id: Uuid) -> Result<(), AppError> {
    let sid_str = rule.action_config.get("stage_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing stage_id".into()))?;
    let stage_id = Uuid::parse_str(sid_str).map_err(|_| AppError::Validation("Invalid stage_id".into()))?;
    let r = sqlx::query("UPDATE opportunities SET stage_id=$1, updated_at=NOW() WHERE id=$2 AND tenant_id=$3").bind(stage_id).bind(entity_id).bind(tenant_id).execute(db).await?;
    if r.rows_affected() > 0 { sqlx::query("INSERT INTO stage_history(id,opportunity_id,to_stage_id) VALUES($1,$2,$3)").bind(Uuid::new_v4()).bind(entity_id).bind(stage_id).execute(db).await?; }
    Ok(())
}

async fn exec_add_to_list(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_id: Uuid) -> Result<(), AppError> {
    let lid_str = rule.action_config.get("list_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing list_id".into()))?;
    let list_id = Uuid::parse_str(lid_str).map_err(|_| AppError::Validation("Invalid list_id".into()))?;
    sqlx::query("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,false) ON CONFLICT DO NOTHING")
        .bind(Uuid::new_v4()).bind(list_id).bind(entity_id).bind(tenant_id).execute(db).await?;
    Ok(())
}

async fn exec_remove_from_list(db: &PgPool, rule: &AutomationRule, entity_id: Uuid) -> Result<(), AppError> {
    let lid_str = rule.action_config.get("list_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing list_id".into()))?;
    let list_id = Uuid::parse_str(lid_str).map_err(|_| AppError::Validation("Invalid list_id".into()))?;
    sqlx::query("DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2").bind(list_id).bind(entity_id).execute(db).await?;
    Ok(())
}

async fn exec_webhook(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let url = rule.action_config.get("url").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing webhook url".into()))?;

    // Look up contact info for the payload
    let contact_info: Option<(String, Option<String>, Option<String>)> = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT CONCAT(first_name, ' ', last_name), email, phone FROM contacts WHERE id=$1 AND tenant_id=$2"
    )
    .bind(entity_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let (contact_name, contact_email, contact_phone) = contact_info.unwrap_or_else(|| ("Unknown".into(), None, None));

    let payload = serde_json::json!({
        "event": rule.trigger_type,
        "tenant_id": tenant_id,
        "entity_type": entity_type,
        "entity_id": entity_id,
        "contact_name": contact_name,
        "contact_email": contact_email,
        "contact_phone": contact_phone,
        "rule_id": rule.id,
        "rule_name": rule.name,
        "timestamp": chrono::Utc::now(),
    });

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let headers: std::collections::HashMap<String, String> =
        rule.action_config.get("headers")
            .and_then(|h| serde_json::from_value(h.clone()).ok())
            .unwrap_or_default();

    let mut req = client.post(url).json(&payload);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }

    match req.send().await {
        Ok(resp) => {
            tracing::info!(rule = %rule.id, url = %url, status = %resp.status(), "Webhook delivered");
        }
        Err(e) => {
            tracing::warn!(rule = %rule.id, url = %url, error = %e, "Webhook delivery failed");
        }
    }

    // Update execution tracking
    let _ = sqlx::query(
        "UPDATE automation_rules SET execution_count = execution_count + 1, last_executed_at = NOW() WHERE id = $1"
    ).bind(rule.id).execute(db).await;

    Ok(())
}

async fn exec_notify(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid) -> Result<(), AppError> {
    let user_id_str = rule.action_config.get("user_id").and_then(|v| v.as_str());
    let message = rule.action_config.get("message").and_then(|v| v.as_str()).unwrap_or("Automation triggered");

    if let Some(uid_str) = user_id_str {
        if let Ok(user_id) = Uuid::parse_str(uid_str) {
            let _ = sqlx::query(
                "INSERT INTO notifications(id, tenant_id, user_id, message) VALUES($1, $2, $3, $4)"
            )
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(user_id)
            .bind(message)
            .execute(db)
            .await;
        }
    }

    Ok(())
}

/// Execute send_email action — sends an email via the comms provider
async fn exec_send_email(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let template_id = rule.action_config.get("template_id").and_then(|v| v.as_str());

    // Get contact email
    let contact_email: Option<String> = if entity_type == "contact" {
        sqlx::query_scalar("SELECT email FROM contacts WHERE id=$1 AND tenant_id=$2")
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_optional(db)
            .await?
    } else {
        None
    };

    let to = rule.action_config.get("to").and_then(|v| v.as_str()).map(|s| s.to_string())
        .or(contact_email)
        .ok_or_else(|| AppError::Validation("No recipient email available".into()))?;

    let subject = rule.action_config.get("subject").and_then(|v| v.as_str()).unwrap_or("Automated Message");
    let body = rule.action_config.get("body").and_then(|v| v.as_str()).unwrap_or("");

    let body_text = if !body.is_empty() {
        body.to_string()
    } else if let Some(tid) = template_id {
        // Load template body
        sqlx::query_scalar::<_, String>(
            "SELECT body FROM message_templates WHERE id=$1 AND tenant_id=$2"
        )
        .bind(Uuid::parse_str(tid).unwrap_or(Uuid::nil()))
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "No template body".to_string())
    } else {
        "Automated message".to_string()
    };

    // Insert into outbound_messages for delivery
    let msg_id = Uuid::new_v4();
    let _ = sqlx::query(
        "INSERT INTO outbound_messages(id, tenant_id, channel, to_address, subject, body, status)
         VALUES($1, $2, 'email', $3, $4, $5, 'queued')"
    )
    .bind(msg_id)
    .bind(tenant_id)
    .bind(&to)
    .bind(subject)
    .bind(&body_text)
    .execute(db)
    .await;

    // Fire transport via comms providers
    let cfg = crate::communications::providers::load_delivery_config(
        db,
        msg_id,
        tenant_id,
        "email",
        &to,
        Some(subject.to_string()),
        &body_text,
    ).await;
    let _ = crate::communications::providers::deliver(&cfg).await;

    let _ = sqlx::query(
        "UPDATE automation_rules SET execution_count = execution_count + 1, last_executed_at = NOW() WHERE id = $1"
    ).bind(rule.id).execute(db).await;

    Ok(())
}

/// Execute send_sms action
async fn exec_send_sms(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let template_id = rule.action_config.get("template_id").and_then(|v| v.as_str());

    // Get contact phone
    let contact_phone: Option<String> = if entity_type == "contact" {
        sqlx::query_scalar("SELECT phone FROM contacts WHERE id=$1 AND tenant_id=$2")
            .bind(entity_id)
            .bind(tenant_id)
            .fetch_optional(db)
            .await?
    } else {
        None
    };

    let to = rule.action_config.get("to").and_then(|v| v.as_str()).map(|s| s.to_string())
        .or(contact_phone)
        .ok_or_else(|| AppError::Validation("No recipient phone available".into()))?;

    let body = rule.action_config.get("body").and_then(|v| v.as_str()).unwrap_or("");

    let body_text = if !body.is_empty() {
        body.to_string()
    } else if let Some(tid) = template_id {
        sqlx::query_scalar::<_, String>(
            "SELECT body FROM message_templates WHERE id=$1 AND tenant_id=$2"
        )
        .bind(Uuid::parse_str(tid).unwrap_or(Uuid::nil()))
        .bind(tenant_id)
        .fetch_optional(db)
        .await?
        .unwrap_or_else(|| "No template body".to_string())
    } else {
        "Automated message".to_string()
    };

    let msg_id = Uuid::new_v4();
    let _ = sqlx::query(
        "INSERT INTO outbound_messages(id, tenant_id, channel, to_address, body, status)
         VALUES($1, $2, 'sms', $3, $4, 'queued')"
    )
    .bind(msg_id)
    .bind(tenant_id)
    .bind(&to)
    .bind(&body_text)
    .execute(db)
    .await;

    let cfg = crate::communications::providers::load_delivery_config(
        db,
        msg_id,
        tenant_id,
        "sms",
        &to,
        None,
        &body_text,
    ).await;
    let _ = crate::communications::providers::deliver(&cfg).await;

    let _ = sqlx::query(
        "UPDATE automation_rules SET execution_count = execution_count + 1, last_executed_at = NOW() WHERE id = $1"
    ).bind(rule.id).execute(db).await;

    Ok(())
}

/// Execute scoring.update action — adjust contact score
async fn exec_scoring_update(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_id: Uuid) -> Result<(), AppError> {
    let points = rule.action_config.get("points").and_then(|v| v.as_i64()).unwrap_or(0) as i32;

    if points != 0 {
        let score_id = match sqlx::query_as::<_, (Uuid,)>(
            "SELECT id FROM contact_scores WHERE tenant_id=$1 AND contact_id=$2"
        )
        .bind(tenant_id)
        .bind(entity_id)
        .fetch_optional(db)
        .await?
        {
            Some((sid,)) => sid,
            None => {
                let sid = Uuid::new_v4();
                sqlx::query(
                    "INSERT INTO contact_scores(id,tenant_id,contact_id,total_score,category,updated_at) VALUES($1,$2,$3,0,'interested',NOW())"
                )
                .bind(sid)
                .bind(tenant_id)
                .bind(entity_id)
                .execute(db)
                .await?;
                sid
            }
        };

        sqlx::query(
            "UPDATE contact_scores SET total_score = GREATEST(0, total_score + $1), last_event_type = 'automation', last_event_at = NOW(), updated_at = NOW() WHERE id = $2"
        )
        .bind(points)
        .bind(score_id)
        .execute(db)
        .await?;
    }

    let _ = sqlx::query(
        "UPDATE automation_rules SET execution_count = execution_count + 1, last_executed_at = NOW() WHERE id = $1"
    ).bind(rule.id).execute(db).await;

    Ok(())
}
