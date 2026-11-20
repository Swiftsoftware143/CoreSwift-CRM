//! Deal Writer — console routes (`/api/deals`).
//!
//! Every route here is authenticated by the session JWT and scoped to `claims.aid`. The SQL is
//! written literally (gate rule 5d) and every statement carries `tenant_id` in its WHERE clause.

use axum::{
    extract::{Extension, Path, Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use base64::Engine as _;
use serde_json::{json, Value};
use uuid::Uuid;

use super::fx;
use super::models::*;
use super::writer::{self, Actor};
use crate::audit;
use crate::auth::models::Claims;
use crate::errors::{validate_pagination, ApiResult, AppError};
use crate::pipelines::opportunity::{opp_cols, OpportunityFull};
use crate::AppState;

/// Decoded attachment ceiling. The router raises axum's body limit to fit the base64 envelope
/// (4/3 of this) plus JSON overhead.
const MAX_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;

fn tenant_of(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

fn user_of(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)
}

/// The signed-in person's name, for the deal timeline. `Claims` carries only the user id, so the
/// name is read from the row the token points at — a timeline that says "Console user" for every
/// entry is a timeline nobody can use.
async fn label_for(state: &AppState, claims: &Claims) -> (Option<Uuid>, String) {
    let uid = Uuid::parse_str(&claims.sub).ok();
    let name: Option<String> = match uid {
        Some(id) => sqlx::query_scalar("SELECT name FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.db)
            .await
            .ok()
            .flatten(),
        None => None,
    };
    (uid, name.unwrap_or_else(|| "console user".to_string()))
}

async fn load_deal(state: &AppState, tenant: Uuid, id: Uuid) -> Result<OpportunityFull, AppError> {
    let sql = concat!(
        "SELECT ",
        opp_cols!(),
        " FROM opportunities WHERE id = $1 AND tenant_id = $2"
    );
    sqlx::query_as::<_, OpportunityFull>(sql)
        .bind(id)
        .bind(tenant)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Deal {id} not found")))
}

// ── list ────────────────────────────────────────────────────────────────────────────────────

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<DealListParams>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;
    let status = params.status.clone();
    if let Some(s) = status.as_deref() {
        if !matches!(s, "won" | "lost" | "open") {
            return Err(AppError::Validation(
                "status must be one of won/lost/open".to_string(),
            ));
        }
    }
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| format!("%{s}%"));

    let sql = concat!(
        "SELECT ",
        opp_cols!(),
        " FROM opportunities o
          WHERE o.tenant_id = $1
            AND ($2::uuid IS NULL OR o.pipeline_id = $2)
            AND ($3::uuid IS NULL OR o.stage_id = $3)
            AND ($4::uuid IS NULL OR o.owner_id = $4)
            AND ($5::text IS NULL OR o.source = $5)
            AND ($6::text IS NULL
                 OR ($6 = 'won' AND o.is_won)
                 OR ($6 = 'lost' AND o.is_lost)
                 OR ($6 = 'open' AND NOT o.is_won AND NOT o.is_lost))
            AND ($7::date IS NULL OR o.created_at >= $7)
            AND ($8::date IS NULL OR o.created_at < ($8::date + 1))
            AND ($9::text IS NULL
                 OR o.name ILIKE $9
                 OR COALESCE(o.notes,'') ILIKE $9
                 OR COALESCE(o.source,'') ILIKE $9
                 OR COALESCE(o.external_ref,'') ILIKE $9)
            AND (NOT $10::bool OR o.last_activity_at < NOW() - ($11::int * INTERVAL '1 day'))
          ORDER BY o.last_activity_at DESC, o.created_at DESC
          LIMIT $12 OFFSET $13"
    );
    let rot_days = params.rot_days.unwrap_or(14).clamp(1, 365);
    let deals = sqlx::query_as::<_, OpportunityFull>(sql)
        .bind(tenant)
        .bind(params.pipeline_id)
        .bind(params.stage_id)
        .bind(params.owner_id)
        .bind(params.source.as_deref())
        .bind(status.as_deref())
        .bind(params.from)
        .bind(params.to)
        .bind(q.as_deref())
        .bind(params.rotting.unwrap_or(false))
        .bind(rot_days as i32)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?;

    // Totals for the SAME filter, not for the whole workspace: a stage total under a filter that
    // ignored the filter is a lie the eye cannot catch.
    //
    // And money is NEVER added across currencies (David, 2026-11-20): the raw SUM is returned
    // per currency, and the single number the response offers is the REPORTING-currency
    // equivalent computed from each deal's STORED rate — never a mixed sum that reads as one
    // figure. `total_value` stays null when the result holds more than one currency, because a
    // scalar there would be exactly the meaningless number this rule forbids.
    let by_currency: Vec<(String, i64, f64, f64)> = sqlx::query_as(
        "SELECT COALESCE(currency,'') AS currency, COUNT(*) AS deal_count,
                COALESCE(SUM(value),0)::float8 AS total_value,
                COALESCE(SUM(value * COALESCE(probability,0) / 100.0),0)::float8 AS weighted_value
           FROM opportunities o
          WHERE o.tenant_id = $1
            AND ($2::uuid IS NULL OR o.pipeline_id = $2)
            AND ($3::uuid IS NULL OR o.stage_id = $3)
            AND ($4::uuid IS NULL OR o.owner_id = $4)
            AND ($5::text IS NULL OR o.source = $5)
            AND ($6::text IS NULL
                 OR ($6 = 'won' AND o.is_won)
                 OR ($6 = 'lost' AND o.is_lost)
                 OR ($6 = 'open' AND NOT o.is_won AND NOT o.is_lost))
            AND ($7::date IS NULL OR o.created_at >= $7)
            AND ($8::date IS NULL OR o.created_at < ($8::date + 1))
            AND ($9::text IS NULL
                 OR o.name ILIKE $9
                 OR COALESCE(o.notes,'') ILIKE $9
                 OR COALESCE(o.source,'') ILIKE $9
                 OR COALESCE(o.external_ref,'') ILIKE $9)
            AND (NOT $10::bool OR o.last_activity_at < NOW() - ($11::int * INTERVAL '1 day'))
          GROUP BY COALESCE(currency,'')
          ORDER BY 1",
    )
    .bind(tenant)
    .bind(params.pipeline_id)
    .bind(params.stage_id)
    .bind(params.owner_id)
    .bind(params.source.as_deref())
    .bind(status.as_deref())
    .bind(params.from)
    .bind(params.to)
    .bind(q.as_deref())
    .bind(params.rotting.unwrap_or(false))
    .bind(rot_days as i32)
    .fetch_all(&state.db)
    .await?;

    let reporting_currency = super::fx::workspace_reporting_currency(&state.db, tenant).await?;
    let mut reporting_total = 0.0f64;
    let mut reporting_weighted = 0.0f64;
    let mut unconverted = 0i64;
    if let Some(rc) = reporting_currency.as_deref() {
        let row: (f64, f64, i64) = sqlx::query_as(
            "SELECT
                COALESCE(SUM(CASE
                    WHEN upper(COALESCE(currency,'')) = upper($12) THEN value
                    WHEN exchange_rate IS NOT NULL
                         AND upper(COALESCE(reporting_currency, currency)) = upper($12)
                        THEN value * exchange_rate
                 END),0)::float8 AS reporting_value,
                COALESCE(SUM((CASE
                    WHEN upper(COALESCE(currency,'')) = upper($12) THEN value
                    WHEN exchange_rate IS NOT NULL
                         AND upper(COALESCE(reporting_currency, currency)) = upper($12)
                        THEN value * exchange_rate
                 END) * COALESCE(probability,0) / 100.0),0)::float8 AS reporting_weighted,
                COUNT(*) FILTER (WHERE
                    upper(COALESCE(currency,'')) <> upper($12)
                    AND (exchange_rate IS NULL
                         OR upper(COALESCE(reporting_currency, currency)) <> upper($12))
                ) AS unconverted
               FROM opportunities o
              WHERE o.tenant_id = $1
                AND ($2::uuid IS NULL OR o.pipeline_id = $2)
                AND ($3::uuid IS NULL OR o.stage_id = $3)
                AND ($4::uuid IS NULL OR o.owner_id = $4)
                AND ($5::text IS NULL OR o.source = $5)
                AND ($6::text IS NULL
                     OR ($6 = 'won' AND o.is_won)
                     OR ($6 = 'lost' AND o.is_lost)
                     OR ($6 = 'open' AND NOT o.is_won AND NOT o.is_lost))
                AND ($7::date IS NULL OR o.created_at >= $7)
                AND ($8::date IS NULL OR o.created_at < ($8::date + 1))
                AND ($9::text IS NULL
                     OR o.name ILIKE $9
                     OR COALESCE(o.notes,'') ILIKE $9
                     OR COALESCE(o.source,'') ILIKE $9
                     OR COALESCE(o.external_ref,'') ILIKE $9)
                AND (NOT $10::bool OR o.last_activity_at < NOW() - ($11::int * INTERVAL '1 day'))",
        )
        .bind(tenant)
        .bind(params.pipeline_id)
        .bind(params.stage_id)
        .bind(params.owner_id)
        .bind(params.source.as_deref())
        .bind(status.as_deref())
        .bind(params.from)
        .bind(params.to)
        .bind(q.as_deref())
        .bind(params.rotting.unwrap_or(false))
        .bind(rot_days as i32)
        .bind(rc)
        .fetch_one(&state.db)
        .await?;
        reporting_total = row.0;
        reporting_weighted = row.1;
        unconverted = row.2;
    }

    let deal_count: i64 = by_currency.iter().map(|r| r.1).sum();
    let single_currency = by_currency.len() == 1;
    let totals_by_currency: Vec<serde_json::Value> = by_currency
        .iter()
        .map(|(c, n, total, weighted)| {
            json!({ "currency": c, "deal_count": n, "total_value": total, "weighted_value": weighted })
        })
        .collect();

    Ok(Json(json!({
        "deals": deals,
        "page": page,
        "per_page": per_page,
        "total": deal_count,
        "total_value": if single_currency { Some(by_currency[0].2) } else { None },
        "weighted_value": if single_currency { Some(by_currency[0].3) } else { None },
        "totals_by_currency": totals_by_currency,
        "reporting": {
            "currency": reporting_currency,
            "total_value": reporting_currency.as_ref().map(|_| reporting_total),
            "weighted_value": reporting_currency.as_ref().map(|_| reporting_weighted),
            "unconverted_count": unconverted,
        },
    })))
}

// ── create / detail / update / delete ───────────────────────────────────────────────────────

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<super::models::DealUpsert>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    if req.external_ref.is_some() {
        // The console must not mint an external reference: that identifier belongs to the outside
        // system, and letting a human type one would let them collide with a real push.
        return Err(AppError::Validation(
            "external_ref is set by the outside system, not the console".to_string(),
        ));
    }
    let outcome = writer::upsert(&state, tenant, req, &actor).await?;
    Ok((StatusCode::CREATED, Json(json!(outcome.deal))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let deal = load_deal(&state, tenant, id).await?;

    let stage: Option<crate::pipelines::models::PipelineStage> =
        sqlx::query_as("SELECT * FROM pipeline_stages WHERE id = $1")
            .bind(deal.stage_id)
            .fetch_optional(&state.db)
            .await?;
    let pipeline_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM pipelines WHERE id = $1 AND tenant_id = $2")
            .bind(deal.pipeline_id)
            .bind(tenant)
            .fetch_optional(&state.db)
            .await?;
    let owner: Option<PersonRef> = match deal.owner_id {
        Some(uid) => {
            sqlx::query_as("SELECT id, name, email FROM users WHERE id = $1 AND tenant_id = $2")
                .bind(uid)
                .bind(tenant)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };
    let contact: Option<PersonRef> = match deal.contact_id {
        Some(cid) => {
            sqlx::query_as(
                "SELECT id, (first_name || ' ' || COALESCE(last_name,'')) AS name, email
               FROM contacts WHERE id = $1 AND tenant_id = $2",
            )
            .bind(cid)
            .bind(tenant)
            .fetch_optional(&state.db)
            .await?
        }
        None => None,
    };
    let company_name: Option<String> = match deal.company_id {
        Some(cid) => {
            sqlx::query_scalar("SELECT name FROM companies WHERE id = $1 AND tenant_id = $2")
                .bind(cid)
                .bind(tenant)
                .fetch_optional(&state.db)
                .await?
        }
        None => None,
    };

    let collaborators: Vec<DealCollaborator> = sqlx::query_as(
        "SELECT c.id, c.user_id, u.name, u.email, c.role, c.created_at
           FROM deal_collaborators c
           JOIN users u ON u.id = c.user_id
          WHERE c.opportunity_id = $1 AND c.tenant_id = $2
          ORDER BY u.name",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;

    let custom_fields: Vec<DealFieldValue> = sqlx::query_as(
        "SELECT d.id AS field_id, d.key, d.label, d.field_type, v.value,
                v.money_amount::float8 AS money_amount, v.money_currency,
                v.money_exchange_rate::float8 AS money_exchange_rate,
                v.money_reporting_currency
           FROM deal_custom_fields d
           LEFT JOIN deal_field_values v ON v.field_id = d.id AND v.opportunity_id = $1
          WHERE d.tenant_id = $2 AND d.is_active
          ORDER BY d.position, d.label",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;

    let activities: Vec<DealActivity> = sqlx::query_as(
        "SELECT id, kind, body, actor_id, actor_label, metadata, created_at
           FROM deal_activities WHERE opportunity_id = $1 AND tenant_id = $2
          ORDER BY created_at DESC LIMIT 200",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;

    let tasks: Vec<DealTask> = sqlx::query_as(
        "SELECT t.id, t.title, t.notes, t.due_date, t.assigned_to, u.name AS assignee_name,
                t.completed_at, t.created_at, t.updated_at
           FROM deal_tasks t LEFT JOIN users u ON u.id = t.assigned_to
          WHERE t.opportunity_id = $1 AND t.tenant_id = $2
          ORDER BY (t.completed_at IS NOT NULL), t.due_date NULLS LAST, t.created_at",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;

    let attachments: Vec<DealAttachment> = sqlx::query_as(
        "SELECT id, filename, content_type, size_bytes, uploaded_by, created_at
           FROM deal_attachments WHERE opportunity_id = $1 AND tenant_id = $2
          ORDER BY created_at DESC",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;

    let stage_history: Vec<StageMove> = sqlx::query_as(
        "SELECT h.from_stage_id, h.to_stage_id, h.moved_at, u.name AS moved_by_name
           FROM opportunity_stage_history h LEFT JOIN users u ON u.id = h.moved_by
          WHERE h.opportunity_id = $1
          ORDER BY h.moved_at DESC LIMIT 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;

    let weighted = deal.value.unwrap_or(0.0) * f64::from(deal.probability.unwrap_or(0)) / 100.0;
    let days_in_stage = (chrono::Utc::now() - deal.stage_entered_at).num_days();

    let detail = DealDetail {
        deal,
        stage,
        pipeline_name,
        owner,
        contact,
        company_name,
        collaborators,
        custom_fields,
        activities,
        tasks,
        attachments,
        stage_history,
        weighted_value: weighted,
        days_in_stage,
    };
    Ok(Json(json!(detail)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(mut req): Json<super::models::DealUpsert>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    // An update addressed by id must not carry a reference — that would be a second, unchecked
    // dedupe key against the unique index.
    req.external_ref = None;
    let existing = load_deal(&state, tenant, id).await?;
    // The writer keys updates on external_ref. To reuse it, hand it the deal's OWN reference when
    // it has one; a console deal (no ref) takes the console update path below.
    if let Some(reference) = existing.external_ref.clone() {
        req.external_ref = Some(reference);
        let outcome = writer::upsert(&state, tenant, req, &actor).await?;
        return Ok(Json(json!(outcome.deal)));
    }
    let outcome = console_update(&state, tenant, &existing, req, &actor).await?;
    Ok(Json(json!(outcome)))
}

/// Update a console-created (reference-less) deal. Same writer semantics, addressed by id.
async fn console_update(
    state: &AppState,
    tenant: Uuid,
    existing: &OpportunityFull,
    req: super::models::DealUpsert,
    actor: &Actor<'_>,
) -> Result<OpportunityFull, AppError> {
    let wanted_stage = match (req.stage_id, req.stage.as_deref()) {
        (Some(sid), _) => Some(sid),
        (None, Some(name)) => {
            let sid: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND lower(name) = lower($2) LIMIT 1",
            )
            .bind(existing.pipeline_id)
            .bind(name)
            .fetch_optional(&state.db)
            .await?;
            Some(sid.ok_or_else(|| {
                AppError::Validation(format!("stage \"{name}\" does not exist on this pipeline"))
            })?)
        }
        _ => None,
    };

    let contact_id = match (req.contact_id, req.contact_email.as_deref()) {
        (Some(cid), _) => {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM contacts WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(cid)
            .bind(tenant)
            .fetch_one(&state.db)
            .await?;
            if !ok {
                return Err(AppError::NotFound(format!("Contact {cid} not found")));
            }
            Some(cid)
        }
        (None, Some(email)) => {
            writer::find_or_create_contact(
                state,
                tenant,
                email,
                req.contact_name.as_deref(),
                req.contact_phone.as_deref(),
            )
            .await?
        }
        _ => None,
    };

    let owner_id = match (req.owner_id, req.owner_email.as_deref()) {
        (Some(uid), _) => {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(uid)
            .bind(tenant)
            .fetch_one(&state.db)
            .await?;
            if !ok {
                return Err(AppError::NotFound(format!("User {uid} not found")));
            }
            Some(uid)
        }
        _ => None,
    };

    let company_id = match (req.company_id, req.company_name.as_deref()) {
        (Some(cid), _) => {
            let ok: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM companies WHERE id = $1 AND tenant_id = $2)",
            )
            .bind(cid)
            .bind(tenant)
            .fetch_one(&state.db)
            .await?;
            if !ok {
                return Err(AppError::NotFound(format!("Company {cid} not found")));
            }
            Some(cid)
        }
        _ => None,
    };

    let flags = writer::status_flags_pub(req.status.as_deref())?;

    // A currency the console sends is normalized and validated; a blank one keeps the deal's own.
    let currency: Option<String> = match writer::blank_pub(&req.currency) {
        None => None,
        Some(raw) => {
            let up = raw.to_uppercase();
            if up.len() != 3 || !up.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(AppError::Validation(format!(
                    "currency must be a 3-letter code — got \"{raw}\""
                )));
            }
            Some(up)
        }
    };
    if let Some(rate) = req.exchange_rate {
        if !rate.is_finite() || rate <= 0.0 {
            return Err(AppError::Validation(
                "exchange_rate must be a number greater than zero".to_string(),
            ));
        }
    }
    let rate_date = if req.exchange_rate.is_some() {
        Some(chrono::Utc::now())
    } else {
        None
    };

    let sql = concat!(
        "UPDATE opportunities SET
            name = COALESCE($3, name),
            stage_id = COALESCE($4, stage_id),
            contact_id = COALESCE($5, contact_id),
            company_id = COALESCE($6, company_id),
            value = COALESCE($7::float8, value),
            currency = COALESCE($8, currency),
            probability = COALESCE($9, probability),
            expected_close_date = COALESCE($10, expected_close_date),
            source = COALESCE($11, source),
            notes = COALESCE($12, notes),
            metadata = COALESCE($13, metadata),
            owner_id = COALESCE($14, owner_id),
            is_won = COALESCE($15, is_won),
            is_lost = COALESCE($16, is_lost),
            lost_reason = COALESCE($17, lost_reason),
            exchange_rate = COALESCE($18::float8, exchange_rate),
            rate_date = COALESCE($19, rate_date),
            updated_at = NOW(), last_activity_at = NOW()
          WHERE id = $1 AND tenant_id = $2 RETURNING ",
        opp_cols!()
    );
    let row: OpportunityFull = sqlx::query_as(sql)
        .bind(existing.id)
        .bind(tenant)
        .bind(writer::blank_pub(&req.name))
        .bind(wanted_stage)
        .bind(contact_id)
        .bind(company_id)
        .bind(req.value)
        .bind(currency)
        .bind(req.probability)
        .bind(req.expected_close_date)
        .bind(writer::blank_pub(&req.source))
        .bind(writer::blank_pub(&req.notes))
        .bind(req.metadata.clone())
        .bind(owner_id)
        .bind(flags.map(|f| f.0))
        .bind(flags.map(|f| f.1))
        .bind(writer::blank_pub(&req.lost_reason))
        .bind(req.exchange_rate)
        .bind(rate_date)
        .fetch_one(&state.db)
        .await?;

    if let Some(fields) = req.custom_fields.as_ref() {
        writer::write_custom_fields_pub(state, tenant, existing.id, fields).await?;
    }
    if let Some(ids) = req.collaborator_ids.as_ref() {
        writer::set_collaborators_pub(state, tenant, existing.id, ids).await?;
    }
    writer::touch(
        &state.db,
        tenant,
        existing.id,
        "updated",
        Some("Deal edited in the console"),
        actor,
        json!({}),
    )
    .await?;

    if let Some(sid) = wanted_stage {
        if sid != existing.stage_id {
            let (moved, _) = writer::move_stage(state, tenant, existing.id, sid, actor).await?;
            return Ok(moved);
        }
    }
    Ok(row)
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let user = user_of(&claims)?;
    let gone: Option<(String, Option<f64>)> = sqlx::query_as(
        "SELECT name, value::float8 FROM opportunities WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant)
    .fetch_optional(&state.db)
    .await?;
    let r = sqlx::query("DELETE FROM opportunities WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Deal {id} not found")));
    }
    if let Some((name, value)) = gone {
        audit::logger::log_event(
            &state.db,
            tenant,
            Some(user),
            "opportunity.deleted",
            "opportunity",
            Some(id),
            Some(json!({"name": name, "value": value})),
            None,
        )
        .await;
    }
    Ok(Json(json!({"message": "Deal deleted"})))
}

// ── stage / status ──────────────────────────────────────────────────────────────────────────

pub async fn move_deal(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<MoveDealRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    let deal = load_deal(&state, tenant, id).await?;
    let stage_id = match (req.stage_id, req.stage.as_deref()) {
        (Some(sid), _) => sid,
        (None, Some(name)) => sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM pipeline_stages WHERE pipeline_id = $1 AND lower(name) = lower($2) LIMIT 1",
        )
        .bind(deal.pipeline_id)
        .bind(name)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Validation(format!("stage \"{name}\" does not exist")))?,
        (None, None) => {
            return Err(AppError::Validation(
                "stage_id or stage is required".to_string(),
            ))
        }
    };
    let (row, moved) = writer::move_stage(&state, tenant, id, stage_id, &actor).await?;
    Ok(Json(json!({"deal": row, "moved": moved})))
}

pub async fn set_status(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<DealStatusRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    let row = writer::set_status(
        &state,
        tenant,
        id,
        &req.status,
        req.lost_reason.as_deref(),
        &actor,
    )
    .await?;
    Ok(Json(json!(row)))
}

// ── activity timeline ──────────────────────────────────────────────────────────────────────

pub async fn add_activity(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<NewActivityRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    load_deal(&state, tenant, id).await?;
    let body = req.body.trim();
    if body.is_empty() {
        return Err(AppError::Validation("body is required".to_string()));
    }
    let kind = req.kind.as_deref().unwrap_or("note");
    if !matches!(kind, "note" | "call" | "email" | "meeting") {
        return Err(AppError::Validation(
            "kind must be note/call/email/meeting".to_string(),
        ));
    }
    writer::touch(&state.db, tenant, id, kind, Some(body), &actor, json!({})).await?;
    let row: DealActivity = sqlx::query_as(
        "SELECT id, kind, body, actor_id, actor_label, metadata, created_at
           FROM deal_activities WHERE opportunity_id = $1 AND tenant_id = $2
          ORDER BY created_at DESC LIMIT 1",
    )
    .bind(id)
    .bind(tenant)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

// ── tasks / follow-ups ────────────────────────────────────────────────────────────────────

pub async fn add_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<NewTaskRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let user = user_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    load_deal(&state, tenant, id).await?;
    let title = req.title.trim();
    if title.is_empty() {
        return Err(AppError::Validation("title is required".to_string()));
    }
    if let Some(uid) = req.assigned_to {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(uid)
        .bind(tenant)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("User {uid} not found")));
        }
    }
    let task: DealTask = sqlx::query_as(
        "WITH ins AS (
            INSERT INTO deal_tasks (tenant_id, opportunity_id, title, notes, due_date, assigned_to, created_by)
            VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING *
         )
         SELECT i.id, i.title, i.notes, i.due_date, i.assigned_to, u.name AS assignee_name,
                i.completed_at, i.created_at, i.updated_at
           FROM ins i LEFT JOIN users u ON u.id = i.assigned_to",
    )
    .bind(tenant)
    .bind(id)
    .bind(title)
    .bind(req.notes.as_deref())
    .bind(req.due_date)
    .bind(req.assigned_to)
    .bind(user)
    .fetch_one(&state.db)
    .await?;
    writer::touch(
        &state.db,
        tenant,
        id,
        "task",
        Some(&format!("Follow-up added: {title}")),
        &actor,
        json!({"task_id": task.id}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(task))))
}

