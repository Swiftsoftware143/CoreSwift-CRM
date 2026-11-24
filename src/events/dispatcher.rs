//! Event Dispatcher — routes incoming events to automation, delayed evaluation, and webhooks.
//!
//! This is the core of the Flawless Follow-up system:
//! 1. Incoming webhook arrives → stored in events table
//! 2. dispatcher checks automation rules for matching triggers
//! 3. If a delay is required, delayed_actions table tracks it
//! 4. After timeout, evaluate_delayed_action checks condition and fires

use crate::automation::models::{rule_columns, AutomationRule};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Fire the tenant's ACTIVE rules for one event label — the rules half of `dispatch_automation`,
/// split out so a door that emits a NEW event can run automations without also starting onboarding
/// checklists and health signals the event was never about (kanban t_990c386e: `contact.created`
/// is dispatched from the contact writers with exactly this call).
///
/// Every match goes through `automation::engine::fire_rule`, the ONE executor, so a rule fired by an
/// ingested event honours its delay, its no-reply condition and its `template_id` — the same way a
/// tag/list/score/stage match does. Before this the event path had its own naive executor that read
/// `action_config.to/subject/body` and ignored templates and conditions entirely.
pub async fn dispatch_rule_event(
    db: &PgPool,
    tenant_id: Uuid,
    event_type: &str,
    entity_type: Option<&str>,
    entity_id: Option<Uuid>,
) {
    if !crate::automation::engine::automations_enabled(db, tenant_id).await {
        return;
    }
    let rules = match sqlx::query_as::<_, AutomationRule>(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM automation_rules WHERE tenant_id = $1 AND trigger_type = $2 \
         AND is_active IS NOT FALSE ORDER BY execution_count ASC"
    ))
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

    for rule in rules {
        // Optional entity-type filter, unchanged from the original loop.
        if let Some(conditions) = rule.trigger_config.as_object() {
            if let Some(entity_match) = conditions.get("entity_type") {
                if let Some(et) = entity_type {
                    if entity_match.as_str() != Some(et) {
                        continue;
                    }
                }
            }
        }

        match (entity_type, entity_id) {
            (Some(et), Some(eid)) => {
                if let Err(e) =
                    crate::automation::engine::fire_rule(db, &rule, tenant_id, et, eid, false).await
                {
                    tracing::warn!(rule = %rule.id, action = %rule.action_type, error = %e, "Automation action failed");
                }
            }
            // No subject: the action cannot be queued per-entity, so keep the legacy path.
            _ => {
                execute_action(
                    db,
                    tenant_id,
                    rule.id,
                    &rule.action_type,
                    &rule.action_config,
                    None,
                    &serde_json::Value::Null,
                )
                .await;
                let _ = sqlx::query("UPDATE automation_rules SET execution_count = COALESCE(execution_count,0) + 1, last_executed_at = NOW() WHERE id = $1")
                    .bind(rule.id)
                    .execute(db)
                    .await;
            }
        }
    }
}

