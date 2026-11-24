use super::actions;
use super::models::{rule_columns, AutomationRule};
use crate::errors::AppError;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// How long a rule waits before its action runs, in MINUTES, read from `trigger_config`.
///
/// Two spellings are accepted because the console writes the second and an API caller may write the
/// first: `{"delay_minutes": 90}`, or `{"delay": {"value": 3, "unit": "days"}}`. A bare number in
/// `delay` is minutes. Anything unreadable is 0 (fire on match) — never a default the caller did not
/// ask for, because a silent 5-minute wait on a "send instantly" rule is undebuggable.
pub fn delay_minutes(cfg: &Value) -> i64 {
    let raw = if let Some(m) = cfg.get("delay_minutes").and_then(|v| v.as_i64()) {
        m
    } else if let Some(o) = cfg.get("delay").and_then(|v| v.as_object()) {
        let value = o.get("value").and_then(|v| v.as_i64()).unwrap_or(0);
        match o.get("unit").and_then(|v| v.as_str()).unwrap_or("minutes") {
            "seconds" | "second" | "s" => (value + 59) / 60,
            "hours" | "hour" | "h" => value * 60,
            "days" | "day" | "d" => value * 1440,
            _ => value,
        }
    } else if let Some(m) = cfg.get("delay").and_then(|v| v.as_i64()) {
        m
    } else {
        0
    };
    // Cap at 10 years in minutes: make_interval takes an i32, and a nonsensical value must not wrap.
    raw.clamp(0, 5_256_000)
}

/// The rule's condition, read from `trigger_config.condition`.
///
/// `{"type":"no_reply"}` means "only act if THIS contact has not replied since the trigger". A
/// string shorthand (`"no_reply"`) is accepted. `no_event` may name `expected_event`. Absent = no
/// condition (act when the wait elapses).
pub fn condition_of(cfg: &Value) -> Option<(String, Value)> {
    match cfg.get("condition")? {
        Value::String(s) if !s.is_empty() => Some((s.clone(), json!({}))),
        Value::Object(o) => {
            let t = o.get("type").and_then(|v| v.as_str())?.to_string();
            let mut extra = o.clone();
            extra.remove("type");
            Some((t, Value::Object(extra)))
        }
        _ => None,
    }
}

/// What happened when a trigger matched a rule.
#[derive(Debug, PartialEq, Eq)]
pub enum FireOutcome {
    /// The action ran before this call returned.
    Executed,
    /// The action was queued in `delayed_actions` — the engine must NOT run it now.
    Scheduled,
}