pub async fn update_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, task_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateTaskRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    load_deal(&state, tenant, id).await?;
    let completed_at: Option<chrono::DateTime<chrono::Utc>> = match req.completed {
        Some(true) => Some(chrono::Utc::now()),
        Some(false) => None,
        None => None,
    };
    let task: DealTask = sqlx::query_as(
        "WITH upd AS (
            UPDATE deal_tasks SET
                title = COALESCE($4, title),
                notes = COALESCE($5, notes),
                due_date = COALESCE($6, due_date),
                assigned_to = COALESCE($7, assigned_to),
                completed_at = CASE WHEN $8::bool IS NULL THEN completed_at
                                    WHEN $8 THEN COALESCE(completed_at, NOW())
                                    ELSE NULL END,
                updated_at = NOW()
              WHERE id = $1 AND opportunity_id = $2 AND tenant_id = $3 RETURNING *
         )
         SELECT u.id, u.title, u.notes, u.due_date, u.assigned_to, us.name AS assignee_name,
                u.completed_at, u.created_at, u.updated_at
           FROM upd u LEFT JOIN users us ON us.id = u.assigned_to",
    )
    .bind(task_id)
    .bind(id)
    .bind(tenant)
    .bind(
        req.title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    )
    .bind(req.notes.as_deref())
    .bind(req.due_date)
    .bind(req.assigned_to)
    .bind(req.completed)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Task {task_id} not found")))?;

    if let Some(done) = req.completed {
        writer::touch(
            &state.db,
            tenant,
            id,
            "task",
            Some(if done {
                "Follow-up completed"
            } else {
                "Follow-up reopened"
            }),
            &actor,
            json!({"task_id": task_id, "completed_at": completed_at}),
        )
        .await?;
    }
    Ok(Json(json!(task)))
}

