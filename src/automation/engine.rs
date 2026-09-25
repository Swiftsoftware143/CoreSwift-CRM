use super::actions;
use super::models::{AutomationRule, RULE_COLUMNS};
use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

/// Fire a tenant's `TagAdded` / `TagRemoved` automations.
///
/// WHO CALLS THIS — the rule decided in kanban t_56dddec2. Read it before touching any
/// `tag_assignments` writer, and before "fixing" a site that deliberately does not call it:
///
/// * FIRE: a tag assignment **a caller asked this app to make**, and only when the row ACTUALLY
///   came into existence — gate on `rows_affected() > 0`, because every one of these statements is
///   `ON CONFLICT DO NOTHING` and a repeat must not re-fire:
///   `POST /api/tags/assign` and `DELETE /api/tags/assign/:id` (src/tags/handlers.rs), the webhook
///   `tags.assign` / `tags.unassign` actions (src/webhook/actions.rs), the hub lead intake
///   `POST /api/external/contacts` (src/external_api.rs), the satellite capture
///   `POST /inbound/.../contact-sync` (src/inbound/handlers.rs), the inbound tag sync
///   `POST /api/v1/webhooks/cross-app/tag-sync` (`tags` and `removed_tags`,
///   src/webhooks/cross_app_tag_sync.rs) and `POST /api/internal/tags/assign`
///   (src/tags/internal_handler.rs).
/// * NO-FIRE, on purpose — do NOT add a call at these three kinds of site:
///   1. a rule/scoring ACTION that writes a tag (`exec_add_tag` / `exec_remove_tag` in
///      src/automation/actions.rs, the `add_tag` arm in src/events/dispatcher.rs, the `assign_tag`
///      arm in src/scoring/engine.rs). They are executed BY this evaluator
///      (`evaluate_tag_triggers` -> `actions::execute_action`), so fanning out from them would
///      re-enter the engine that invoked them — a TagAdded rule whose action is AddTag would
///      chain forever.
///   2. tag LIFECYCLE (src/tags/internal_handler.rs `internal_delete_tag`): deleting the tag
///      object takes its assignment rows with it via `ON DELETE CASCADE`. The tenant asked to
///      delete a tag, not to remove a tag from N contacts.
///   3. tenant PROVISIONING (src/tag_provision_handler.rs): the tenant is created by the same
///      request, so the `Free` marker applied there is the new tenant's birth state, and it
///      provably has no rule to fire (measured: 0 `automation_rules` for that tenant).
pub async fn fire_tag_trigger(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    tag_id: Uuid,
    trigger_type: &str,
) {
    if let Err(e) =
        evaluate_tag_triggers(db, tenant_id, entity_type, entity_id, tag_id, trigger_type).await
    {
        tracing::error!("Tag trigger eval error: {e:?}");
    }
}

pub async fn evaluate_tag_triggers(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    tag_id: Uuid,
    trigger_type: &str,
) -> Result<(), AppError> {
    // Try matching the legacy trigger_type first, then the new style
    let trigger_types = match trigger_type {
        "TagAdded" => vec!["TagAdded", "tag.assigned"],
        "TagRemoved" => vec!["TagRemoved", "tag.unassigned"],
        _ => vec![trigger_type],
    };

    for tt in &trigger_types {
        let rules = sqlx::query_as::<_, AutomationRule>(&format!(
            "SELECT {RULE_COLUMNS} FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_active IS NOT FALSE"
        ))
        .bind(tenant_id).bind(tt).fetch_all(db).await?;

        for rule in rules {
            // Check trigger_config for tag_id match
            // Supports both: {"tag_id": "<uuid>"} and {"tag_ids": ["<uuid>", ...]}
            let matches =
                if let Some(tid_str) = rule.trigger_config.get("tag_id").and_then(|v| v.as_str()) {
                    if let Ok(conf_tid) = Uuid::parse_str(tid_str) {
                        conf_tid == tag_id || tid_str == "*"
                    } else {
                        false
                    }
                } else if let Some(tag_ids) = rule
                    .trigger_config
                    .get("tag_ids")
                    .and_then(|v| v.as_array())
                {
                    tag_ids
                        .iter()
                        .any(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()) == Some(tag_id))
                } else {
                    false
                };

            if matches {
                // A failing action used to be dropped here (`let _ =`), so an AddTag that tripped
                // tag_assignments' unique constraint was a silent no-op with no operator signal.
                if let Err(e) =
                    actions::execute_action(db, &rule, tenant_id, entity_type, entity_id).await
                {
                    tracing::warn!(rule = %rule.id, action = %rule.action_type, error = %e, "Automation action failed");
                }
            }
        }
    }
    Ok(())
}

pub async fn fire_score_trigger(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    total_score: i32,
    category: &str,
) {
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>(&format!("SELECT {RULE_COLUMNS} FROM automation_rules WHERE tenant_id=$1 AND trigger_type='ScoreChanged' AND is_active IS NOT FALSE"))
        .bind(tenant_id).fetch_all(db).await else { return };
    for rule in rules {
        let should = match rule.trigger_config.get("category").and_then(|v| v.as_str()) {
            Some(cat) => cat == category,
            None => {
                let min = rule
                    .trigger_config
                    .get("min_score")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(i64::MIN);
                let max = rule
                    .trigger_config
                    .get("max_score")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(i64::MAX);
                (total_score as i64) >= min && (total_score as i64) <= max
            }
        };
        if should {
            // Same as the tag path below: a dropped action error left no trace anywhere.
            if let Err(e) =
                actions::execute_action(db, &rule, tenant_id, "contact", contact_id).await
            {
                tracing::warn!(rule = %rule.id, action = %rule.action_type, error = %e, "Automation action failed");
            }
        }
    }
}

pub async fn fire_list_trigger(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    list_id: Uuid,
    trigger_type: &str,
) {
    // `trigger_type` is a plain varchar(50) column — the dropped `::trigger_type` cast named an
    // enum type that does not exist in this database (42704), which silently turned every
    // list-triggered rule into a no-op (the `let Ok(...) else { return }` below swallowed it).
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>(&format!("SELECT {RULE_COLUMNS} FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_active IS NOT FALSE"))
        .bind(tenant_id).bind(trigger_type).fetch_all(db).await else { return };
    for rule in rules {
        if let Some(lid_str) = rule.trigger_config.get("list_id").and_then(|v| v.as_str()) {
            if let Ok(conf_lid) = Uuid::parse_str(lid_str) {
                if conf_lid == list_id {
                    if let Err(e) =
                        actions::execute_action(db, &rule, tenant_id, "contact", contact_id).await
                    {
                        tracing::warn!(rule = %rule.id, action = %rule.action_type, error = %e, "Automation action failed");
                    }
                }
            }
        }
    }
}
