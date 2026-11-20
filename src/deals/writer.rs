//! The ONE deal writer.
//!
//! Both doors into the deal record — the console (`/api/deals`) and an outside system
//! (`POST /api/external/deals`) — call [`upsert`]. One insert path is what makes
//! "one external reference = one deal" true rather than hoped for: the second door cannot
//! invent its own dedupe rule because it has no SQL of its own.
//!
//! ## The dedupe rule (David's acceptance, 2026-11-20)
//! `(tenant_id, external_ref)` is unique (partial index `uq_opportunities_tenant_external_ref`).
//! A push carrying an `external_ref` the workspace already holds UPDATES that deal and answers
//! `created: false`. A push with an unknown ref INSERTs and answers `created: true`. The index
//! makes the rule hold even for two pushes that race: the loser's insert is refused by Postgres,
//! never by application timing.
//!
//! ## What a push may not do
//! `tenant_id` in the body is ignored: the tenant comes from the credential (the API key's hash,
//! or the session's `aid`). Every referenced row (pipeline, stage, contact, company, owner) is
//! re-checked against the caller's tenant, and a foreign id answers 404 rather than 403 so the
//! refusal never confirms another workspace's row exists.

use chrono::{DateTime, Utc};
use serde_json::{json, Map, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::models::{DealUpsert, UpsertOutcome};
use crate::errors::AppError;
use crate::pipelines::opportunity::{opp_cols, OpportunityFull};
use crate::AppState;

/// Who made this write, for the timeline. `label` is what a human reads in the deal's activity
/// list ("xride-dispatch (API key q7f2)", "Ada (console)", "system").
pub struct Actor<'a> {
    pub user_id: Option<Uuid>,
    pub label: &'a str,
}

impl<'a> Actor<'a> {
    pub fn system(label: &'a str) -> Self {
        Self {
            user_id: None,
            label,
        }
    }
}

fn blank(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

// ── public helpers for the console handlers (they reuse ONE writer, not a second one) ────────

pub fn blank_pub(s: &Option<String>) -> Option<String> {
    blank(s)
}

pub fn status_flags_pub(status: Option<&str>) -> Result<Option<(bool, bool)>, AppError> {
    status_flags(status)
}

pub fn normalize_field_key_pub(raw: &str) -> String {
    normalize_field_key(raw)
}

pub async fn find_or_create_contact(
    state: &AppState,
    tenant: Uuid,
    email: &str,
    name: Option<&str>,
    phone: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    resolve_contact(state, tenant, None, Some(email), name, phone).await
}

pub async fn write_custom_fields_pub(
    state: &AppState,
    tenant: Uuid,
    deal_id: Uuid,
    fields: &Map<String, Value>,
) -> Result<Vec<String>, AppError> {
    write_custom_fields(state, tenant, deal_id, fields, "console").await
}

pub async fn set_collaborators_pub(
    state: &AppState,
    tenant: Uuid,
    deal_id: Uuid,
    ids: &[Uuid],
) -> Result<usize, AppError> {
    set_collaborators(state, tenant, deal_id, ids).await
}

/// The workspace's own currency, as the workspace stated it (`tenants.settings.default_currency`).
///
/// There is deliberately NO fallback to `USD`: an amount with no stated currency is not a dollar
/// amount, and guessing is how a 1,200 EUR deal becomes a 1,200 USD report line (David,
/// 2026-11-20). A workspace that has not chosen one is asked to choose.
pub async fn workspace_default_currency(
    db: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<String>, AppError> {
    let settings: Option<Value> = sqlx::query_scalar("SELECT settings FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(db)
        .await?;
    Ok(settings.and_then(|s| {
        ["default_currency", "currency"]
            .iter()
            .find_map(|k| s.get(k).and_then(|v| v.as_str()).map(str::to_string))
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty())
    }))
}

/// Resolve the deal's currency: what the caller said → what the deal already held → what the
/// workspace chose. Never a guess. `None` is refused by the caller with a sentence naming both
/// remedies.
fn normalize_currency(raw: Option<&str>) -> Result<Option<String>, AppError> {
    match raw.map(str::trim).filter(|c| !c.is_empty()) {
        None => Ok(None),
        Some(c) => {
            let up = c.to_uppercase();
            if up.len() != 3 || !up.chars().all(|ch| ch.is_ascii_alphabetic()) {
                return Err(AppError::Validation(format!(
                    "currency must be a 3-letter code — got \"{c}\""
                )));
            }
            Ok(Some(up))
        }
    }
}

/// Append one line to the deal's activity timeline and stamp the deal as touched.
pub async fn touch(
    db: &PgPool,
    tenant_id: Uuid,
    deal_id: Uuid,
    kind: &str,
    body: Option<&str>,
    actor: &Actor<'_>,
    metadata: Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO deal_activities (tenant_id, opportunity_id, kind, body, actor_id, actor_label, metadata)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(tenant_id)
    .bind(deal_id)
    .bind(kind)
    .bind(body)
    .bind(actor.user_id)
    .bind(actor.label)
    .bind(metadata)
    .execute(db)
    .await?;
    sqlx::query(
        "UPDATE opportunities SET last_activity_at = NOW() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(deal_id)
    .bind(tenant_id)
    .execute(db)
    .await?;
    Ok(())
}

/// Find (or create) the workspace's default pipeline and make sure it has stages.
///
/// A brand-new workspace must be able to accept its first API push without a human pressing
/// "New Pipeline" first — otherwise the outside system's very first quote fails, which is the
/// worst possible moment to teach somebody about pipelines.
async fn ensure_pipeline(
    state: &AppState,
    tenant_id: Uuid,
    wanted_id: Option<Uuid>,
    wanted_name: Option<&str>,
) -> Result<(Uuid, bool), AppError> {
    if let Some(pid) = wanted_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pipelines WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(pid)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("Pipeline {pid} not found")));
        }
        ensure_default_stages(state, tenant_id, pid).await?;
        return Ok((pid, false));
    }

    if let Some(name) = wanted_name {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM pipelines WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
        )
        .bind(tenant_id)
        .bind(name)
        .fetch_optional(&state.db)
        .await?;
        if let Some(pid) = found {
            ensure_default_stages(state, tenant_id, pid).await?;
            return Ok((pid, false));
        }
        let pid = create_pipeline(state, tenant_id, name).await?;
        return Ok((pid, true));
    }

    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM pipelines WHERE tenant_id = $1 AND is_active
          ORDER BY is_default DESC, created_at ASC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?;
    match existing {
        Some(pid) => {
            ensure_default_stages(state, tenant_id, pid).await?;
            Ok((pid, false))
        }
        None => {
            let pid = create_pipeline(state, tenant_id, "Sales Pipeline").await?;
            Ok((pid, true))
        }
    }
}

