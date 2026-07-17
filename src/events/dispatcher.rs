//! Event Dispatcher — routes incoming events to automation, delayed evaluation, and webhooks.
//!
//! This is the core of the Flawless Follow-up system:
//! 1. Incoming webhook arrives → stored in events table
//! 2. dispatcher checks automation rules for matching triggers
//! 3. If a delay is required, delayed_actions table tracks it
//! 4. After timeout, evaluate_delayed_action checks condition and fires

use sqlx::PgPool;
use uuid::Uuid;
use serde_json::json;

/// Dispatch an event to all matching automation rules for a tenant.
/// Fire-and-forget — logs failures but never blocks.
/// Also triggers onboarding checklists and records health signals for matching events.
pub async fn dispatch_automation(
    db: &PgPool,
    tenant_id: Uuid,
    event_type: &str,
    entity_type: Option<&str>,
    entity_id: Option<Uuid>,
    payload: &serde_json::Value,
) {
    // Find active automation rules matching this trigger
    let rules = match sqlx::query_as::<_, (Uuid, String, serde_json::Value, String, serde_json::Value)>(
        r#"SELECT id, trigger_type, trigger_config, action_type, action_config
           FROM automation_rules
           WHERE tenant_id = $1 AND is_active = true AND trigger_type = $2
           ORDER BY execution_count ASC"#
    )
    .bind(tenant_id)
    .bind(event_type)
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, tenant = %tenant_id, event = %event_type, "Failed to fetch automation rules");
            return;
        }
    };

    for (rule_id, _trigger_type, trigger_config, action_type, action_config) in rules {
        // Check if trigger_config has filter conditions
        if let Some(conditions) = trigger_config.as_object() {
            if let Some(entity_match) = conditions.get("entity_type") {
                if let Some(et) = entity_type {
                    if entity_match.as_str() != Some(et) {
                        continue; // entity type doesn't match
                    }
                }
            }
        }

        // Execute the action
        execute_action(db, tenant_id, rule_id, &action_type, &action_config, entity_id, payload).await;

        // Increment execution count
        let _ = sqlx::query("UPDATE automation_rules SET execution_count = execution_count + 1, last_executed_at = NOW() WHERE id = $1")
            .bind(rule_id)
            .execute(db)
            .await;
    }

    // Also trigger onboarding checklists for matching events
    match event_type {
        "contact.created" | "signup" | "trial.started" | "payment.received" => {
            if let (Some(et), Some(eid)) = (entity_type, entity_id) {
                crate::checklists::engine::trigger_checklist(db, tenant_id, event_type, et, eid).await;
            }
        }
        _ => {}
    }

    // Record health signal for positive interactions
    match event_type {
        "login" | "feature_used" | "api_call" => {
            if let Some(eid) = entity_id {
                crate::monitoring::engine::record_signal(db, tenant_id, entity_type.unwrap_or("contact"), eid, event_type, 1).await;
            }
        }
        _ => {}
    }
}