pub async fn delete_task(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, task_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let r = sqlx::query(
        "DELETE FROM deal_tasks WHERE id = $1 AND opportunity_id = $2 AND tenant_id = $3",
    )
    .bind(task_id)
    .bind(id)
    .bind(tenant)
    .execute(&state.db)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Task {task_id} not found")));
    }
    Ok(Json(json!({"message": "Follow-up deleted"})))
}

// ── attachments ───────────────────────────────────────────────────────────────────────────

pub async fn add_attachment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<NewAttachmentRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let user = user_of(&claims)?;
    let (actor_id, actor_label) = label_for(&state, &claims).await;
    let actor = Actor {
        user_id: actor_id,
        label: &actor_label,
    };
    load_deal(&state, tenant, id).await?;
    let filename = req.filename.trim();
    if filename.is_empty() {
        return Err(AppError::Validation("filename is required".to_string()));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(req.content_base64.trim())
        .map_err(|_| AppError::Validation("content_base64 is not valid base64".to_string()))?;
    if bytes.is_empty() {
        return Err(AppError::Validation("the file is empty".to_string()));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AppError::Validation(format!(
            "the file is larger than {} MB",
            MAX_ATTACHMENT_BYTES / (1024 * 1024)
        )));
    }
    let content_type = req
        .content_type
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();
    let att: DealAttachment = sqlx::query_as(
        "INSERT INTO deal_attachments (tenant_id, opportunity_id, filename, content_type, size_bytes, content, uploaded_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         RETURNING id, filename, content_type, size_bytes, uploaded_by, created_at",
    )
    .bind(tenant)
    .bind(id)
    .bind(filename)
    .bind(&content_type)
    .bind(bytes.len() as i32)
    .bind(&bytes)
    .bind(user)
    .fetch_one(&state.db)
    .await?;
    writer::touch(
        &state.db,
        tenant,
        id,
        "file",
        Some(&format!("Attached {filename}")),
        &actor,
        json!({"attachment_id": att.id, "size_bytes": att.size_bytes}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!(att))))
}