async fn create_pipeline(state: &AppState, tenant_id: Uuid, name: &str) -> Result<Uuid, AppError> {
    let pid = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO pipelines (id, tenant_id, name, description, is_default, is_active, created_at, updated_at)
         VALUES ($1,$2,$3,'Created automatically so deals have somewhere to live.',true,true,NOW(),NOW())",
    )
    .bind(pid)
    .bind(tenant_id)
    .bind(name)
    .execute(&state.db)
    .await?;
    ensure_default_stages(state, tenant_id, pid).await?;
    Ok(pid)
}

/// Every pipeline in every industry needs the same four open stages plus a closed pair; the
/// workspace renames them afterwards. Inserted only when the pipeline has NONE, so a pipeline a
/// human already shaped is never touched.
async fn ensure_default_stages(
    state: &AppState,
    tenant_id: Uuid,
    pipeline_id: Uuid,
) -> Result<(), AppError> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipeline_stages WHERE pipeline_id = $1")
        .bind(pipeline_id)
        .fetch_one(&state.db)
        .await?;
    if n > 0 {
        return Ok(());
    }
    for (name, position, probability, won, lost) in [
        ("Lead", 0, 10, false, false),
        ("Qualified", 1, 30, false, false),
        ("Proposal", 2, 60, false, false),
        ("Negotiation", 3, 80, false, false),
        ("Won", 4, 100, true, false),
        ("Lost", 5, 0, false, true),
    ] {
        sqlx::query(
            "INSERT INTO pipeline_stages (id, pipeline_id, name, color, position, is_won, is_lost, probability, created_at, updated_at)
             VALUES ($1,$2,$3,'#6366f1',$4,$5,$6,$7,NOW(),NOW())",
        )
        .bind(Uuid::new_v4())
        .bind(pipeline_id)
        .bind(name)
        .bind(position)
        .bind(won)
        .bind(lost)
        .bind(probability)
        .execute(&state.db)
        .await?;
    }
    let _ = tenant_id;
    Ok(())
}

async fn resolve_stage(
    state: &AppState,
    pipeline_id: Uuid,
    stage_id: Option<Uuid>,
    stage_name: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(sid) = stage_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pipeline_stages WHERE id = $1 AND pipeline_id = $2)",
        )
        .bind(sid)
        .bind(pipeline_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::Validation(format!(
                "stage_id {sid} does not belong to pipeline {pipeline_id}"
            )));
        }
        return Ok(Some(sid));
    }
    if let Some(name) = stage_name {
        let found: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND lower(name) = lower($2)
              ORDER BY position LIMIT 1",
        )
        .bind(pipeline_id)
        .bind(name)
        .fetch_optional(&state.db)
        .await?;
        return match found {
            Some(sid) => Ok(Some(sid)),
            None => Err(AppError::Validation(format!(
                "stage \"{name}\" does not exist on this pipeline"
            ))),
        };
    }
    Ok(None)
}

async fn first_stage(db: &PgPool, pipeline_id: Uuid) -> Result<Uuid, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY position, name LIMIT 1",
    )
    .bind(pipeline_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::BadRequest("this pipeline has no stages".to_string()))
}

