//! Visual data mapping — the CoreSwift side of the mapper.
//!
//! Contract: `/opt/swift/docs/coreswift-data-mapping-spec-2026-11-13.md`.
//!
//! Three moving parts:
//!   * `schema`    — what the destination accepts + a proxy to a sibling app's field description,
//!   * `transforms`— deterministic `{{token}}` resolution and the transform vocabulary,
//!   * `runner`    — resolve a saved mapping against one source row and write it through the SAME
//!                   contact writer the public external endpoint uses.
//!
//! Nothing here invents data: an unresolvable token is reported, a missing required field is refused
//! with its name, and a source that cannot describe itself yields an error rather than a field list.

pub mod runner;
pub mod schema;
pub mod transforms;

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    routing::{get, post},
    Extension, Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Claims;
use crate::errors::AppError;
use crate::external_api;
use crate::AppState;

type ApiResult<T> = Result<Json<T>, AppError>;

/// One `field_map` entry: a destination field, the template that fills it, and optional shaping.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldMapEntry {
    /// Destination address — a contact column or `metadata.<path>`.
    pub to: String,
    /// Plain text plus `{{tokens}}`.
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub transform: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub default: Option<String>,
}

/// A saved mapping row.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Mapping {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// `{"app":"funnelswift","entity":"leads","schema_version":1}`
    pub source: Value,
    /// `{"entity":"contact","list_id":null}`
    pub destination: Value,
    /// jsonb column, decoded through serde into typed entries.
    #[sqlx(json)]
    pub field_map: Vec<FieldMapEntry>,
    /// `{"keys":["email"],"on_match":"update"}`
    pub dedupe: Value,
    /// `{"skip_empty":true}`
    pub options: Value,
    pub enabled: bool,
}

