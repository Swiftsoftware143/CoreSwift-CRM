use sqlx::PgPool;
use uuid::Uuid;
use crate::errors::AppError;
use super::models::AutomationRule;
use super::actions;

pub async fn fire_tag_trigger(db: &PgPool, tenant_id: Uuid, entity_type: &str, entity_id: Uuid, tag_id: Uuid, trigger_type: &str) {
    if let Err(e) = evaluate_tag_triggers(db, tenant_id, entity_type, entity_id, tag_id, trigger_type).await {
        tracing::error!("Tag trigger eval error: {e:?}");
    }
}

pub async fn evaluate_tag_triggers(db: &PgPool, tenant_id: Uuid, entity_type: &str, entity_id: Uuid, tag_id: Uuid, trigger_type: &str) -> Result<(), AppError> {
    // Try matching the legacy trigger_type first, then the new style
    let trigger_types = match trigger_type {
        "TagAdded" => vec!["TagAdded", "tag.assigned"],
        "TagRemoved" => vec!["TagRemoved", "tag.unassigned"],
        _ => vec![trigger_type],
    };

    for tt in &trigger_types {
        let rules = sqlx::query_as::<_, AutomationRule>(
            "SELECT * FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_enabled=true"
        )
        .bind(tenant_id).bind(tt).fetch_all(db).await?;

        for rule in rules {
            // Check trigger_config for tag_id match
            // Supports both: {"tag_id": "<uuid>"} and {"tag_ids": ["<uuid>", ...]}
            let matches = if let Some(tid_str) = rule.trigger_config.get("tag_id").and_then(|v| v.as_str()) {
                if let Ok(conf_tid) = Uuid::parse_str(tid_str) {
                    conf_tid == tag_id || tid_str == "*"
                } else { false }
            } else if let Some(tag_ids) = rule.trigger_config.get("tag_ids").and_then(|v| v.as_array()) {
                tag_ids.iter().any(|v| {
                    v.as_str().and_then(|s| Uuid::parse_str(s).ok()) == Some(tag_id)
                })
            } else {
                false
            };

            if matches {
                let _ = actions::execute_action(db, &rule, tenant_id, entity_type, entity_id).await;
            }
        }
    }
    Ok(())
}

pub async fn fire_score_trigger(db: &PgPool, tenant_id: Uuid, contact_id: Uuid, total_score: i32, category: &str) {
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>("SELECT * FROM automation_rules WHERE tenant_id=$1 AND trigger_type='ScoreChanged' AND is_enabled=true")
        .bind(tenant_id).fetch_all(db).await else { return };
    for rule in rules {
        let should = match rule.trigger_config.get("category").and_then(|v| v.as_str()) {
            Some(cat) => cat == category,
            None => {
                let min = rule.trigger_config.get("min_score").and_then(|v| v.as_i64()).unwrap_or(i64::MIN);
                let max = rule.trigger_config.get("max_score").and_then(|v| v.as_i64()).unwrap_or(i64::MAX);
                (total_score as i64) >= min && (total_score as i64) <= max
            }
        };
        if should { let _ = actions::execute_action(db, &rule, tenant_id, "contact", contact_id).await; }
    }
}

pub async fn fire_list_trigger(db: &PgPool, tenant_id: Uuid, contact_id: Uuid, list_id: Uuid, trigger_type: &str) {
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>("SELECT * FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2::trigger_type AND is_enabled=true")
        .bind(tenant_id).bind(trigger_type).fetch_all(db).await else { return };
    for rule in rules {
        if let Some(lid_str) = rule.trigger_config.get("list_id").and_then(|v| v.as_str()) {
            if let Ok(conf_lid) = Uuid::parse_str(lid_str) {
                if conf_lid == list_id { let _ = actions::execute_action(db, &rule, tenant_id, "contact", contact_id).await; }
            }
        }
    }
}