/// Find-or-create a contact by email. Email is the only field on `contacts` with a uniqueness
/// guard (`(tenant_id, email)` where email is not null), so it is the only safe dedupe key —
/// matching on a name would mint a duplicate per deal.
async fn resolve_contact(
    state: &AppState,
    tenant_id: Uuid,
    contact_id: Option<Uuid>,
    email: Option<&str>,
    name: Option<&str>,
    phone: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(id) = contact_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM contacts WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(id)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("Contact {id} not found")));
        }
        return Ok(Some(id));
    }
    let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) else {
        return Ok(None);
    };
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND lower(email) = lower($2) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(email)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(Some(id));
    }
    let (first, last) = match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => match n.split_once(' ') {
            Some((f, l)) => (f.to_string(), l.to_string()),
            None => (n.to_string(), String::new()),
        },
        None => (email.to_string(), String::new()),
    };
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, source, created_at, updated_at)
         VALUES ($1,$2,$3,$4,$5,$6,'deal',NOW(),NOW())
         ON CONFLICT (tenant_id, email) WHERE email IS NOT NULL DO NOTHING",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&first)
    .bind(&last)
    .bind(email)
    .bind(phone)
    .execute(&state.db)
    .await?;
    Ok(Some(
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM contacts WHERE tenant_id = $1 AND lower(email) = lower($2) LIMIT 1",
        )
        .bind(tenant_id)
        .bind(email)
        .fetch_one(&state.db)
        .await?,
    ))
}

async fn resolve_company(
    state: &AppState,
    tenant_id: Uuid,
    company_id: Option<Uuid>,
    company_name: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(id) = company_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM companies WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(id)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("Company {id} not found")));
        }
        return Ok(Some(id));
    }
    let Some(name) = company_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM companies WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(name)
    .fetch_optional(&state.db)
    .await?
    {
        return Ok(Some(id));
    }
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO companies (id, tenant_id, name, created_at, updated_at) VALUES ($1,$2,$3,NOW(),NOW())",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(name)
    .execute(&state.db)
    .await?;
    Ok(Some(id))
}

async fn resolve_owner(
    state: &AppState,
    tenant_id: Uuid,
    owner_id: Option<Uuid>,
    owner_email: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(id) = owner_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(id)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("User {id} not found")));
        }
        return Ok(Some(id));
    }
    if let Some(email) = owner_email.map(str::trim).filter(|e| !e.is_empty()) {
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE tenant_id = $1 AND lower(email) = lower($2) LIMIT 1",
        )
        .bind(tenant_id)
        .bind(email)
        .fetch_optional(&state.db)
        .await?;
        return match id {
            Some(id) => Ok(Some(id)),
            None => Err(AppError::Validation(format!(
                "owner_email \"{email}\" is not a user in this workspace"
            ))),
        };
    }
    Ok(None)
}

fn normalize_field_key(raw: &str) -> String {
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if (ch == ' ' || ch == '-' || ch == '_') && !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "field".to_string()
    } else {
        trimmed
    }
}

fn infer_type(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::String(s) => {
            if s.len() == 10
                && s.as_bytes().get(4) == Some(&b'-')
                && s.as_bytes().get(7) == Some(&b'-')
            {
                "date"
            } else if s.starts_with("http://") || s.starts_with("https://") {
                "url"
            } else {
                "text"
            }
        }
        _ => "text",
    }
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Write the caller's custom data points, auto-provisioning a definition the first time a key is
/// seen. This is how the industry layer arrives: Xride Dispatch sends `service_level`,
/// `pickup_city`, … and the workspace gets those fields without anyone opening an admin screen.
async fn write_custom_fields(
    state: &AppState,
    tenant_id: Uuid,
    deal_id: Uuid,
    fields: &Map<String, Value>,
    source_app: &str,
) -> Result<Vec<String>, AppError> {
    let mut written = Vec::new();
    for (raw_key, val) in fields.iter() {
        let key = normalize_field_key(raw_key);
        let label = raw_key.trim().to_string();
        let field_type = infer_type(val);
        let field_id: Uuid = sqlx::query_scalar(
            "INSERT INTO deal_custom_fields (tenant_id, key, label, field_type, source_app)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (tenant_id, key) DO UPDATE SET updated_at = NOW()
             RETURNING id",
        )
        .bind(tenant_id)
        .bind(&key)
        .bind(&label)
        .bind(field_type)
        .bind(source_app)
        .fetch_one(&state.db)
        .await?;
        let Some(text) = scalar_to_string(val) else {
            sqlx::query(
                "DELETE FROM deal_field_values WHERE opportunity_id = $1 AND field_id = $2",
            )
            .bind(deal_id)
            .bind(field_id)
            .execute(&state.db)
            .await?;
            written.push(format!("{key} cleared"));
            continue;
        };
        sqlx::query(
            "INSERT INTO deal_field_values (opportunity_id, field_id, value)
             VALUES ($1,$2,$3)
             ON CONFLICT (opportunity_id, field_id)
             DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
        )
        .bind(deal_id)
        .bind(field_id)
        .bind(&text)
        .execute(&state.db)
        .await?;
        written.push(format!("{key}={text}"));
    }
    Ok(written)
}

