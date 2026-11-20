use super::actions;
use super::models::{rule_columns, AutomationRule};
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

/// Does this account's plan include automations at all?
///
/// ONE question, asked in ONE place, because the three trigger paths were answering it differently:
/// measured 2026-11-20, the tag path refused to fire for a workspace whose plan carries no automations,
/// while the list path and the score path did not — so the same workspace could run an automation
/// through a list that it could not run through a tag. A gate that only some doors check is not a gate.
async fn automations_enabled(db: &PgPool, tenant_id: Uuid) -> bool {
    match crate::module_registry::resolve(db, tenant_id, "automation").await {
        Ok(m) => m.enabled,
        Err(e) => {
            tracing::warn!(tenant = %tenant_id, error = %e,
                "automation entitlement could not be resolved; refusing to fire (fail closed)");
            false
        }
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
    // PLAN GATE (migration 114, kanban t_9b2e0c3e) — the "Enterprise automations" half of the Tags
    // row in the plan table. This evaluator is reached from the TAG paths (src/tags/triggers.rs, the
    // `tags.assign` webhook actions, the satellite tag sync, the internal tag assign), NOT from the
    // /api/automation router's `automation` gate — so without this check a workspace whose plan does
    // not include automations would keep firing every rule it had. Refusing here is a NO-FIRE, not an
    // error: the callers are fire-and-forget (`fire_tag_trigger`), and a plan without automations
    // simply runs none. The `no_plan` arm still grants (module_registry, ARM (a)).
    if !automations_enabled(db, tenant_id).await {
        return Ok(());
    }

    // Try matching the legacy trigger_type first, then the new style
    let trigger_types = match trigger_type {
        "TagAdded" => vec!["TagAdded", "tag.assigned"],
        "TagRemoved" => vec!["TagRemoved", "tag.unassigned"],
        _ => vec![trigger_type],
    };

    for tt in &trigger_types {
        let rules = sqlx::query_as::<_, AutomationRule>(concat!(
            "SELECT ", rule_columns!(), " FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_active IS NOT FALSE"
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
    if !automations_enabled(db, tenant_id).await {
        return;
    }
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>(concat!("SELECT ", rule_columns!(), " FROM automation_rules WHERE tenant_id=$1 AND trigger_type='ScoreChanged' AND is_active IS NOT FALSE"))
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
    // The entitlement question, asked here too (see `automations_enabled`): without it this path was
    // the way around the gate the tag path honours.
    if !automations_enabled(db, tenant_id).await {
        return;
    }
    // `trigger_type` is a plain varchar(50) column — the dropped `::trigger_type` cast named an
    // enum type that does not exist in this database (42704), which silently turned every
    // list-triggered rule into a no-op (the `let Ok(...) else { return }` below swallowed it).
    let Ok(rules) = sqlx::query_as::<_, AutomationRule>(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_active IS NOT FALSE"
    ))
    .bind(tenant_id)
    .bind(trigger_type)
    .fetch_all(db)
    .await
    else {
        return;
    };
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

/// Fire a tenant's `StageChanged` automations when a DEAL enters a stage.
///
/// The label has been accepted by `POST /api/automation/rules` since the engine shipped, but
/// nothing emitted it: measured 2026-11-20, `grep -rn StageChanged src/` found the rule-creation
/// validation and no caller, so every StageChanged rule a customer wrote was decorative. This is
/// the emitter, reached from the ONE deal writer (`crate::deals::writer::fire_stage_hooks`) — a
/// stage move, a status transition that lands on a closed stage, and an API push onto a stage all
/// pass through it.
///
/// WHO CALLS THIS: only `fire_stage_hooks`, and only when the stage carries an active
/// `deal_stage_automations` row with `action_kind = 'workflow'`. That opt-in is deliberate: a
/// stage with no hook must not run a rule somebody wrote months ago for a different stage.
pub async fn fire_stage_trigger(
    db: &PgPool,
    tenant_id: Uuid,
    opportunity_id: Uuid,
    stage_id: Uuid,
    moved_by: Option<Uuid>,
) {
    if !automations_enabled(db, tenant_id).await {
        return;
    }
    // Legacy label first, then the dotted spelling, so a rule written either way fires.
    for trigger_type in ["StageChanged", "stage.changed"] {
        let Ok(rules) = sqlx::query_as::<_, AutomationRule>(concat!(
            "SELECT ",
            rule_columns!(),
            " FROM automation_rules WHERE tenant_id=$1 AND trigger_type=$2 AND is_active IS NOT FALSE"
        ))
        .bind(tenant_id)
        .bind(trigger_type)
        .fetch_all(db)
        .await
        else {
            return;
        };

        for rule in rules {
            let matches =
                if let Some(sid) = rule.trigger_config.get("stage_id").and_then(|v| v.as_str()) {
                    if sid == "*" {
                        true
                    } else {
                        Uuid::parse_str(sid)
                            .map(|id| id == stage_id)
                            .unwrap_or(false)
                    }
                } else if let Some(ids) = rule
                    .trigger_config
                    .get("stage_ids")
                    .and_then(|v| v.as_array())
                {
                    ids.iter()
                        .any(|v| v.as_str().and_then(|s| Uuid::parse_str(s).ok()) == Some(stage_id))
                } else {
                    // A rule with no stage filter means "any stage" — that is the only sensible
                    // reading, and it makes the label usable without editing the rule first.
                    true
                };

            if matches {
                if let Err(e) =
                    actions::execute_action(db, &rule, tenant_id, "opportunity", opportunity_id)
                        .await
                {
                    tracing::warn!(
                        rule = %rule.id,
                        action = %rule.action_type,
                        moved_by = ?moved_by,
                        error = %e,
                        "Automation action failed"
                    );
                }
            }
        }
    }
}