pub async fn download_attachment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, att_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Response> {
    let tenant = tenant_of(&claims)?;
    let row: Option<(String, String, Vec<u8>)> = sqlx::query_as(
        "SELECT filename, content_type, content FROM deal_attachments
          WHERE id = $1 AND opportunity_id = $2 AND tenant_id = $3",
    )
    .bind(att_id)
    .bind(id)
    .bind(tenant)
    .fetch_optional(&state.db)
    .await?;
    let (filename, content_type, content) =
        row.ok_or_else(|| AppError::NotFound(format!("Attachment {att_id} not found")))?;
    // The filename is caller-supplied, so it is quoted and the quotes/CR/LF stripped: an
    // attachment called `x"bpln` must not be able to break the header.
    let safe_name = filename.replace(['"', '\r', '\n'], "_");
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{safe_name}\""),
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        content,
    )
        .into_response())
}

pub async fn delete_attachment(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path((id, att_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let r = sqlx::query(
        "DELETE FROM deal_attachments WHERE id = $1 AND opportunity_id = $2 AND tenant_id = $3",
    )
    .bind(att_id)
    .bind(id)
    .bind(tenant)
    .execute(&state.db)
    .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Attachment {att_id} not found")));
    }
    Ok(Json(json!({"message": "Attachment deleted"})))
}

// ── per-workspace custom field definitions ────────────────────────────────────────────────

pub async fn list_fields(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let fields: Vec<DealFieldDefinition> = sqlx::query_as(
        "SELECT id, key, label, field_type, options, position, is_active, source_app, created_at, updated_at
           FROM deal_custom_fields WHERE tenant_id = $1 ORDER BY position, label",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"fields": fields})))
}

pub async fn create_field(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<NewFieldRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let label = req.label.trim();
    if label.is_empty() {
        return Err(AppError::Validation("label is required".to_string()));
    }
    let key = req
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(writer::normalize_field_key_pub)
        .unwrap_or_else(|| writer::normalize_field_key_pub(label));
    let field_type = req
        .field_type
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("text");
    if !matches!(
        field_type,
        "text" | "number" | "money" | "date" | "boolean" | "url" | "select"
    ) {
        return Err(AppError::Validation(
            "field_type must be text/number/money/date/boolean/url/select".to_string(),
        ));
    }
    let field: DealFieldDefinition = sqlx::query_as(
        "INSERT INTO deal_custom_fields (tenant_id, key, label, field_type, options, position, source_app)
         VALUES ($1,$2,$3,$4,$5,$6,'console')
         ON CONFLICT (tenant_id, key) DO UPDATE SET label = EXCLUDED.label, is_active = true, updated_at = NOW()
         RETURNING id, key, label, field_type, options, position, is_active, source_app, created_at, updated_at",
    )
    .bind(tenant)
    .bind(&key)
    .bind(label)
    .bind(field_type)
    .bind(req.options.clone().unwrap_or_else(|| json!([])))
    .bind(req.position.unwrap_or(0))
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(field))))
}

