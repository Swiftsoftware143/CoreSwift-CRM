//! The REST surface (`/api/objects/**`), one handler per line of the Phase 1 spec.
//!
//! Every handler takes [`ObjCtx`], which the module's own middleware inserted after it decided which
//! account this request acts as. Nothing here reads a tenant id from a body, a query string or a
//! path — that is the single invariant the tenancy proof tests.

use axum::{
    extract::{Extension, Path, Query, State},
    Json,
};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use uuid::Uuid;

use super::store::{self, FieldPatch, FieldSpec, Query as RecQuery};
use super::ObjCtx;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

// ── body helpers ───────────────────────────────────────────────────────────────────────────────

/// Refuse a key this endpoint does not read. `serde` would drop it silently, and a caller who typed
/// `requried` would get a 201 and a field that is not required — the exact way a published field list
/// starts lying.
fn only_keys(m: &Map<String, Value>, allowed: &[&str]) -> Result<(), AppError> {
    for k in m.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(AppError::BadRequest(format!(
                "unknown parameter '{k}' — this call accepts: {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn body_object(v: Value) -> Result<Map<String, Value>, AppError> {
    v.as_object()
        .cloned()
        .ok_or_else(|| AppError::BadRequest("the request body must be a JSON object".into()))
}

fn req_str(m: &Map<String, Value>, k: &str) -> Result<String, AppError> {
    m.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::BadRequest(format!("'{k}' is required and must be a string")))
}

fn opt_str(m: &Map<String, Value>, k: &str) -> Result<Option<String>, AppError> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(AppError::BadRequest(format!("'{k}' must be a string"))),
    }
}

fn opt_bool(m: &Map<String, Value>, k: &str) -> Result<Option<bool>, AppError> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(AppError::BadRequest(format!("'{k}' must be true or false"))),
    }
}

fn opt_i32(m: &Map<String, Value>, k: &str) -> Result<Option<i32>, AppError> {
    match m.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => n
            .as_i64()
            .and_then(|v| i32::try_from(v).ok())
            .map(Some)
            .ok_or_else(|| AppError::BadRequest(format!("'{k}' must be a whole number"))),
        Some(_) => Err(AppError::BadRequest(format!(
            "'{k}' must be a whole number"
        ))),
    }
}

fn opt_value(m: &Map<String, Value>, k: &str) -> Option<Value> {
    match m.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.clone()),
    }
}

/// A record body is the record's OWN fields at the top level. Wrapping it in `{"data": {…}}` is the
/// most likely mistake (the MCP tool does take a `data` argument), so it gets its own sentence
/// instead of "unknown field 'data'".
fn record_body(v: Value, active: &[store::Field]) -> Result<Map<String, Value>, AppError> {
    let m = body_object(v)?;
    if m.len() == 1 && m.get("data").map(|d| d.is_object()).unwrap_or(false) {
        if !active.iter().any(|f| f.key == "data") {
            return Err(AppError::BadRequest(
                "send the record's own fields at the top level, e.g. {\"email\":\"a@b.c\"} — not \
                 wrapped in \"data\" (that wrapper is the MCP argument shape, not the REST body)"
                    .into(),
            ));
        }
    }
    Ok(m)
}

// ── objects ─────────────────────────────────────────────────────────────────────────────────────

pub async fn list_objects(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
) -> ApiResult<Json<Value>> {
    let objects = store::list_objects(&st.db, ctx.tenant_id).await?;
    Ok(Json(json!({ "objects": objects })))
}

pub async fn create_object(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let m = body_object(body)?;
    only_keys(&m, &["slug", "name"])?;
    let slug = req_str(&m, "slug")?;
    let name = req_str(&m, "name")?;
    let obj = store::create_object(&st.db, ctx.tenant_id, &ctx.actor, &slug, &name).await?;
    Ok(Json(json!({ "object": obj })))
}

pub async fn describe_object(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path(slug): Path<String>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let all = store::fields_for(&st.db, obj.id, true).await?;
    let (archived, active): (Vec<_>, Vec<_>) = all.iter().partition(|f| f.archived);
    let record_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM custom_records
          WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL",
    )
    .bind(ctx.tenant_id)
    .bind(obj.id)
    .fetch_one(&st.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(json!({
        "object": obj.json(),
        "fields": active.iter().map(|f| f.json()).collect::<Vec<_>>(),
        "archived_fields": archived.iter().map(|f| f.json()).collect::<Vec<_>>(),
        "record_count": record_count,
        // The two addresses an outside app needs, stated here so nobody has to guess them.
        "rest_base": format!("/api/objects/{}", obj.slug),
        "mcp": { "transport": "HTTP POST", "path": "/api/mcp", "auth": "Authorization: Bearer <workspace api key>" },
    })))
}