/// Dispatch an event to all matching automation rules for a tenant.
/// Fire-and-forget — logs failures but never blocks.
/// Also triggers onboarding checklists and records health signals for matching events.
pub async fn dispatch_automation(
    db: &PgPool,
    tenant_id: Uuid,
    event_type: &str,
    entity_type: Option<&str>,
    entity_id: Option<Uuid>,
    _payload: &serde_json::Value,
) {
    dispatch_rule_event(db, tenant_id, event_type, entity_type, entity_id).await;

    // Also trigger onboarding checklists for matching events
    match event_type {
        "contact.created" | "signup" | "trial.started" | "payment.received" => {
            if let (Some(et), Some(eid)) = (entity_type, entity_id) {
                crate::checklists::engine::trigger_checklist(db, tenant_id, event_type, et, eid)
                    .await;
            }
        }
        _ => {}
    }

    // Record health signal for positive interactions
    match event_type {
        "login" | "feature_used" | "api_call" => {
            if let Some(eid) = entity_id {
                crate::monitoring::engine::record_signal(
                    db,
                    tenant_id,
                    entity_type.unwrap_or("contact"),
                    eid,
                    event_type,
                    1,
                )
                .await;
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
            // DELIBERATELY NO TAG FAN-OUT (kanban t_56dddec2). This arm is an automation rule
            // ACTION (`dispatch_automation` -> here), i.e. it runs inside the evaluator for a rule
            // whose trigger already fired; firing `TagAdded` from here would re-enter the engine.
            // See the rule on `crate::automation::engine::fire_tag_trigger`.
            if let (Some(eid), Some(tag_name)) = (
                entity_id,
                action_config.get("tag_name").and_then(|v| v.as_str()),
            ) {
                let _ = sqlx::query(
                    r#"INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id)
                       SELECT uuid_generate_v4(), t.id, 'contact', $1
                       FROM tags t WHERE t.tenant_id = $2 AND t.name = $3
                       ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"#,
                )
                .bind(eid)
                .bind(tenant_id)
                .bind(tag_name)
                .execute(db)
                .await;
            }
        }
        "send_email" => {
            if let (Some(to), Some(subject), Some(body)) = (
                action_config.get("to").and_then(|v| v.as_str()),
                action_config.get("subject").and_then(|v| v.as_str()),
                action_config.get("body").and_then(|v| v.as_str()),
            ) {
                // Queue via the AUTOMATION lane: the worker delivers it from the account's
                // `mg.<domain>` subdomain, never the main domain (kanban t_044d5d62). The lane is
                // derived at queue time, so the worker needs no second resolution.
                let automation_domain =
                    crate::communications::providers::account_main_domain(db, tenant_id)
                        .await
                        .map(|base| crate::communications::providers::automation_subdomain(&base));
                let _ = sqlx::query(
                    r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status, automation_domain)
                       VALUES ($1, $2, 'email', $3, $4, $5, 'queued', $6)"#,
                )
                .bind(Uuid::new_v4())
                .bind(tenant_id)
                .bind(to)
                .bind(subject)
                .bind(body)
                .bind(automation_domain.as_deref())
                .execute(db)
                .await;
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
                    let _ = client
                        .post(&url_owned)
                        .json(&payload)
                        .timeout(std::time::Duration::from_secs(10))
                        .send()
                        .await;
                });
            }
        }
        "notify_user" => {
            if let (Some(user_id), Some(message)) = (
                action_config
                    .get("user_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| Uuid::parse_str(s).ok()),
                action_config.get("message").and_then(|v| v.as_str()),
            ) {
                // notifications.title is NOT NULL with no default: the action's own label if it has
                // one, else the first 255 characters of its own message (t_ed3c2591).
                let title = crate::notifications::title::resolve(
                    action_config.get("title").and_then(|v| v.as_str()),
                    action_config.get("subject").and_then(|v| v.as_str()),
                    message,
                );
                if let Err(e) = sqlx::query(
                    r#"INSERT INTO notifications (id, tenant_id, user_id, title, message)
                       VALUES ($1, $2, $3, $4, $5)"#,
                )
                .bind(Uuid::new_v4())
                .bind(tenant_id)
                .bind(user_id)
                .bind(title)
                .bind(message)
                .execute(db)
                .await
                {
                    tracing::error!(tenant = %tenant_id, user = %user_id, rule = %rule_id, error = %e, "notify_user notification not written");
                }
            }
        }
        _ => {
            tracing::warn!(action = %action_type, "Unknown automation action type");
        }
    }
}

/// Did THIS contact send/reply since `since`?
///
/// Two real signals, because a reply can arrive through either door and neither covers both:
///   * `private_email_messages` — inbound mail filed by the private-email receiver, matched on the
///     contact's own email address (case-insensitive). `direction='inbound'` only: our own outgoing
///     mail must never count as a reply.
///   * `events` — an ingested reply-ish event whose subject is that contact. The vocabulary is
///     matched case-insensitively so `email.reply` and `Email.Reply` cannot disagree.
///
/// This is the honest boundary of "replied": a reply typed into some surface that writes neither of
/// these is invisible here, so the condition would fire. That is why the console labels the checkbox
/// "they have not replied" and the guide says what counts (see www-app/coreswift/guide.html).
async fn contact_replied_since(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        r#"SELECT EXISTS(
               SELECT 1 FROM private_email_messages m
                 JOIN contacts c ON c.id = $2 AND c.tenant_id = m.tenant_id
                WHERE m.tenant_id = $1 AND m.direction = 'inbound'
                  AND m.received_at > $3
                  AND c.email IS NOT NULL AND lower(m.from_address) = lower(c.email)
           ) OR EXISTS(
               SELECT 1 FROM events e
                WHERE e.tenant_id = $1 AND e.created_at > $3
                  AND e.entity_type = 'contact' AND e.entity_id = $2
                  AND lower(e.event_type) IN ('email.reply','emailed.reply','reply','inbound.email','email_received')
           )"#,
    )
    .bind(tenant_id)
    .bind(contact_id)
    .bind(since)
    .fetch_one(db)
    .await
}