pub async fn update_field(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(field_id): Path<Uuid>,
    Json(req): Json<UpdateFieldRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let field: DealFieldDefinition = sqlx::query_as(
        "UPDATE deal_custom_fields SET
            label = COALESCE($3, label),
            field_type = COALESCE($4, field_type),
            options = COALESCE($5, options),
            position = COALESCE($6, position),
            is_active = COALESCE($7, is_active),
            updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2
          RETURNING id, key, label, field_type, options, position, is_active, source_app, created_at, updated_at",
    )
    .bind(field_id)
    .bind(tenant)
    .bind(req.label.as_deref().map(str::trim).filter(|l| !l.is_empty()))
    .bind(req.field_type.as_deref())
    .bind(req.options.clone())
    .bind(req.position)
    .bind(req.is_active)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Field {field_id} not found")))?;
    Ok(Json(json!(field)))
}

pub async fn delete_field(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(field_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let r = sqlx::query("DELETE FROM deal_custom_fields WHERE id = $1 AND tenant_id = $2")
        .bind(field_id)
        .bind(tenant)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Field {field_id} not found")));
    }
    Ok(Json(json!({"message": "Field deleted"})))
}

// ── configurable lost-reason list ─────────────────────────────────────────────────────────

pub async fn list_lost_reasons(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let reasons: Vec<LostReason> = sqlx::query_as(
        "SELECT id, label, position, is_active FROM deal_lost_reasons
          WHERE tenant_id = $1 ORDER BY position, label",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"reasons": reasons})))
}

pub async fn create_lost_reason(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<NewLostReasonRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let label = req.label.trim();
    if label.is_empty() {
        return Err(AppError::Validation("label is required".to_string()));
    }
    let reason: LostReason = sqlx::query_as(
        "INSERT INTO deal_lost_reasons (tenant_id, label, position) VALUES ($1,$2,$3)
         ON CONFLICT (tenant_id, label) DO UPDATE SET is_active = true, updated_at = NOW()
         RETURNING id, label, position, is_active",
    )
    .bind(tenant)
    .bind(label)
    .bind(req.position.unwrap_or(100))
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(reason))))
}