pub async fn add_field(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path(slug): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let m = body_object(body)?;
    only_keys(
        &m,
        &[
            "key", "label", "kind", "required", "unique", "options", "position",
        ],
    )?;
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let key = req_str(&m, "key")?;
    let spec = FieldSpec {
        label: opt_str(&m, "label")?.unwrap_or_else(|| key.clone()),
        key,
        kind: req_str(&m, "kind")?,
        required: opt_bool(&m, "required")?.unwrap_or(false),
        unique: opt_bool(&m, "unique")?.unwrap_or(false),
        options: opt_value(&m, "options").unwrap_or_else(|| json!([])),
        position: opt_i32(&m, "position")?.unwrap_or(0),
    };
    let field = store::add_field(&st.db, ctx.tenant_id, &ctx.actor, &obj, spec).await?;
    Ok(Json(json!({ "field": field })))
}

pub async fn patch_field(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path((slug, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let m = body_object(body)?;
    only_keys(
        &m,
        &["key", "label", "position", "required", "unique", "options"],
    )?;
    if m.is_empty() {
        return Err(AppError::BadRequest(
            "nothing to change — pass at least one of: key, label, position, required, unique, options"
                .into(),
        ));
    }
    let field_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest(format!("'{id}' is not a field id")))?;
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let p = FieldPatch {
        key: opt_str(&m, "key")?,
        label: opt_str(&m, "label")?,
        position: opt_i32(&m, "position")?,
        required: opt_bool(&m, "required")?,
        unique: opt_bool(&m, "unique")?,
        options: opt_value(&m, "options"),
    };
    let field = store::patch_field(&st.db, ctx.tenant_id, &ctx.actor, &obj, field_id, p).await?;
    Ok(Json(json!({ "field": field })))
}

pub async fn archive_field(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path((slug, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let out = store::archive_field(&st.db, ctx.tenant_id, &ctx.actor, &obj, &id).await?;
    Ok(Json(out))
}

pub async fn list_audit(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path(slug): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    for k in q.keys() {
        if k != "limit" {
            return Err(AppError::BadRequest(format!(
                "unknown query parameter '{k}' — this call accepts: limit"
            )));
        }
    }
    let (limit, _) = store::clamp_window(q.get("limit").map(|s| s.as_str()), None)?;
    // An unknown object is a 404; a known one with no audit rows answers an empty list.
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let rows = store::audit_rows(&st.db, ctx.tenant_id, &obj.slug, limit).await?;
    Ok(Json(json!({ "object": obj.slug, "audit": rows })))
}

// ── records ────────────────────────────────────────────────────────────────────────────────────

pub async fn query_records(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path(slug): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let active = store::fields_for(&st.db, obj.id, false).await?;

    let mut filters = Map::new();
    let mut sort = "created_at".to_string();
    for (k, v) in q.iter() {
        match k.as_str() {
            "limit" | "offset" => {}
            "sort" => sort = v.clone(),
            other => match other.strip_prefix("filter.") {
                Some(field) => {
                    filters.insert(field.to_string(), store::coerce_filter(&active, field, v)?);
                }
                None => {
                    return Err(AppError::BadRequest(format!(
                        "unknown query parameter '{other}' — use limit, offset, sort or \
                         filter.<field>=<value>"
                    )))
                }
            },
        }
    }
    let (limit, offset) = store::clamp_window(
        q.get("limit").map(|s| s.as_str()),
        q.get("offset").map(|s| s.as_str()),
    )?;

    let out = store::query_records(
        &st.db,
        ctx.tenant_id,
        &obj,
        RecQuery {
            limit,
            offset,
            sort,
            filters,
        },
    )
    .await?;
    Ok(Json(out))
}

pub async fn insert_record(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path(slug): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let active = store::fields_for(&st.db, obj.id, false).await?;
    let data = record_body(body, &active)?;
    let out = store::insert_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, data).await?;
    Ok(Json(out))
}

pub async fn update_record(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path((slug, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let active = store::fields_for(&st.db, obj.id, false).await?;
    let record_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest(format!("'{id}' is not a record id")))?;
    let patch = record_body(body, &active)?;
    if patch.is_empty() {
        return Err(AppError::BadRequest(
            "nothing to change — send at least one field".into(),
        ));
    }
    let out =
        store::update_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, record_id, patch).await?;
    Ok(Json(out))
}

pub async fn delete_record(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    Path((slug, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
    let record_id = Uuid::parse_str(&id)
        .map_err(|_| AppError::BadRequest(format!("'{id}' is not a record id")))?;
    let out = store::delete_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, record_id).await?;
    Ok(Json(out))
}
