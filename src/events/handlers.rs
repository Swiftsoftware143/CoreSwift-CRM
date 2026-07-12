use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use serde_json::{json, Value};
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;
use super::models::*;
use super::dispatcher;

/// POST /api/events/ingest/{source} — Receive webhook from any external service
///
/// Normalizes incoming events from landing pages, directories, SaaS platforms.
/// source can be anything: "landing-page", "directory", "saas-app", "n8n"
pub async fn ingest(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(source): Path<String>,
    headers: axum::http::HeaderMap,
    Json(body): Json<IngestPayload>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if body.event_type.is_empty() {
        return Err(AppError::Validation("event_type is required".to_string()));
    }

    // Capture relevant headers for audit
    let raw_headers = json!({
        "content_type": headers.get("content-type").and_then(|v| v.to_str().ok()),
        "user_agent": headers.get("user-agent").and_then(|v| v.to_str().ok()),
        "x_forwarded_for": headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()),
    });

    // Store event
    let event = sqlx::query_as::<_, Event>(
        r#"INSERT INTO events (id, tenant_id, source, event_type, entity_type, entity_id, payload, raw_headers)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(tid)
    .bind(&source)
    .bind(&body.event_type)
    .bind(&body.entity_type)
    .bind(body.entity_id)
    .bind(&body.payload)
    .bind(raw_headers)
    .fetch_one(&s.db)
    .await?;

    // Dispatch to automation engine (fire-and-forget)
    let db_clone = s.db.clone();
    let _eid = event.id;
    let et = event.event_type.clone();
    let ent_t = event.entity_type.clone();
    let ent_id = event.entity_id;
    let tid_clone = tid;
    let payload = event.payload.clone();
    tokio::spawn(async move {
        dispatcher::dispatch_automation(&db_clone, tid_clone, &et, ent_t.as_deref(), ent_id, &payload).await;
    });

    // Log audit
    crate::audit::logger::log_event(&s.db, tid, Some(Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?),
        &format!("event.ingested.{}", source), "event", Some(event.id),
        Some(json!({"event_type": body.event_type, "source": source})), None).await;

    Ok((StatusCode::CREATED, Json(json!(event))))
}

/// GET /api/events/ingest/{source} — Simple health check for webhook endpoints
pub async fn ingest_get(
    State(_s): State<AppState>,
    Path(source): Path<String>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(json!({"status": "ready", "source": source, "message": "Webhook endpoint active. POST events here."})))
}

/// GET /api/events — List events for tenant (filterable)
pub async fn list_events(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(p): Query<Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let source = p.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let event_type = p.get("event_type").and_then(|v| v.as_str()).unwrap_or("");

    let (events, total): (Vec<Event>, i64) = if !source.is_empty() {
        let ev = sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE tenant_id = $1 AND source = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ).bind(tid).bind(source).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let ct = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND source = $2"
        ).bind(tid).bind(source).fetch_one(&s.db).await?.unwrap_or(0);
        (ev, ct)
    } else if !event_type.is_empty() {
        let ev = sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE tenant_id = $1 AND event_type = $2 ORDER BY created_at DESC LIMIT $3 OFFSET $4"
        ).bind(tid).bind(event_type).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let ct = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM events WHERE tenant_id = $1 AND event_type = $2"
        ).bind(tid).bind(event_type).fetch_one(&s.db).await?.unwrap_or(0);
        (ev, ct)
    } else {
        let ev = sqlx::query_as::<_, Event>(
            "SELECT * FROM events WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;
        let ct = sqlx::query_scalar::<_, Option<i64>>(
            "SELECT COUNT(*) FROM events WHERE tenant_id = $1"
        ).bind(tid).fetch_one(&s.db).await?.unwrap_or(0);
        (ev, ct)
    };

    Ok(Json(json!({"events": events, "total": total, "page": page, "per_page": per_page})))
}

/// GET /api/events/{id} — Get single event
pub async fn get_event(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let event = sqlx::query_as::<_, Event>("SELECT * FROM events WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("Event not found".to_string()))?;
    Ok(Json(json!(event)))
}

// ====== Delayed Action Engine (If-Not-Then) ======

/// GET /api/events/delayed — List pending delayed actions
pub async fn list_delayed(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let actions = sqlx::query_as::<_, DelayedAction>(
        "SELECT * FROM delayed_actions WHERE tenant_id = $1 AND cancelled = false ORDER BY execute_at ASC LIMIT $2 OFFSET $3"
    ).bind(tid).bind(per_page).bind(offset).fetch_all(&s.db).await?;

    let total: i64 = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM delayed_actions WHERE tenant_id = $1 AND cancelled = false"
    ).bind(tid).fetch_one(&s.db).await?.unwrap_or(0);

    Ok(Json(json!({"delayed_actions": actions, "total": total, "page": page, "per_page": per_page})))
}

/// POST /api/events/delayed — Schedule an "If-Not-Then" delayed action
pub async fn schedule_delayed(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<ScheduleDelayedRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if !["timeout", "no_event", "no_action"].contains(&r.condition_type.as_str()) {
        return Err(AppError::Validation("condition_type must be 'timeout', 'no_event', or 'no_action'".to_string()));
    }

    let execute_at = chrono::DateTime::parse_from_rfc3339(&r.execute_at)
        .map_err(|e| AppError::Validation(format!("Invalid execute_at timestamp: {}", e)))?
        .with_timezone(&chrono::Utc);

    if execute_at < chrono::Utc::now() {
        return Err(AppError::Validation("execute_at must be in the future".to_string()));
    }

    let action = sqlx::query_as::<_, DelayedAction>(
        r#"INSERT INTO delayed_actions (id, tenant_id, trigger_event_id, condition_type, condition_config, action_type, action_config, execute_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(r.trigger_event_id)
    .bind(&r.condition_type).bind(&r.condition_config)
    .bind(&r.action_type).bind(&r.action_config).bind(execute_at)
    .fetch_one(&s.db).await?;

    // Schedule evaluation via the delay engine (fire-and-forget)
    let db_clone = s.db.clone();
    let da_id = action.id;
    let da_exec = action.execute_at;
    tokio::spawn(async move {
        let wait_ms = (da_exec - chrono::Utc::now()).num_milliseconds().max(0) as u64;
        tokio::time::sleep(tokio::time::Duration::from_millis(wait_ms)).await;
        dispatcher::evaluate_delayed_action(&db_clone, da_id).await;
    });

    Ok((StatusCode::CREATED, Json(json!(action))))
}

/// DELETE /api/events/delayed/{id} — Cancel a delayed action
pub async fn cancel_delayed(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("UPDATE delayed_actions SET cancelled = true, updated_at = NOW() WHERE id = $1 AND tenant_id = $2")
        .bind(id).bind(tid)
        .execute(&s.db).await?;
    Ok(Json(json!({"message": "Delayed action cancelled"})))
}