pub async fn update_lost_reason(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(reason_id): Path<Uuid>,
    Json(req): Json<NewLostReasonRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let reason: LostReason = sqlx::query_as(
        "UPDATE deal_lost_reasons SET label = COALESCE($3, label),
                position = COALESCE($4, position), updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2 RETURNING id, label, position, is_active",
    )
    .bind(reason_id)
    .bind(tenant)
    .bind(req.label.trim())
    .bind(req.position)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Lost reason {reason_id} not found")))?;
    Ok(Json(json!(reason)))
}

pub async fn delete_lost_reason(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(reason_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let r = sqlx::query("DELETE FROM deal_lost_reasons WHERE id = $1 AND tenant_id = $2")
        .bind(reason_id)
        .bind(tenant)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!(
            "Lost reason {reason_id} not found"
        )));
    }
    Ok(Json(json!({"message": "Lost reason deleted"})))
}

// ── stage automation hooks ───────────────────────────────────────────────────────────────

pub async fn list_stage_automations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let pipeline_filter = params
        .get("pipeline_id")
        .and_then(|v| Uuid::parse_str(v).ok());
    let hooks: Vec<StageAutomation> = sqlx::query_as(
        "SELECT a.id, a.pipeline_id, a.stage_id, s.name AS stage_name, a.action_kind,
                a.tag_id, t.name AS tag_name, a.is_active
           FROM deal_stage_automations a
           JOIN pipeline_stages s ON s.id = a.stage_id
           LEFT JOIN tags t ON t.id = a.tag_id
          WHERE a.tenant_id = $1 AND ($2::uuid IS NULL OR a.pipeline_id = $2)
          ORDER BY s.position, a.created_at",
    )
    .bind(tenant)
    .bind(pipeline_filter)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"hooks": hooks})))
}