impl Mapping {
    /// The dedupe key names this mapping matches on, lower-cased and de-duplicated.
    pub fn dedupe_keys(&self) -> Vec<String> {
        self.dedupe
            .get("keys")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|k| k.as_str())
                    .map(|k| k.trim().to_ascii_lowercase())
                    .filter(|k| !k.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The list this mapping adds new contacts to, when one is configured.
    pub fn destination_list_id(&self) -> Option<Uuid> {
        self.destination
            .get("list_id")
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
    }
}

fn tenant_of(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// Validate a mapping body and refuse anything the destination cannot honour (spec §7.7).
///
/// A mapping that targets a field the writer cannot store, or names a transform the runner does not
/// implement, is refused here by name — never saved and then silently dropped at run time.
fn validate(body: &Value) -> Result<(), AppError> {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    if name.is_empty() {
        return Err(AppError::BadRequest("A mapping needs a name.".into()));
    }

    let app = body
        .get("source")
        .and_then(|s| s.get("app"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let entity = body
        .get("source")
        .and_then(|s| s.get("entity"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if app.is_empty() || entity.is_empty() {
        return Err(AppError::BadRequest(
            "A mapping needs a source app and a source entity (e.g. funnelswift / leads).".into(),
        ));
    }

    let dest_entity = body
        .get("destination")
        .and_then(|d| d.get("entity"))
        .and_then(|v| v.as_str())
        .unwrap_or("contact");
    if dest_entity != "contact" {
        return Err(AppError::BadRequest(format!(
            "Destination '{dest_entity}' is not available yet — this milestone writes contacts."
        )));
    }

    if let Some(list_id) = body
        .get("destination")
        .and_then(|d| d.get("list_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        Uuid::parse_str(list_id).map_err(|_| {
            AppError::BadRequest("destination.list_id must be a list id from this account.".into())
        })?;
    }

    let field_map = body
        .get("field_map")
        .and_then(|v| v.as_array())
        .ok_or_else(|| AppError::BadRequest("A mapping needs a field_map array.".into()))?;
    if field_map.is_empty() {
        return Err(AppError::BadRequest(
            "This mapping has no fields to write — map at least one destination field.".into(),
        ));
    }

    for entry in field_map {
        let to = entry
            .get("to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default();
        if to.is_empty() {
            return Err(AppError::BadRequest(
                "Every field_map entry needs a destination field.".into(),
            ));
        }
        if !schema::is_writable_destination(to) {
            return Err(AppError::BadRequest(format!(
                "'{to}' is not a field this account can write. Pick one from the destination list, or use metadata.<path> for extra data."
            )));
        }
        if let Some(t) = entry.get("transform").and_then(|v| v.as_str()) {
            if !transforms::is_known_transform(t) {
                return Err(AppError::BadRequest(format!(
                    "Unknown transform '{t}' on '{to}'. Known transforms: {}.",
                    transforms::TRANSFORMS.join(", ")
                )));
            }
        }
    }

    let keys = body
        .get("dedupe")
        .and_then(|d| d.get("keys"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            AppError::BadRequest("A mapping needs dedupe.keys (e.g. [\"email\"]).".into())
        })?;
    for k in keys {
        let key = k.as_str().unwrap_or_default();
        if !matches!(key, "email" | "phone") {
            return Err(AppError::BadRequest(format!(
                "Dedupe key '{key}' is not supported — use email or phone."
            )));
        }
    }
    if !keys.is_empty() {
        let on_match = body
            .get("dedupe")
            .and_then(|d| d.get("on_match"))
            .and_then(|v| v.as_str())
            .unwrap_or("update");
        if !matches!(on_match, "update" | "skip" | "create_new") {
            return Err(AppError::BadRequest(format!(
                "on_match '{on_match}' is not one of update, skip, create_new."
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- handlers

/// GET /api/data-mapping/destinations — what can be written, what cannot, and why.
pub async fn destinations(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    Ok(Json(schema::destinations(&state, tenant).await?))
}

#[derive(Deserialize)]
pub struct SchemaQuery {
    pub app: String,
}

/// GET /api/data-mapping/schema?app=funnelswift — proxy the source app's own field description.
pub async fn source_schema(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(q): Query<SchemaQuery>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    Ok(Json(schema::source_schema(&state, tenant, &q.app).await?))
}

/// GET /api/data-mapping/mappings
pub async fn list_mappings(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    let rows: Vec<Mapping> = sqlx::query_as(
        "SELECT id, tenant_id, name, source, destination, field_map, dedupe, options, enabled
           FROM integration_mappings WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    // A mapping that has never succeeded must say so (spec §6).
    let mut out = Vec::with_capacity(rows.len());
    for m in rows {
        let (runs, ok, last): (i64, i64, Option<String>) = sqlx::query_as(
            "SELECT count(*),
                    count(*) FILTER (WHERE status IN ('created','updated')),
                    max(created_at) FILTER (WHERE status IN ('created','updated'))::text
               FROM integration_mapping_runs WHERE mapping_id = $1 AND tenant_id = $2",
        )
        .bind(m.id)
        .bind(tenant)
        .fetch_one(&state.db)
        .await
        .map_err(AppError::Database)?;
        out.push(json!({
            "id": m.id, "name": m.name, "source": m.source, "destination": m.destination,
            "field_map": m.field_map, "dedupe": m.dedupe, "options": m.options,
            "enabled": m.enabled, "runs": runs, "successful_runs": ok, "last_success": last,
            "never_succeeded": ok == 0
        }));
    }
    Ok(Json(json!({ "mappings": out })))
}

/// POST /api/data-mapping/mappings
pub async fn create_mapping(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    validate(&body)?;

    let field_map: Vec<FieldMapEntry> =
        serde_json::from_value(body.get("field_map").cloned().unwrap_or(json!([])))
            .map_err(|e| AppError::BadRequest(format!("field_map is malformed: {e}")))?;

    let id = Uuid::new_v4();
    let row: Mapping = sqlx::query_as(
        "INSERT INTO integration_mappings
            (id, tenant_id, name, source, destination, field_map, dedupe, options, enabled)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)
         RETURNING id, tenant_id, name, source, destination, field_map, dedupe, options, enabled",
    )
    .bind(id)
    .bind(tenant)
    .bind(
        body.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim(),
    )
    .bind(body.get("source").cloned().unwrap_or_else(|| json!({})))
    .bind(
        body.get("destination")
            .cloned()
            .unwrap_or_else(|| json!({"entity": "contact", "list_id": null})),
    )
    .bind(serde_json::to_value(&field_map).unwrap_or_else(|_| json!([])))
    .bind(
        body.get("dedupe")
            .cloned()
            .unwrap_or_else(|| json!({"keys": ["email"], "on_match": "update"})),
    )
    .bind(
        body.get("options")
            .cloned()
            .unwrap_or_else(|| json!({"skip_empty": true})),
    )
    .bind(
        body.get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(db) if db.constraint().is_some() => {
            AppError::Conflict("A mapping with that name already exists on this account.".into())
        }
        other => AppError::Database(other),
    })?;

    Ok(Json(json!({ "mapping": row })))
}

/// PUT /api/data-mapping/mappings/{id}
pub async fn update_mapping(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    validate(&body)?;
    let field_map: Vec<FieldMapEntry> =
        serde_json::from_value(body.get("field_map").cloned().unwrap_or(json!([])))
            .map_err(|e| AppError::BadRequest(format!("field_map is malformed: {e}")))?;

    let row: Option<Mapping> = sqlx::query_as(
        "UPDATE integration_mappings SET
            name = $3, source = $4, destination = $5, field_map = $6, dedupe = $7,
            options = $8, enabled = $9, updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2
          RETURNING id, tenant_id, name, source, destination, field_map, dedupe, options, enabled",
    )
    .bind(id)
    .bind(tenant)
    .bind(
        body.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim(),
    )
    .bind(body.get("source").cloned().unwrap_or_else(|| json!({})))
    .bind(
        body.get("destination")
            .cloned()
            .unwrap_or_else(|| json!({"entity": "contact", "list_id": null})),
    )
    .bind(serde_json::to_value(&field_map).unwrap_or_else(|_| json!([])))
    .bind(
        body.get("dedupe")
            .cloned()
            .unwrap_or_else(|| json!({"keys": ["email"], "on_match": "update"})),
    )
    .bind(
        body.get("options")
            .cloned()
            .unwrap_or_else(|| json!({"skip_empty": true})),
    )
    .bind(
        body.get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true),
    )
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(m) => Ok(Json(json!({ "mapping": m }))),
        None => Err(AppError::NotFound(
            "No such mapping on this account.".into(),
        )),
    }
}

/// DELETE /api/data-mapping/mappings/{id}
pub async fn delete_mapping(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    let res = sqlx::query("DELETE FROM integration_mappings WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    if res.rows_affected() == 0 {
        return Err(AppError::NotFound(
            "No such mapping on this account.".into(),
        ));
    }
    Ok(Json(json!({ "deleted": true })))
}

/// GET /api/data-mapping/mappings/{id}
pub async fn get_mapping(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    let m = load_mapping(&state, tenant, id).await?;
    Ok(Json(json!({ "mapping": m })))
}

async fn load_mapping(state: &AppState, tenant: Uuid, id: Uuid) -> Result<Mapping, AppError> {
    let row: Option<Mapping> = sqlx::query_as(
        "SELECT id, tenant_id, name, source, destination, field_map, dedupe, options, enabled
           FROM integration_mappings WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    row.ok_or_else(|| AppError::NotFound("No such mapping on this account.".into()))
}

#[derive(Deserialize)]
pub struct RunRequest {
    /// The source row to resolve against.
    pub row: Value,
    /// Caller-supplied idempotency key. Omitted for interactive previews.
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub source_row_id: Option<String>,
    /// Resolve and report without writing.
    #[serde(default)]
    pub dry_run: bool,
}

/// POST /api/data-mapping/mappings/{id}/run — resolve now (optionally dry) and record the run.
pub async fn run_mapping(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<RunRequest>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    let mapping = load_mapping(&state, tenant, id).await?;
    let outcome = runner::run(
        &state,
        &mapping,
        &req.row,
        req.event_id.as_deref(),
        req.source_row_id.as_deref(),
        req.dry_run,
    )
    .await?;
    if !req.dry_run {
        runner::record(
            &state,
            &mapping,
            &outcome,
            req.event_id.as_deref(),
            req.source_row_id.as_deref(),
        )
        .await?;
    }
    Ok(Json(json!({
        "status": outcome.status,
        "contact_id": outcome.contact_id,
        "refused": outcome.refused,
        "reason": outcome.reason,
        "payload": outcome.payload,
        "ms": outcome.ms
    })))
}

/// GET /api/data-mapping/mappings/{id}/runs — the last 50 runs, newest first.
pub async fn list_runs(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    let tenant = tenant_of(&claims)?;
    let rows: Vec<(
        Uuid,
        Option<String>,
        Option<String>,
        String,
        Option<Uuid>,
        Value,
        Option<String>,
        Option<i32>,
        Value,
        String,
    )> = sqlx::query_as(
        "SELECT id, event_id, source_row_id, status, contact_id, COALESCE(refused,'[]'::jsonb),
                    reason, ms, COALESCE(payload,'{}'::jsonb), created_at::text
               FROM integration_mapping_runs
              WHERE mapping_id = $1 AND tenant_id = $2
              ORDER BY created_at DESC LIMIT 50",
    )
    .bind(id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let runs: Vec<Value> = rows
        .into_iter()
        .map(
            |(
                rid,
                event_id,
                source_row_id,
                status,
                contact_id,
                refused,
                reason,
                ms,
                payload,
                created_at,
            )| {
                json!({
                    "id": rid, "event_id": event_id, "source_row_id": source_row_id,
                    "status": status, "contact_id": contact_id, "refused": refused,
                    "reason": reason, "ms": ms, "payload": payload, "created_at": created_at
                })
            },
        )
        .collect();
    Ok(Json(json!({ "runs": runs })))
}

/// POST /api/data-mapping/events — the intake a sibling app calls with its own API key.
///
/// Body: `{"app":"funnelswift","entity":"leads","event_id":"…","row":{…}}`.
/// Authenticated by the account's issued API key (the caller's tenant is the key's tenant — a caller
/// can never name another tenant).
pub async fn intake(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<Value> {
    // resolve_key returns (tenant_id, key_id) in that order — the tenant is what scopes this call.
    let (tenant, _key_id) = external_api::resolve_key(&state, &headers).await?;

    let app = body
        .get("app")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let entity = body
        .get("entity")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let event_id = body
        .get("event_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let source_row_id = body
        .get("source_row_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let row = body.get("row").cloned().unwrap_or(Value::Null);

    if app.is_empty() || entity.is_empty() {
        return Err(AppError::BadRequest(
            "This endpoint needs an app and an entity (e.g. funnelswift / leads).".into(),
        ));
    }
    if row.is_null() {
        return Err(AppError::BadRequest(
            "This endpoint needs the source row to map.".into(),
        ));
    }

    let mappings: Vec<Mapping> = sqlx::query_as(
        "SELECT id, tenant_id, name, source, destination, field_map, dedupe, options, enabled
           FROM integration_mappings
          WHERE tenant_id = $1 AND enabled
            AND source->>'app' = $2 AND source->>'entity' = $3",
    )
    .bind(tenant)
    .bind(&app)
    .bind(&entity)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let mut results = Vec::new();
    for m in mappings {
        let outcome = runner::run(
            &state,
            &m,
            &row,
            event_id.as_deref(),
            source_row_id.as_deref(),
            false,
        )
        .await?;
        runner::record(
            &state,
            &m,
            &outcome,
            event_id.as_deref(),
            source_row_id.as_deref(),
        )
        .await?;
        results.push(json!({
            "mapping": m.name, "mapping_id": m.id, "status": outcome.status,
            "contact_id": outcome.contact_id, "refused": outcome.refused, "reason": outcome.reason
        }));
    }

    Ok(Json(json!({
        "app": app, "entity": entity, "event_id": event_id,
        "mappings_matched": results.len(),
        "results": results
    })))
}

pub fn router(state: AppState) -> Router<AppState> {
    // The console routes are session-authenticated, exactly like every other `/api/*` module: the
    // auth middleware is what injects `Claims`, so without this layer a handler that reads the
    // signed-in account answers 500 instead of working.
    let console = Router::new()
        .route("/destinations", get(destinations))
        .route("/schema", get(source_schema))
        .route("/mappings", get(list_mappings).post(create_mapping))
        .route(
            "/mappings/:id",
            get(get_mapping).put(update_mapping).delete(delete_mapping),
        )
        .route("/mappings/:id/run", post(run_mapping))
        .route("/mappings/:id/runs", get(list_runs))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new()
        .merge(console)
        // Machine route: authenticated by the account's issued API key (route_policy API_KEY_ROUTES),
        // never by a session — a sibling app has no console login.
        .route("/events", post(intake))
        .route("/health", get(|| async { Json(json!({ "ok": true })) }))
}
