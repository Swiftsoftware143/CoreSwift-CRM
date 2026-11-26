use super::models::*;
use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

/// Cancel every queue row this rule still has waiting — the LEDGER is kept, the SEND is not.
///
/// Kanban t_b54589a3: pausing a rule used to flip `is_active` and nothing else, so a step that was
/// already sitting in `delayed_actions` fired anyway when its wait elapsed. `cancelled = true` is
/// what the 60-second worker pass filters on (`executed = false AND cancelled = false`), so a
/// cancelled row can never run; `result.reason` is what the queue screen shows the operator
/// instead of silence. Called for a rule that is now (or was created) inactive, and before a rule
/// is deleted — a deleted rule's rows would otherwise have `source_rule_id` set to NULL by the FK
/// and be picked up by the legacy executor, i.e. fire with nobody able to see why.
async fn cancel_pending_for_rule<'e, E>(
    executor: E,
    tenant_id: Uuid,
    rule_id: Uuid,
) -> Result<u64, sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let res = sqlx::query(
        "UPDATE delayed_actions \
            SET cancelled = true, \
                status = 'cancelled', \
                result = jsonb_build_object( \
                    'condition', condition_type, \
                    'reason', 'rule paused - not sent', \
                    'executed', false), \
                updated_at = NOW() \
          WHERE source_rule_id = $1 AND tenant_id = $2 \
            AND executed = false AND cancelled = false",
    )
    .bind(rule_id)
    .bind(tenant_id)
    .execute(executor)
    .await?;
    Ok(res.rows_affected())
}

/// The rule as JSON with the number of steps this request cancelled beside it. Every existing
/// field is unchanged, so callers that read the rule body keep working.
fn rule_with_cancelled(
    rule: &AutomationRule,
    cancelled: u64,
) -> Result<serde_json::Value, AppError> {
    let mut body = serde_json::to_value(rule).map_err(|e| AppError::Internal(e.to_string()))?;
    if let Some(obj) = body.as_object_mut() {
        obj.insert("cancelled_pending".to_string(), json!(cancelled));
    }
    Ok(body)
}

pub async fn list_rules(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(
        json!({"rules": sqlx::query_as::<_,AutomationRule>(concat!("SELECT ", rule_columns!(), " FROM automation_rules WHERE tenant_id=$1 ORDER BY name")).bind(t).fetch_all(&s.db).await?}),
    ))
}

pub async fn create_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateRuleRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() {
        return Err(AppError::Validation("Name required".into()));
    }
    let valid_t = [
        "TagAdded",
        "TagRemoved",
        "StageChanged",
        "ScoreChanged",
        "ListAdded",
        "ListRemoved",
        "tag.assigned",
        "tag.unassigned",
        "stage.changed",
        // The three trigger kinds the six approved drafts need and the vocabulary was missing
        // (kanban t_990c386e). Each one has a real emitter: `contact.created` is dispatched from
        // the contact-creation doors, and `date.renewal` / `contact.inactive` are evaluated by the
        // clock scanner in src/automation/scan.rs. A label with no emitter is decorative, which is
        // why none of these is accepted "just in case".
        "contact.created",
        "ContactCreated",
        "date.renewal",
        "contact.inactive",
    ];
    if !valid_t.contains(&r.trigger_type.as_str()) {
        return Err(AppError::Validation("Invalid trigger_type".into()));
    }
    let valid_a = [
        "AddTag",
        "RemoveTag",
        "MovePipeline",
        "AddToList",
        "RemoveFromList",
        "Webhook",
        "NotifyUser",
        "send_email",
        "send_sms",
        "pipeline.move",
        "scoring.update",
    ];
    if !valid_a.contains(&r.action_type.as_str()) {
        return Err(AppError::Validation("Invalid action_type".into()));
    }
    // `trigger_type` / `action_type` are plain `character varying(50)` columns in this database —
    // the removed `::trigger_type` / `::action_type` casts pointed at enum types that do not
    // exist (pg_type has business_unit, channel_type, user_role, user_state — nothing else), which
    // is why this INSERT had never succeeded for anyone.
    //
    // `is_active` (kanban t_990c386e): COALESCE keeps the old contract — an omitted field is an
    // ACTIVE rule, exactly as before this change, because the column's own default is true. The
    // console's "Save as draft" sends false explicitly; nothing in this endpoint can activate a
    // rule that was created as a draft, and nothing here queues an action.
    // Free-text hygiene before the write (`security::free_text`, 2026-11-26): a NUL in the name or
    // the description reached the INSERT, and one inside either JSON config reached the jsonb cast —
    // both answered the same raw 500 and saved nothing.
    let name = crate::security::free_text::single_line(&r.name, "Rule name", 200)?;
    let description = crate::security::free_text::nul_free(r.description);
    let trigger_config = crate::security::free_text::strip_nul_json(&r.trigger_config);
    let action_config = crate::security::free_text::strip_nul_json(&r.action_config);
    let rule = sqlx::query_as::<_,AutomationRule>(concat!("INSERT INTO automation_rules(id,tenant_id,name,description,trigger_type,trigger_config,action_type,action_config,is_active) VALUES($1,$2,$3,$4,$5,$6,$7,$8,COALESCE($9,true)) RETURNING ", rule_columns!()))
        .bind(Uuid::new_v4()).bind(t).bind(&name).bind(&description).bind(&r.trigger_type).bind(&trigger_config).bind(&r.action_type).bind(&action_config).bind(r.is_active).fetch_one(&s.db).await?;
    // Belt and braces (kanban t_b54589a3): a brand-new rule has no queue rows, so this is a no-op
    // today — but the invariant "an inactive rule has nothing waiting" is enforced at every door
    // that can produce an inactive rule, not just at Pause.
    let cancelled = if rule.is_active {
        0
    } else {
        cancel_pending_for_rule(&s.db, t, rule.id).await?
    };
    Ok((
        StatusCode::CREATED,
        Json(rule_with_cancelled(&rule, cancelled)?),
    ))
}