pub async fn create_stage_automation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<NewStageAutomationRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    if !matches!(req.action_kind.as_str(), "tag" | "workflow") {
        return Err(AppError::Validation(
            "action_kind must be tag or workflow".to_string(),
        ));
    }
    if req.action_kind == "tag" && req.tag_id.is_none() {
        return Err(AppError::Validation(
            "tag_id is required for a tag hook".to_string(),
        ));
    }
    let stage: Option<(Uuid,)> = sqlx::query_as(
        "SELECT s.pipeline_id FROM pipeline_stages s
           JOIN pipelines p ON p.id = s.pipeline_id
          WHERE s.id = $1 AND p.tenant_id = $2",
    )
    .bind(req.stage_id)
    .bind(tenant)
    .fetch_optional(&state.db)
    .await?;
    let (pipeline_id,) = stage.ok_or_else(|| AppError::NotFound("Stage not found".to_string()))?;
    if let Some(tag_id) = req.tag_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(tag_id)
        .bind(tenant)
        .fetch_one(&state.db)
        .await?;
        if !ok {
            return Err(AppError::NotFound(format!("Tag {tag_id} not found")));
        }
    }
    let hook: StageAutomation = sqlx::query_as(
        "WITH ins AS (
            INSERT INTO deal_stage_automations (tenant_id, pipeline_id, stage_id, action_kind, tag_id)
            VALUES ($1,$2,$3,$4,$5) RETURNING *
         )
         SELECT i.id, i.pipeline_id, i.stage_id, s.name AS stage_name, i.action_kind,
                i.tag_id, t.name AS tag_name, i.is_active
           FROM ins i JOIN pipeline_stages s ON s.id = i.stage_id
           LEFT JOIN tags t ON t.id = i.tag_id",
    )
    .bind(tenant)
    .bind(pipeline_id)
    .bind(req.stage_id)
    .bind(&req.action_kind)
    .bind(req.tag_id)
    .fetch_one(&state.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(hook))))
}