async fn set_collaborators(
    state: &AppState,
    tenant_id: Uuid,
    deal_id: Uuid,
    user_ids: &[Uuid],
) -> Result<usize, AppError> {
    for uid in user_ids {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(uid)
        .bind(tenant_id)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("User {uid} not found")));
        }
    }
    let keep: Vec<Uuid> = user_ids.to_vec();
    sqlx::query(
        "DELETE FROM deal_collaborators WHERE opportunity_id = $1 AND tenant_id = $2
           AND NOT (user_id = ANY($3))",
    )
    .bind(deal_id)
    .bind(tenant_id)
    .bind(&keep)
    .execute(&state.db)
    .await?;
    for uid in &keep {
        sqlx::query(
            "INSERT INTO deal_collaborators (tenant_id, opportunity_id, user_id)
             VALUES ($1,$2,$3) ON CONFLICT (opportunity_id, user_id) DO NOTHING",
        )
        .bind(tenant_id)
        .bind(deal_id)
        .bind(uid)
        .execute(&state.db)
        .await?;
    }
    Ok(keep.len())
}

/// Move a deal to a stage, keeping the flags, the probability default, the stage clock, the
/// history row, the timeline line and the stage hooks in agreement.
pub async fn move_stage(
    state: &AppState,
    tenant_id: Uuid,
    deal_id: Uuid,
    stage_id: Uuid,
    actor: &Actor<'_>,
) -> Result<(OpportunityFull, bool), AppError> {
    let deal: OpportunityFull = sqlx::query_as(&*concat!(
        "SELECT ",
        opp_cols!(),
        " FROM opportunities WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(deal_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Deal {deal_id} not found")))?;

    let stage: crate::pipelines::models::PipelineStage =
        sqlx::query_as("SELECT * FROM pipeline_stages WHERE id = $1 AND pipeline_id = $2")
            .bind(stage_id)
            .bind(deal.pipeline_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound(format!(
                "Stage {stage_id} not found on this deal's pipeline"
            )))?;

    if stage.id == deal.stage_id {
        return Ok((deal, false));
    }

    let probability = stage.probability.unwrap_or(deal.probability.unwrap_or(0));
    let moved: OpportunityFull = sqlx::query_as(&*concat!(
        "UPDATE opportunities
            SET stage_id = $3, is_won = $4, is_lost = $5, probability = $6,
                stage_entered_at = NOW(), updated_at = NOW(), last_activity_at = NOW()
          WHERE id = $1 AND tenant_id = $2 RETURNING ",
        opp_cols!()
    ))
    .bind(deal_id)
    .bind(tenant_id)
    .bind(stage.id)
    .bind(stage.is_won)
    .bind(stage.is_lost)
    .bind(probability)
    .fetch_one(&state.db)
    .await?;

    sqlx::query(
        "INSERT INTO opportunity_stage_history (id, opportunity_id, from_stage_id, to_stage_id, moved_by)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::new_v4())
    .bind(deal_id)
    .bind(deal.stage_id)
    .bind(stage.id)
    .bind(actor.user_id)
    .execute(&state.db)
    .await?;

    let move_body = format!("Moved to {}", stage.name);
    touch(
        &state.db,
        tenant_id,
        deal_id,
        "stage",
        Some(move_body.as_str()),
        actor,
        json!({"from_stage_id": deal.stage_id, "to_stage_id": stage.id}),
    )
    .await?;

    fire_stage_hooks(state, tenant_id, deal_id, stage.id, actor).await;
    Ok((moved, true))
}