pub async fn get_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_, AutomationRule>(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM automation_rules WHERE id=$1 AND tenant_id=$2"
    ))
    .bind(id)
    .bind(t)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound(format!("Rule {id} not found")))?)))
}

pub async fn update_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateRuleRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // `is_active` is the real column (nullable). COALESCE keeps the card's semantics:
    // a NULL request value means "leave as is"; the read path reports NULL as enabled.
    //
    // PAUSE (kanban t_b54589a3): when the rule ends up inactive, every step it still has waiting is
    // cancelled in the SAME transaction as the flip — the console's "it will not fire" sentence is
    // only true if the queue agrees. Rows are kept as the ledger (`result.reason` says why).
    // Same free-text hygiene as the create door, including both JSON configs.
    let name = crate::security::free_text::optional_line(r.name.as_deref(), "Rule name", 200)?;
    let description = crate::security::free_text::nul_free(r.description);
    let trigger_config = r
        .trigger_config
        .as_ref()
        .map(crate::security::free_text::strip_nul_json);
    let action_config = r
        .action_config
        .as_ref()
        .map(crate::security::free_text::strip_nul_json);
    let mut tx = s.db.begin().await?;
    let rule = sqlx::query_as::<_,AutomationRule>(concat!("UPDATE automation_rules SET name=COALESCE($1,name), description=COALESCE($2,description), trigger_type=COALESCE($3,trigger_type), trigger_config=COALESCE($4,trigger_config), action_type=COALESCE($5,action_type), action_config=COALESCE($6,action_config), is_active=COALESCE($7,is_active), updated_at=NOW() WHERE id=$8 AND tenant_id=$9 RETURNING ", rule_columns!()))
        .bind(&name).bind(&description).bind(&r.trigger_type).bind(&trigger_config).bind(&r.action_type).bind(&action_config).bind(r.is_active).bind(id).bind(t).fetch_optional(&mut *tx).await?.ok_or(AppError::NotFound(format!("Rule {id} not found")))?;
    let cancelled = if rule.is_active {
        0
    } else {
        cancel_pending_for_rule(&mut *tx, t, id).await?
    };
    tx.commit().await?;
    Ok(Json(rule_with_cancelled(&rule, cancelled)?))
}

pub async fn delete_rule(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Cancel before the DELETE: `delayed_actions.source_rule_id` is `ON DELETE SET NULL`, so a row
    // left waiting when its rule is removed would come back as a LEGACY row and fire its stored
    // action with no rule behind it (kanban t_b54589a3). Removing a rule must stop its waits too.
    let mut tx = s.db.begin().await?;
    let found: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM automation_rules WHERE id=$1 AND tenant_id=$2")
            .bind(id)
            .bind(t)
            .fetch_optional(&mut *tx)
            .await?;
    if found.is_none() {
        return Err(AppError::NotFound(format!("Rule {id} not found")));
    }
    let cancelled = cancel_pending_for_rule(&mut *tx, t, id).await?;
    sqlx::query("DELETE FROM automation_rules WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(
        json!({"message":"Deleted","cancelled_pending":cancelled}),
    ))
}