pub async fn delete_stage_automation(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(hook_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let r = sqlx::query("DELETE FROM deal_stage_automations WHERE id = $1 AND tenant_id = $2")
        .bind(hook_id)
        .bind(tenant)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Hook {hook_id} not found")));
    }
    Ok(Json(json!({"message": "Hook deleted"})))
}

// ── reporting ──────────────────────────────────────────────────────────────────────────────

pub async fn report(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<DealListParams>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let rot_days = params.rot_days.unwrap_or(14).clamp(1, 365);
    let reports =
        super::reporting::build_report(&state, tenant, params.pipeline_id, rot_days).await?;
    Ok(Json(json!({"reports": reports, "rot_days": rot_days})))
}

pub async fn team(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let people: Vec<PersonRef> = sqlx::query_as(
        "SELECT id, name, email FROM users
          WHERE tenant_id = $1 AND is_active AND closed_at IS NULL ORDER BY name",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({"people": people})))
}

/// Keep `Value` in use for the json! macro's type inference in this module.
#[allow(dead_code)]
fn _value_marker(_v: Value) {}

// ── workspace currency (CORE: every deal states its money) ────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct CurrencySetting {
    /// 3-letter ISO 4217 code, e.g. "USD", "EUR", "GBP".
    pub currency: Option<String>,
    /// What reports are computed in. Defaults to `currency` when a workspace only states one.
    pub reporting_currency: Option<String>,
}

pub async fn get_currency(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    let (default, reporting) = fx::workspace_currencies(&state.db, tenant).await?;
    // `set: false` is the console's cue to ask the human, not to print a bracket of dollars.
    Ok(Json(json!({
        "currency": default,
        "reporting_currency": reporting,
        "set": default.is_some(),
        "iso_currencies": fx::ISO_4217,
    })))
}

/// Set the workspace's currencies (default and reporting). Merges two keys into
/// `tenants.settings`, so it can never clobber branding, booking or integration settings that
/// share the column. Both codes are validated against the real ISO 4217 list — a free-text code
/// would store a figure nothing can convert.
///
/// Changing the REPORTING currency re-quotes every deal in the workspace using the rate that was
/// true on each deal's own rate date, so the switch does not silently re-price history from
/// today's market (David's condition: a past won deal's value does not move).
pub async fn set_currency(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CurrencySetting>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&claims)?;
    if !crate::auth::middleware::is_account_admin(&claims.role) {
        return Err(AppError::Forbidden);
    }
    if req.currency.is_none() && req.reporting_currency.is_none() {
        return Err(AppError::Validation(
            "send `currency` (the workspace default) and/or `reporting_currency`".to_string(),
        ));
    }
    let default = req.currency.as_deref().map(fx::normalize_iso).transpose()?;
    let reporting = req
        .reporting_currency
        .as_deref()
        .map(fx::normalize_iso)
        .transpose()?;

    let (default, reporting) =
        fx::set_workspace_currencies(&state.db, tenant, default.as_deref(), reporting.as_deref())
            .await?;

    let mut requoted = 0u64;
    if let Some(rc) = reporting.as_deref() {
        // Only a CHANGE of destination needs a re-quote: re-running it on every save would move
        // rates around for no reason.
        requoted = fx::requote_tenant(&state.db, tenant, rc).await?;
    }
    Ok(Json(json!({
        "currency": default,
        "reporting_currency": reporting,
        "set": default.is_some(),
        "deals_requoted": requoted,
    })))
}

/// The rate table's latest row per currency — what the console's settings panel shows, and what a
/// human reads when a cross-currency deal needs a rate.
pub async fn get_fx_rates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let _tenant = tenant_of(&claims)?;
    let rows = fx::latest_rates(&state.db).await?;
    let rates: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|(currency, usd_rate, as_of)| {
            json!({ "currency": currency, "usd_rate": usd_rate, "as_of": as_of })
        })
        .collect();
    Ok(Json(json!({ "rates": rates })))
}

/// Pull a fresh snapshot of the rate table from the provider. Driven by the fleet's n8n cron with
/// the shared internal key; also reachable by an account admin from the console, because a
/// workspace whose rates are a week stale should not have to wait for a scheduler.
pub async fn refresh_fx_rates(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let _tenant = tenant_of(&claims)?;
    if !crate::auth::middleware::is_account_admin(&claims.role) {
        return Err(AppError::Forbidden);
    }
    let (stored, as_of) = fx::refresh_rates(&state.db).await?;
    Ok(Json(
        json!({ "stored": stored, "as_of": as_of, "message": format!("{stored} rates stored for {as_of}") }),
    ))
}

/// The same refresh, for the fleet's scheduler (n8n cron): no session, the shared internal key is
/// the credential — checked here and again at the credential boundary (`INTERNAL_ROUTES`). The
/// scheduled path exists because a rate table that only moves when a human presses a button is a
/// rate table that silently ages.
pub async fn internal_refresh_fx(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.config.internal_sync_key.clone();
    if expected.is_empty() || key != expected {
        return Err(AppError::Unauthorized);
    }
    let (stored, as_of) = fx::refresh_rates(&state.db).await?;
    Ok(Json(
        json!({ "stored": stored, "as_of": as_of, "message": format!("{stored} rates stored for {as_of}") }),
    ))
}