/// Run whatever the workspace configured for entering this stage, then the automation rules.
///
/// Two mechanisms, one trigger point:
///   * `deal_stage_automations` — the explicit per-stage hook a human sets in the deal tab
///     ('tag' applies a tag to the deal, 'workflow' opts the stage into the automation engine);
///   * the automation rules engine — `StageChanged` rules. The label has existed since the
///     engine shipped but NOTHING emitted it (measured 2026-11-20), so the rules were decorative.
///     They now fire when the stage carries a 'workflow' hook — the opt-in that keeps a stage
///     with no hooks from running somebody's old rules by surprise.
async fn fire_stage_hooks(
    state: &AppState,
    tenant_id: Uuid,
    deal_id: Uuid,
    stage_id: Uuid,
    actor: &Actor<'_>,
) {
    let hooks: Result<Vec<(Uuid, String, Option<Uuid>, Option<String>)>, sqlx::Error> =
        sqlx::query_as(
            "SELECT a.id, a.action_kind, a.tag_id, t.name
           FROM deal_stage_automations a
           LEFT JOIN tags t ON t.id = a.tag_id
          WHERE a.tenant_id = $1 AND a.stage_id = $2 AND a.is_active",
        )
        .bind(tenant_id)
        .bind(stage_id)
        .fetch_all(&state.db)
        .await;

    let Ok(hooks) = hooks else {
        tracing::warn!(deal = %deal_id, "stage automation lookup failed");
        return;
    };

    let mut wants_workflow = false;
    for (_id, kind, tag_id, tag_name) in hooks {
        match kind.as_str() {
            "tag" => {
                let Some(tag_id) = tag_id else { continue };
                match sqlx::query(
                    "INSERT INTO tag_assignments (tag_id, entity_type, entity_id, tenant_id)
                     VALUES ($1,'opportunity',$2,$3)
                     ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING",
                )
                .bind(tag_id)
                .bind(deal_id)
                .bind(tenant_id)
                .execute(&state.db)
                .await
                {
                    Ok(r) if r.rows_affected() > 0 => {
                        crate::automation::engine::fire_tag_trigger(
                            &state.db,
                            tenant_id,
                            "opportunity",
                            deal_id,
                            tag_id,
                            "TagAdded",
                        )
                        .await;
                        let _ = touch(
                            &state.db,
                            tenant_id,
                            deal_id,
                            "automation",
                            Some(&format!(
                                "Stage hook applied tag {}",
                                tag_name.unwrap_or_default()
                            )),
                            &Actor::system("stage hook"),
                            json!({ "action": "tag", "tag_id": tag_id }),
                        )
                        .await;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(tag = %tag_id, error = %e, "stage hook tag failed"),
                }
            }
            "workflow" => wants_workflow = true,
            _ => {}
        }
    }

    if wants_workflow {
        let _ = touch(
            &state.db,
            tenant_id,
            deal_id,
            "automation",
            Some("Stage hook ran the workspace's StageChanged workflow"),
            &Actor::system("stage hook"),
            json!({ "action": "workflow" }),
        )
        .await;
        crate::automation::engine::fire_stage_trigger(
            &state.db,
            tenant_id,
            deal_id,
            stage_id,
            actor.user_id,
        )
        .await;
    }
}

fn status_flags(status: Option<&str>) -> Result<Option<(bool, bool)>, AppError> {
    match status
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("") => Ok(None),
        Some("open") => Ok(Some((false, false))),
        Some("won") => Ok(Some((true, false))),
        Some("lost") => Ok(Some((false, true))),
        Some(other) => Err(AppError::Validation(format!(
            "status must be one of open/won/lost — got \"{other}\""
        ))),
    }
}

