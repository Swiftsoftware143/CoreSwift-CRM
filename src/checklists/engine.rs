//! Checklist engine — triggers checklists on events and creates progress rows.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Start a checklist for a given entity.
/// Called when trigger event fires (e.g., contact.created, payment.received)
pub async fn trigger_checklist(
    db: &PgPool,
    tenant_id: Uuid,
    trigger_type: &str,
    entity_type: &str,
    entity_id: Uuid,
) {
    // Find matching template
    let templates = match sqlx::query_as::<_, (Uuid, i32, i32)>(
        "SELECT id, stage_count, days_per_stage FROM checklist_templates WHERE tenant_id = $1 AND trigger_type = $2 AND is_active = true LIMIT 1"
    ).bind(tenant_id).bind(trigger_type).fetch_all(db).await {
        Ok(t) => t,
        Err(e) => { tracing::warn!(error=%e, "Failed to find checklist template"); return; }
    };

    for (template_id, _stage_count, _days) in templates {
        let instance = sqlx::query_as::<_, (Uuid,)>(
            r#"INSERT INTO checklist_instances (id, tenant_id, template_id, entity_type, entity_id)
               VALUES ($1, $2, $3, $4, $5) RETURNING id"#,
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(template_id)
        .bind(entity_type)
        .bind(entity_id)
        .fetch_optional(db)
        .await;

        if let Ok(Some((inst_id,))) = instance {
            // Create progress rows for each stage
            let stages = sqlx::query_as::<_, (i32, String, String, i32)>(
                "SELECT stage_order, title, message_template, delay_hours FROM checklist_stages WHERE template_id = $1 ORDER BY stage_order ASC"
            ).bind(template_id).fetch_all(db).await;

            if let Ok(stages) = stages {
                for (order, title, msg_template, delay) in &stages {
                    let _ = sqlx::query(
                        r#"INSERT INTO checklist_progress (id, instance_id, stage_order)
                           VALUES ($1, $2, $3)"#,
                    )
                    .bind(Uuid::new_v4())
                    .bind(inst_id)
                    .bind(order)
                    .execute(db)
                    .await;

                    // Schedule delayed action for this stage
                    let _ = sqlx::query(
                        r#"INSERT INTO delayed_actions (id, tenant_id, trigger_event_id, condition_type, condition_config, action_type, action_config, execute_at)
                           VALUES ($1, $2, NULL, 'timeout', '{}', 'send_email', $3, NOW() + ($4 || ' hours')::INTERVAL)"#
                    )
                    .bind(Uuid::new_v4()).bind(tenant_id)
                    .bind(json!({"to_entity": entity_type, "entity_id": entity_id.to_string(), "template_type": "checklist", "stage_title": title, "message": msg_template}))
                    .bind(delay)
                    .execute(db).await;
                }
            }
        }
    }
}