/// Execute a single automation action
async fn execute_action(
    db: &PgPool,
    tenant_id: Uuid,
    rule_id: Uuid,
    action_type: &str,
    action_config: &serde_json::Value,
    entity_id: Option<Uuid>,
    _payload: &serde_json::Value,
) {
    tracing::debug!(rule = %rule_id, action = %action_type, "Executing automation action");

    match action_type {
        "tag_contact" | "add_tag" => {
            if let (Some(eid), Some(tag_name)) = (entity_id, action_config.get("tag_name").and_then(|v| v.as_str())) {
                let _ = sqlx::query(
                    r#"INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id)
                       SELECT uuid_generate_v4(), t.id, 'contact', $1
                       FROM tags t WHERE t.tenant_id = $2 AND t.name = $3
                       ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"#
                )
                .bind(eid).bind(tenant_id).bind(tag_name)
                .execute(db).await;
            }
        }
        "send_email" => {
            if let (Some(to), Some(subject), Some(body)) = (
                action_config.get("to").and_then(|v| v.as_str()),
                action_config.get("subject").and_then(|v| v.as_str()),
                action_config.get("body").and_then(|v| v.as_str()),
            ) {
                // Queue email via communications module
                let _ = sqlx::query(
                    r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status)
                       VALUES ($1, $2, 'email', $3, $4, $5, 'queued')"#
                )
                .bind(Uuid::new_v4()).bind(tenant_id).bind(to).bind(subject).bind(body)
                .execute(db).await;
            }
        }
        "send_sms" => {
            if let (Some(to), Some(body)) = (
                action_config.get("to").and_then(|v| v.as_str()),
                action_config.get("body").and_then(|v| v.as_str()),
            ) {
                let _ = sqlx::query(
                    r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, body, status)
                       VALUES ($1, $2, 'sms', $3, $4, 'queued')"#
                )
                .bind(Uuid::new_v4()).bind(tenant_id).bind(to).bind(body)
                .execute(db).await;
            }
        }
        "webhook" => {
            if let Some(url) = action_config.get("url").and_then(|v| v.as_str()) {
                // Fire-and-forget webhook call
                let payload = json!({
                    "event": "automation_triggered",
                    "rule_id": rule_id,
                    "tenant_id": tenant_id,
                    "action": action_type,
                    "config": action_config,
                });
                let url_owned = url.to_string();
                tokio::spawn(async move {
                    let client = reqwest::Client::new();
                    let _ = client.post(&url_owned)
                        .json(&payload)
                        .timeout(std::time::Duration::from_secs(10))
                        .send()
                        .await;
                });
            }
        }
        "notify_user" => {
            if let (Some(user_id), Some(message)) = (
                action_config.get("user_id").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok()),
                action_config.get("message").and_then(|v| v.as_str()),
            ) {
                let _ = sqlx::query(
                    r#"INSERT INTO notifications (id, tenant_id, user_id, message)
                       VALUES ($1, $2, $3, $4)"#
                )
                .bind(Uuid::new_v4()).bind(tenant_id).bind(user_id).bind(message)
                .execute(db).await;
            }
        }
        _ => {
            tracing::warn!(action = %action_type, "Unknown automation action type");
        }
    }
}

/// Evaluate a delayed "If-Not-Then" action after its wait period.
/// Checks if the expected event occurred. If not, fires the action.
pub async fn evaluate_delayed_action(db: &PgPool, action_id: Uuid) {
    let action = match sqlx::query_as::<_, crate::events::models::DelayedAction>(
        "SELECT * FROM delayed_actions WHERE id = $1 AND executed = false AND cancelled = false"
    )
    .bind(action_id)
    .fetch_optional(db)
    .await
    {
        Ok(Some(a)) => a,
        Ok(None) => return, // already executed or cancelled
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch delayed action");
            return;
        }
    };

    // Check condition: did the expected event happen?
    let should_execute = match action.condition_type.as_str() {
        "timeout" => {
            // Timeout always fires after the wait period
            true
        }
        "no_event" => {
            // Check if the expected event occurred between trigger and now
            if let Some(expected_event) = action.condition_config.get("expected_event").and_then(|v| v.as_str()) {
                let count: i64 = sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND event_type = $2 AND created_at > $3"
                )
                .bind(action.tenant_id)
                .bind(expected_event)
                .bind(action.created_at)
                .fetch_one(db)
                .await.unwrap_or(None).unwrap_or(0);
                count == 0 // fire if NOT seen
            } else {
                true
            }
        }
        "no_action" => {
            // Check if a specific entity action was taken
            let count: i64 = sqlx::query_scalar::<_, Option<i64>>(
                "SELECT COUNT(*) FROM delayed_actions WHERE tenant_id = $1 AND trigger_event_id = $2 AND executed = true"
            )
            .bind(action.tenant_id)
            .bind(action.trigger_event_id)
            .fetch_one(db)
            .await.unwrap_or(None).unwrap_or(0);
            count == 0 // fire if no follow-up action taken
        }
        _ => false,
    };

    if should_execute {
        tracing::info!(action = %action_id, "Condition met — executing delayed action");
        execute_action(
            db,
            action.tenant_id,
            action.id,
            &action.action_type,
            &action.action_config,
            None,
            &serde_json::Value::Null,
        ).await;
    }

    // Mark as executed regardless — condition already evaluated
    let _ = sqlx::query("UPDATE delayed_actions SET executed = true, updated_at = NOW() WHERE id = $1")
        .bind(action_id)
        .execute(db).await;
}