/// The writer. Create-or-update by `external_ref`; plain create when no ref was sent.
pub async fn upsert(
    state: &AppState,
    tenant_id: Uuid,
    req: DealUpsert,
    actor: &Actor<'_>,
) -> Result<UpsertOutcome, AppError> {
    let external_ref = blank(&req.external_ref);
    if req.external_ref.is_some() && external_ref.is_none() {
        return Err(AppError::Validation(
            "external_ref must not be blank when sent".to_string(),
        ));
    }

    let existing: Option<OpportunityFull> = match &external_ref {
        Some(r) => {
            sqlx::query_as(&*concat!(
                "SELECT ",
                opp_cols!(),
                " FROM opportunities WHERE tenant_id = $1 AND external_ref = $2"
            ))
            .bind(tenant_id)
            .bind(r)
            .fetch_optional(&state.db)
            .await?
        }
        None => None,
    };

    let flags = status_flags(req.status.as_deref())?;

    // ── pipeline ────────────────────────────────────────────────────────────────────────────
    let wanted_pipeline_id = req.pipeline_id.or(existing.as_ref().map(|d| d.pipeline_id));
    let (pipeline_id, pipeline_created) = match (req.pipeline_id, req.pipeline.as_deref()) {
        (Some(pid), _) => ensure_pipeline(state, tenant_id, Some(pid), None).await?,
        (None, Some(name)) => ensure_pipeline(state, tenant_id, None, Some(name)).await?,
        (None, None) => ensure_pipeline(state, tenant_id, wanted_pipeline_id, None).await?,
    };

    // ── stage ───────────────────────────────────────────────────────────────────────────────
    let explicit_stage =
        resolve_stage(state, pipeline_id, req.stage_id, req.stage.as_deref()).await?;
    let stage_id = match explicit_stage {
        Some(sid) => sid,
        None => match &existing {
            Some(d) if d.pipeline_id == pipeline_id => d.stage_id,
            _ => first_stage(&state.db, pipeline_id).await?,
        },
    };
    let stage: crate::pipelines::models::PipelineStage =
        sqlx::query_as("SELECT * FROM pipeline_stages WHERE id = $1 AND pipeline_id = $2")
            .bind(stage_id)
            .bind(pipeline_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("this pipeline has no stages".to_string()))?;

    // ── linked rows ─────────────────────────────────────────────────────────────────────────
    let contact_id = resolve_contact(
        state,
        tenant_id,
        req.contact_id
            .or_else(|| existing.as_ref().and_then(|d| d.contact_id)),
        req.contact_email.as_deref(),
        req.contact_name.as_deref(),
        req.contact_phone.as_deref(),
    )
    .await?;
    let company_id = resolve_company(
        state,
        tenant_id,
        req.company_id
            .or_else(|| existing.as_ref().and_then(|d| d.company_id)),
        req.company_name.as_deref(),
    )
    .await?;
    let owner_id = resolve_owner(
        state,
        tenant_id,
        req.owner_id
            .or_else(|| existing.as_ref().and_then(|d| d.owner_id)),
        req.owner_email.as_deref(),
    )
    .await?;

    // Probability: explicit -> the stage's own default -> 0. A manual override survives an update
    // because on an update `probability` is COALESCEd and NULL means "not sent".
    let probability = req
        .probability
        .or(existing.as_ref().and_then(|d| d.probability))
        .or(stage.probability)
        .unwrap_or(0);

    let source_app = blank(&req.source).unwrap_or_else(|| "console".to_string());

    // ── currency (CORE, software-wide) ──────────────────────────────────────────────────────
    // Order: what the caller said -> what the deal already held -> what the workspace chose.
    // There is NO USD fallback: a workspace with no stated currency is refused, with the two
    // remedies named, because inventing a currency silently misprices every deal it touches.
    let workspace_currency = workspace_default_currency(&state.db, tenant_id).await?;
    let effective_currency = normalize_currency(req.currency.as_deref())?
        .or_else(|| {
            existing
                .as_ref()
                .and_then(|d| d.currency.clone())
                .map(|c| c.trim().to_uppercase())
                .filter(|c| !c.is_empty())
        })
        .or_else(|| workspace_currency.clone())
        .ok_or_else(|| {
            AppError::Validation(
                "this workspace has no default currency — send `currency` with the deal, or choose a \
                 workspace currency under Deals → Settings"
                    .to_string(),
            )
        })?;
    let currency_bind: Option<String> = if blank(&req.currency).is_some() {
        Some(effective_currency.clone())
    } else {
        // An update keeps the deal's own currency when the caller did not name one, even if the
        // workspace default has since changed.
        None
    };

    // The rate is captured WITH the deal: a same-currency amount has a rate of exactly 1; a
    // cross-currency amount stores the rate the caller supplied (or none — it is then reported in
    // its own currency and never folded into another).
    let (exchange_rate, rate_date) = match req.exchange_rate {
        Some(r) => {
            if !(r.is_finite()) || r <= 0.0 {
                return Err(AppError::Validation(
                    "exchange_rate must be a number greater than zero".to_string(),
                ));
            }
            (Some(r), Some(Utc::now()))
        }
        None => {
            let same_as_workspace = workspace_currency
                .as_ref()
                .map(|w| *w == effective_currency)
                .unwrap_or(false);
            if same_as_workspace {
                (Some(1.0), Some(Utc::now()))
            } else {
                (None, None)
            }
        }
    };

    // A 'won'/'lost' push with no stage named should land on the pipeline's closed stage, so the
    // board and the flags cannot disagree; if the pipeline has no such stage the flags alone move.
    let (target_stage_id, closed_stage) = match flags {
        Some((true, false)) => match sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND is_won ORDER BY position LIMIT 1",
        )
        .bind(pipeline_id)
        .fetch_optional(&state.db)
        .await?
        {
            Some(sid) => (sid, true),
            None => (stage_id, false),
        },
        Some((false, true)) => match sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND is_lost ORDER BY position LIMIT 1",
        )
        .bind(pipeline_id)
        .fetch_optional(&state.db)
        .await?
        {
            Some(sid) => (sid, true),
            None => (stage_id, false),
        },
        // "won AND lost" is not a state this app produces; a caller that sends both gets the
        // won reading, because a won deal is the stronger claim and the lost flag would need a
        // reason the caller did not supply.
        Some((true, true)) => match sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND is_won ORDER BY position LIMIT 1",
        )
        .bind(pipeline_id)
        .fetch_optional(&state.db)
        .await?
        {
            Some(sid) => (sid, true),
            None => (stage_id, false),
        },
        Some((false, false)) => (stage_id, false),
        None => (stage_id, false),
    };
    let stage_changed = existing
        .as_ref()
        .map(|d| d.stage_id != target_stage_id)
        .unwrap_or(false);
    let stage_entered_at: Option<DateTime<Utc>> = if stage_changed {
        Some(Utc::now())
    } else {
        None
    };

    // On an update, probability is written only when the caller overrode it OR the stage moved
    // (a moved deal takes the new stage's default). Otherwise it keeps whatever a human set.
    let update_probability: Option<i32> = match (req.probability, &existing) {
        (Some(p), _) => Some(p),
        (None, Some(d)) if d.stage_id != target_stage_id => Some(stage.probability.unwrap_or(0)),
        _ => None,
    };

    let name = blank(&req.name)
        .or_else(|| existing.as_ref().map(|d| d.name.clone()))
        .ok_or_else(|| {
            AppError::Validation(
                "title (or name) is required on the first push of a deal".to_string(),
            )
        })?;

    let (deal, created) = match &existing {
        None => {
            let id = Uuid::new_v4();
            let row: OpportunityFull = sqlx::query_as(&*concat!(
                "INSERT INTO opportunities
                   (id, tenant_id, pipeline_id, stage_id, contact_id, company_id, name, notes, value,
                    currency, probability, expected_close_date, source, metadata, is_won, is_lost,
                    lost_reason, external_ref, owner_id, exchange_rate, rate_date,
                    last_activity_at, stage_entered_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9::float8,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20::float8,$21,NOW(),NOW())
                 RETURNING ",
                opp_cols!()
            ))
            .bind(id)
            .bind(tenant_id)
            .bind(pipeline_id)
            .bind(target_stage_id)
            .bind(contact_id)
            .bind(company_id)
            .bind(&name)
            .bind(blank(&req.notes))
            .bind(req.value)
            .bind(&effective_currency)
            .bind(probability)
            .bind(req.expected_close_date)
            .bind(blank(&req.source))
            .bind(req.metadata.clone())
            // `is_won` / `is_lost` are NOT NULL with a DEFAULT false. Binding a NULL explicitly
            // OVERRIDES the default and the insert is refused (measured live 2026-11-20: a push
            // without a `status` answered 500 "null value in column is_won"), so the create arm
            // always states the flag.
            .bind(flags.map(|f| f.0).unwrap_or(false))
            .bind(flags.map(|f| f.1).unwrap_or(false))
            .bind(blank(&req.lost_reason))
            .bind(&external_ref)
            .bind(owner_id)
            .bind(exchange_rate)
            .bind(rate_date)
            .fetch_one(&state.db)
            .await?;

            sqlx::query(
                "INSERT INTO opportunity_stage_history (id, opportunity_id, to_stage_id, moved_by)
                 VALUES ($1,$2,$3,$4)",
            )
            .bind(Uuid::new_v4())
            .bind(id)
            .bind(target_stage_id)
            .bind(actor.user_id)
            .execute(&state.db)
            .await?;

            let created_body = match &external_ref {
                Some(r) => format!("Deal created from external reference {r}"),
                None => "Deal created".to_string(),
            };
            touch(
                &state.db,
                tenant_id,
                id,
                "created",
                Some(created_body.as_str()),
                actor,
                json!({ "pipeline_id": pipeline_id, "stage_id": target_stage_id }),
            )
            .await?;

            (row, true)
        }
        Some(existing) => {
            let row: OpportunityFull = sqlx::query_as(&*concat!(
                "UPDATE opportunities SET
                    name = COALESCE($3, name),
                    pipeline_id = $4,
                    stage_id = $5,
                    contact_id = COALESCE($6, contact_id),
                    company_id = COALESCE($7, company_id),
                    value = COALESCE($8::float8, value),
                    currency = COALESCE($9, currency),
                    probability = COALESCE($10, probability),
                    expected_close_date = COALESCE($11, expected_close_date),
                    source = COALESCE($12, source),
                    notes = COALESCE($13, notes),
                    metadata = COALESCE($14, metadata),
                    is_won = COALESCE($15, is_won),
                    is_lost = COALESCE($16, is_lost),
                    lost_reason = COALESCE($17, lost_reason),
                    owner_id = COALESCE($18, owner_id),
                    external_ref = COALESCE($19, external_ref),
                    stage_entered_at = COALESCE($20, stage_entered_at),
                    exchange_rate = COALESCE($21::float8, exchange_rate),
                    rate_date = COALESCE($22, rate_date),
                    updated_at = NOW(), last_activity_at = NOW()
                  WHERE id = $1 AND tenant_id = $2 RETURNING ",
                opp_cols!()
            ))
            .bind(existing.id)
            .bind(tenant_id)
            .bind(blank(&req.name))
            .bind(pipeline_id)
            .bind(target_stage_id)
            .bind(contact_id)
            .bind(company_id)
            .bind(req.value)
            .bind(currency_bind)
            .bind(update_probability)
            .bind(req.expected_close_date)
            .bind(blank(&req.source))
            .bind(blank(&req.notes))
            .bind(req.metadata.clone())
            .bind(flags.map(|f| f.0))
            .bind(flags.map(|f| f.1))
            .bind(blank(&req.lost_reason))
            .bind(owner_id)
            .bind(&external_ref)
            .bind(stage_entered_at)
            .bind(exchange_rate)
            .bind(rate_date)
            .fetch_one(&state.db)
            .await?;

            if existing.stage_id != target_stage_id {
                sqlx::query(
                    "INSERT INTO opportunity_stage_history (id, opportunity_id, from_stage_id, to_stage_id, moved_by)
                     VALUES ($1,$2,$3,$4,$5)",
                )
                .bind(Uuid::new_v4())
                .bind(existing.id)
                .bind(existing.stage_id)
                .bind(target_stage_id)
                .bind(actor.user_id)
                .execute(&state.db)
                .await?;
            }

            let mut changed: Vec<&str> = Vec::new();
            if blank(&req.name).is_some() {
                changed.push("title");
            }
            if req.value.is_some() {
                changed.push("value");
            }
            if req.expected_close_date.is_some() {
                changed.push("expected close date");
            }
            if blank(&req.notes).is_some() {
                changed.push("notes");
            }
            if req.contact_email.is_some() || req.contact_id.is_some() {
                changed.push("contact");
            }
            if req.owner_id.is_some() || req.owner_email.is_some() {
                changed.push("owner");
            }
            if existing.stage_id != target_stage_id {
                changed.push("stage");
            }
            if flags.is_some() {
                changed.push("status");
            }
            let body = if changed.is_empty() {
                "Pushed again with no field changes".to_string()
            } else {
                format!("Updated {}", changed.join(", "))
            };
            touch(
                &state.db,
                tenant_id,
                existing.id,
                "updated",
                Some(&body),
                actor,
                json!({ "external_ref": external_ref, "fields": changed }),
            )
            .await?;

            (row, false)
        }
    };

    if let Some(fields) = req.custom_fields.as_ref() {
        let written = write_custom_fields(state, tenant_id, deal.id, fields, &source_app).await?;
        if !written.is_empty() {
            touch(
                &state.db,
                tenant_id,
                deal.id,
                "updated",
                Some(&format!("Custom fields: {}", written.join(", "))),
                actor,
                json!({ "custom_fields": written }),
            )
            .await?;
        }
    }

    if let Some(ids) = req.collaborator_ids.as_ref() {
        let n = set_collaborators(state, tenant_id, deal.id, ids).await?;
        touch(
            &state.db,
            tenant_id,
            deal.id,
            "updated",
            Some(&format!("Collaborators set to {n}")),
            actor,
            json!({ "collaborators": n }),
        )
        .await?;
    }

    if created && closed_stage {
        // A push that lands straight on a closed stage still runs that stage's hooks/clock.
        fire_stage_hooks(state, tenant_id, deal.id, target_stage_id, actor).await;
    } else if stage_changed {
        fire_stage_hooks(state, tenant_id, deal.id, target_stage_id, actor).await;
    }

    Ok(UpsertOutcome {
        deal,
        created,
        matched_by: if external_ref.is_some() {
            "external_ref"
        } else {
            "insert"
        },
        stage_moved: stage_changed,
        pipeline_created,
    })
}