fn is_reply_event(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "email.reply" | "emailed.reply" | "reply" | "inbound.email" | "email_received"
    )
}

/// Evaluate a delayed "If-Not-Then" action after its wait period.
/// Checks if the expected event occurred. If not, fires the action.
///
/// CONTACT-SCOPED (kanban t_990c386e). The `no_event` arm used to count `events` rows for the whole
/// TENANT (`WHERE tenant_id=$1 AND event_type=$2 AND created_at>$3`), so "wait 3 days and follow up
/// if they have not replied" fired only if NOBODY in the tenant had replied to anything in 3 days —
/// on any account with traffic, a no-reply sequence could never run. The condition is now about one
/// contact when the row names one (`condition_config.contact_id`, or `entity_type='contact'`), and
/// `no_reply` is the explicit spelling the console writes.
///
/// A row queued by a RULE (`source_rule_id`) is executed through `automation::actions`, the same
/// executor every other trigger path uses, so a template, a contact-addressed email and every other
/// action behaviour are identical whether the wait was zero or three days. Legacy rows (monitoring,
/// checklists, the public API) keep their old executor.
pub async fn evaluate_delayed_action(db: &PgPool, action_id: Uuid) {
    let action = match sqlx::query_as::<_, crate::events::models::DelayedAction>(
        "SELECT * FROM delayed_actions WHERE id = $1 AND executed = false AND cancelled = false",
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

    // Who this row is about, and from when. `since` is the trigger instant written by the engine;
    // rows from older writers fall back to the row's own created_at, which is the same meaning.
    let contact_id: Option<Uuid> = action
        .condition_config
        .get("contact_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .or_else(|| {
            if action.entity_type.as_deref() == Some("contact") {
                action.entity_id
            } else {
                None
            }
        });
    let since: chrono::DateTime<chrono::Utc> = action
        .condition_config
        .get("since")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
        .unwrap_or(action.created_at);
    let expected_event = action
        .condition_config
        .get("expected_event")
        .and_then(|v| v.as_str())
        .unwrap_or("email.reply")
        .to_string();

    // `condition_type` is Option: src/private_email/auto_reply_handler.rs:250 queues its delayed
    // action without one (migration 073 made the column nullable for that writer), so a NULL is
    // real data — "no condition recorded" — and never fires. Before this it was a decode error,
    // which returned early and left the row pending forever (re-fetched every tick).
    let (should_execute, reason) = match action.condition_type.as_deref() {
        Some("timeout") => (true, "timeout elapsed".to_string()),
        Some("no_reply") => match contact_id {
            Some(cid) => match contact_replied_since(db, action.tenant_id, cid, since).await {
                Ok(true) => (
                    false,
                    "contact replied since the trigger — not sent".to_string(),
                ),
                Ok(false) => (
                    true,
                    "no reply from this contact since the trigger".to_string(),
                ),
                Err(e) => (
                    false,
                    format!("reply check failed ({e}) — refused rather than sent blind"),
                ),
            },
            // Fail CLOSED: a no-reply condition with no subject cannot be evaluated, and guessing
            // "probably no reply" would send mail nobody can justify.
            None => (
                false,
                "no_reply condition carries no contact — refused".to_string(),
            ),
        },
        Some("no_event") => {
            // Contact-scoped when we know the subject; the reply vocabulary also consults inbound
            // mail, which `events` alone would miss.
            if let Some(cid) = contact_id {
                let seen = if is_reply_event(&expected_event) {
                    contact_replied_since(db, action.tenant_id, cid, since).await
                } else {
                    sqlx::query_scalar::<_, bool>(
                        r#"SELECT EXISTS(
                               SELECT 1 FROM events e
                                WHERE e.tenant_id = $1 AND e.event_type = $2 AND e.created_at > $3
                                  AND e.entity_type = 'contact' AND e.entity_id = $4
                           )"#,
                    )
                    .bind(action.tenant_id)
                    .bind(&expected_event)
                    .bind(since)
                    .bind(cid)
                    .fetch_one(db)
                    .await
                };
                match seen {
                    Ok(true) => (false, format!("{expected_event} seen for this contact")),
                    Ok(false) => (true, format!("no {expected_event} for this contact")),
                    Err(e) => (false, format!("event check failed ({e}) — refused")),
                }
            } else {
                // Legacy tenant-wide arm, unchanged for the writers that queued no subject.
                let count: i64 = sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND event_type = $2 AND created_at > $3"
                )
                .bind(action.tenant_id)
                .bind(&expected_event)
                .bind(since)
                .fetch_one(db)
                .await.unwrap_or(None).unwrap_or(0);
                (
                    count == 0,
                    format!("{count} tenant-wide {expected_event} rows"),
                )
            }
        }
        Some("no_action") => match action.trigger_event_id {
            // trigger_event_id is nullable by design (067: REFERENCES events(id) ON DELETE SET
            // NULL — and ScheduleDelayedRequest accepts an omitted id). A NULL can never equal a
            // row's value, so no follow-up can be counted and the action fires, which is the
            // intent of the "fire if no follow-up action taken" comment below.
            None => (true, "no trigger event — fires".to_string()),
            Some(trigger_event_id) => {
                let count: i64 = sqlx::query_scalar::<_, Option<i64>>(
                    "SELECT COUNT(*) FROM delayed_actions WHERE tenant_id = $1 AND trigger_event_id = $2 AND executed = true"
                )
                .bind(action.tenant_id)
                .bind(trigger_event_id)
                .fetch_one(db)
                .await.unwrap_or(None).unwrap_or(0);
                (count == 0, format!("{count} follow-up actions executed"))
            }
        },
        _ => (false, "no condition type recorded".to_string()),
    };

    let mut executed = false;
    if should_execute {
        tracing::info!(action = %action_id, %reason, "Condition met — executing delayed action");
        match (action.source_rule_id, action.entity_id) {
            // A row queued by a RULE runs the RULE's action, through the one executor.
            (Some(rule_id), Some(entity_id)) => {
                match sqlx::query_as::<_, AutomationRule>(concat!(
                    "SELECT ",
                    rule_columns!(),
                    " FROM automation_rules WHERE id = $1 AND tenant_id = $2"
                ))
                .bind(rule_id)
                .bind(action.tenant_id)
                .fetch_optional(db)
                .await
                {
                    Ok(Some(mut rule)) => {
                        // The QUEUED copy wins: a rule edited between queueing and firing must not
                        // silently change what the customer was promised at trigger time.
                        rule.action_type = action.action_type.clone();
                        rule.action_config = action.action_config.clone();
                        let entity_type = action
                            .entity_type
                            .clone()
                            .unwrap_or_else(|| "contact".to_string());
                        match crate::automation::actions::execute_action(
                            db,
                            &rule,
                            action.tenant_id,
                            &entity_type,
                            entity_id,
                        )
                        .await
                        {
                            Ok(()) => {
                                executed = true;
                                let _ = sqlx::query(
                                    "UPDATE automation_rules \
                                        SET execution_count = COALESCE(execution_count, 0) + 1, \
                                            last_executed_at = now() \
                                      WHERE id = $1",
                                )
                                .bind(rule_id)
                                .execute(db)
                                .await;
                            }
                            Err(e) => {
                                tracing::warn!(rule = %rule_id, action = %rule.action_type, error = %e, "Delayed automation action failed");
                                let _ = sqlx::query(
                                    "UPDATE automation_rules \
                                        SET execution_count = COALESCE(execution_count, 0) + 1, \
                                            last_executed_at = now() \
                                      WHERE id = $1",
                                )
                                .bind(rule_id)
                                .execute(db)
                                .await;
                            }
                        }
                    }
                    // The rule was deleted while the action waited: run nothing, record why.
                    Ok(None) => {
                        tracing::info!(rule = %rule_id, "Delayed action's rule was deleted — skipped");
                    }
                    Err(e) => {
                        tracing::warn!(rule = %rule_id, error = %e, "Could not load delayed action's rule")
                    }
                }
            }
            // Legacy rows (monitoring, checklists, the public API) keep their own executor.
            _ => {
                execute_action(
                    db,
                    action.tenant_id,
                    action.id,
                    &action.action_type,
                    &action.action_config,
                    action.entity_id,
                    &serde_json::Value::Null,
                )
                .await;
                executed = true;
            }
        }
    }

    // Mark as executed regardless — condition already evaluated. `result` records WHY, so the
    // evidence for "this follow-up was suppressed because the contact replied" is a row, not a log
    // line that rotated away.
    let _ = sqlx::query(
        "UPDATE delayed_actions SET executed = true, result = $2, updated_at = NOW() WHERE id = $1",
    )
    .bind(action_id)
    .bind(json!({
        "condition": action.condition_type,
        "reason": reason,
        "executed": executed,
        "contact_id": contact_id,
    }))
    .execute(db)
    .await;
}