async fn count_fire(db: &PgPool, rule_id: Uuid) {
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

/// Run a rule that matched: immediately, or through the delay queue.
///
/// THE ONE PLACE a matched rule turns into work (kanban t_990c386e). Before this, all four trigger
/// evaluators called `actions::execute_action` directly, so a rule could not wait — there was
/// nowhere to put the wait. Now:
///
///   * no delay and no condition  -> execute now, exactly as before (no behaviour change);
///   * a delay (`trigger_config.delay_minutes`) and/or a condition
///     (`trigger_config.condition`)  -> ONE `delayed_actions` row, due at now + delay, carrying the
///     rule id, the trigger's entity and the condition. The 60-second worker pass evaluates it, so
///     the engine never treats "wait 3 days" as "fire now".
///   * `force_schedule` is for the clock-driven triggers (`date.renewal`, `contact.inactive`): they
///     have no event to react to and their queue row is also the ledger that stops the same contact
///     being re-queued on every scan, so they always go through the queue even with a 0 delay.
pub async fn fire_rule(
    db: &PgPool,
    rule: &AutomationRule,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    force_schedule: bool,
) -> Result<FireOutcome, AppError> {
    let delay = delay_minutes(&rule.trigger_config);
    let condition = condition_of(&rule.trigger_config);

    if force_schedule || delay > 0 || condition.is_some() {
        let (condition_type, mut condition_config) = match condition {
            Some((t, extra)) => (t, extra),
            None => ("timeout".to_string(), json!({})),
        };
        let cfg = condition_config
            .as_object_mut()
            .ok_or(AppError::Validation("condition must be an object".into()))?;
        // The subject is carried ON THE ROW (entity_type/entity_id) and repeated in the condition so
        // a reader of the queue can tell what it is about without joining anything.
        cfg.insert("entity_type".into(), json!(entity_type));
        cfg.insert("entity_id".into(), json!(entity_id));
        if entity_type == "contact" {
            cfg.insert("contact_id".into(), json!(entity_id));
        }
        // "since" is the trigger instant. It is what makes the no-reply check about THIS firing
        // rather than about the contact's whole history.
        cfg.insert("since".into(), json!(chrono::Utc::now()));
        if condition_type == "no_event" && !cfg.contains_key("expected_event") {
            cfg.insert("expected_event".into(), json!("email.reply"));
        }

        sqlx::query(
            "INSERT INTO delayed_actions \
                (id, tenant_id, source_rule_id, action_type, action_config, entity_type, entity_id, \
                 condition_type, condition_config, execute_at, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
                     now() + make_interval(mins => $10), now(), now())",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(rule.id)
        .bind(&rule.action_type)
        .bind(&rule.action_config)
        .bind(entity_type)
        .bind(entity_id)
        .bind(&condition_type)
        .bind(&condition_config)
        .bind(delay as i32)
        .execute(db)
        .await?;

        tracing::info!(
            rule = %rule.id, delay_minutes = delay, condition = %condition_type,
            entity = %entity_id, "Automation queued (delay/condition), not executed now"
        );
        return Ok(FireOutcome::Scheduled);
    }

    actions::execute_action(db, rule, tenant_id, entity_type, entity_id).await?;
    count_fire(db, rule.id).await;
    Ok(FireOutcome::Executed)
}

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
///
/// `pub(crate)` since kanban t_990c386e: the event dispatcher and the clock scanner ask the same
/// question through this function rather than re-implementing it.
pub(crate) async fn automations_enabled(db: &PgPool, tenant_id: Uuid) -> bool {
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
                // A RULE THAT RAN MUST SAY SO (measured 2026-11-21): the action executed and the
                // contact got the tag it was told to apply, while `execution_count` stayed 0 - so the
                // one number an owner uses to ask "is this automation working?" reported that it never
                // ran. Both counters are now written here, on success and on failure, because a rule
                // that fires and fails is not the same as a rule nobody triggered.
                //
                // Delay/condition (kanban t_990c386e): `fire_rule` either executes now (and counts)
                // or QUEUES the action and returns Scheduled — a queued action must not be counted
                // here, or "executed 1" would be reported for a mail that has not been sent yet. The
                // worker counts it when it actually runs.
                match fire_rule(db, &rule, tenant_id, entity_type, entity_id, false).await {
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(rule = %rule.id, action = %rule.action_type, error = %e, "Automation action failed");
                        count_fire(db, rule.id).await;
                    }
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
            // Same as the tag path: a dropped action error left no trace anywhere, and a delay or a
            // condition must queue instead of firing (kanban t_990c386e).
            if let Err(e) = fire_rule(db, &rule, tenant_id, "contact", contact_id, false).await {
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
                        fire_rule(db, &rule, tenant_id, "contact", contact_id, false).await
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
                // A DEAL IS ABOUT A PERSON (measured 2026-11-21): the stage trigger passed the
                // opportunity itself, so an "AddTag" action tagged the DEAL and the customer never got
                // the tag — "won deal -> customer gets the Customer tag" was impossible to express.
                // The opportunity's contact is the subject when it has one; the deal stays the subject
                // only when the deal carries no contact.
                let contact: Option<Uuid> = sqlx::query_scalar(
                    "SELECT contact_id FROM opportunities WHERE id = $1 AND tenant_id = $2",
                )
                .bind(opportunity_id)
                .bind(tenant_id)
                .fetch_optional(db)
                .await
                .ok()
                .flatten()
                .flatten();
                let (entity_type, entity_id) = match contact {
                    Some(c) => ("contact", c),
                    None => ("opportunity", opportunity_id),
                };
                if let Err(e) = fire_rule(db, &rule, tenant_id, entity_type, entity_id, false).await
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