/// Status transition (won/lost/open) through the same writer, so the flags, the stage, the
/// history, the timeline and the hooks stay in one place.
pub async fn set_status(
    state: &AppState,
    tenant_id: Uuid,
    deal_id: Uuid,
    status: &str,
    lost_reason: Option<&str>,
    actor: &Actor<'_>,
) -> Result<OpportunityFull, AppError> {
    let deal: OpportunityFull = sqlx::query_as(&*concat!(
        "SELECT ",
        opp_cols!(),
        " FROM opportunities WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(deal_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Deal {deal_id} not found")))?;

    let (is_won, is_lost) = status_flags(Some(status))?
        .ok_or_else(|| AppError::Validation("status must be one of open/won/lost".to_string()))?;

    let closed_stage: Option<Uuid> = if is_won {
        sqlx::query_scalar(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND is_won ORDER BY position LIMIT 1",
        )
        .bind(deal.pipeline_id)
        .fetch_optional(&state.db)
        .await?
    } else if is_lost {
        sqlx::query_scalar(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND is_lost ORDER BY position LIMIT 1",
        )
        .bind(deal.pipeline_id)
        .fetch_optional(&state.db)
        .await?
    } else {
        None
    };

    let updated: OpportunityFull = sqlx::query_as(&*concat!(
        "UPDATE opportunities SET is_won = $3, is_lost = $4,
                lost_reason = CASE WHEN $4 THEN COALESCE($5, lost_reason) ELSE NULL END,
                updated_at = NOW(), last_activity_at = NOW()
          WHERE id = $1 AND tenant_id = $2 RETURNING ",
        opp_cols!()
    ))
    .bind(deal_id)
    .bind(tenant_id)
    .bind(is_won)
    .bind(is_lost)
    .bind(lost_reason)
    .fetch_one(&state.db)
    .await?;

    touch(
        &state.db,
        tenant_id,
        deal_id,
        "status",
        Some(&match (is_won, is_lost) {
            (true, _) => "Marked won".to_string(),
            (_, true) => match lost_reason {
                Some(r) if !r.trim().is_empty() => format!("Marked lost — {r}"),
                _ => "Marked lost".to_string(),
            },
            _ => "Reopened".to_string(),
        }),
        actor,
        json!({"is_won": is_won, "is_lost": is_lost, "lost_reason": lost_reason}),
    )
    .await?;

    if let Some(sid) = closed_stage {
        let (row, moved) = move_stage(state, tenant_id, deal_id, sid, actor).await?;
        return Ok(if moved { row } else { updated });
    }
    Ok(updated)
}
