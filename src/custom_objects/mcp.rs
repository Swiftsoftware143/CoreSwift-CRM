//! The MCP door (`POST /api/mcp`), JSON-RPC 2.0 over HTTP — the surface an MCP-capable editor
//! (Cursor, Claude, Bolt during the transition) speaks.
//!
//! The token IS the tenancy boundary, exactly as Engine's per-workspace MCP does it: no tool takes a
//! tenant or workspace argument, so an agent cannot name another account even by mistake. A tool that
//! is handed an id belonging to another account answers 404 — never 403, because a refusal must not
//! confirm that the row exists.
//!
//! The tools are thin: every one of them calls the SAME `store` functions the REST surface calls, so
//! validation, uniqueness and audit cannot drift between the two doors.

use axum::{
    extract::{Extension, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::store::{self, FieldSpec, Query as RecQuery};
use super::ObjCtx;
use crate::errors::AppError;
use crate::AppState;

/// The tool set, in the order the spec lists it. 11 tools: objects, fields and records only — no
/// tool names the ACCOUNT, so no API key can reach account-level erasure (pinned by a route_policy
/// test; `archive_object` archives a shape, it never deletes a tenant).
///
/// `set_unique_together` and `insert_record`'s `upsert_on` are the composite-key pair from migration
/// 208: a producer that must be able to retry a write names the object's key combination once, then
/// replays safely.
pub const TOOL_NAMES: [&str; 11] = [
    "list_objects",
    "create_object",
    "describe_object",
    "archive_object",
    "add_field",
    "archive_field",
    "insert_record",
    "set_unique_together",
    "query_records",
    "update_record",
    "delete_record",
];

fn s(t: &str) -> Value {
    json!({ "type": t })
}

fn tools() -> Value {
    json!([
        {
            "name": "list_objects",
            "description": "Every custom object in this workspace, with its field and record counts.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "create_object",
            "description": "Create a data object (a table). slug is the URL segment: lowercase letters, digits and underscores, starting with a letter.",
            "inputSchema": {
                "type": "object",
                "properties": { "slug": s("string"), "name": s("string") },
                "required": ["slug", "name"]
            }
        },
        {
            "name": "describe_object",
            "description": "One object's fields (live and archived), its record count, and the REST base address for it.",
            "inputSchema": {
                "type": "object",
                "properties": { "object": s("string") },
                "required": ["object"]
            }
        },
        {
            "name": "archive_object",
            "description": "Archive an object (the only way to remove one). Its fields are archived with it, it stops appearing in list_objects/describe_object, and everything under it answers 404. The records are NOT deleted — they stay in the database, readable by id. Idempotent: calling it twice is safe.",
            "inputSchema": {
                "type": "object",
                "properties": { "object": s("string") },
                "required": ["object"]
            }
        },
        {
            "name": "add_field",
            "description": "Add a field (column) to an object. kind is text | number | bool | date | enum | json. An enum needs options (array of strings). required and unique are enforced on every write.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": s("string"), "key": s("string"), "label": s("string"),
                    "kind": s("string"), "required": { "type": "boolean" },
                    "unique": { "type": "boolean" }, "options": { "type": "array" },
                    "position": { "type": "integer" }
                },
                "required": ["object", "key", "kind"]
            }
        },
        {
            "name": "archive_field",
            "description": "Archive a field by key or id. The rows keep their values; the field just stops accepting writes. Re-adding the same key revives it.",
            "inputSchema": {
                "type": "object",
                "properties": { "object": s("string"), "key": s("string"), "field_id": s("string") }
            }
        },
        {
            "name": "insert_record",
            "description": "Insert one record. data is an object of field key -> value; unknown keys and wrong kinds are refused and nothing is written. Pass upsert_on (the object's unique_together keys) to make the write replay-safe: a second call with the same key values updates that row and merges data into it instead of adding a duplicate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": s("string"),
                    "data": s("object"),
                    "upsert_on": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["object", "data"]
            }
        },
        {
            "name": "set_unique_together",
            "description": "Set (or clear) the object's composite key: the combination of field values that must be unique among live records, e.g. [\"booking_ref\",\"position\"]. It is refused, with the colliding values and row ids named, while live records already collide; clearing it with [] removes the constraint. This is also what insert_record's upsert_on must name.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": s("string"),
                    "fields": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["object", "fields"]
            }
        },
        {
            "name": "query_records",
            "description": "Read records. sort is created_at | updated_at | id | a field key, optionally prefixed with '-' for descending. filter is an object of field key -> value (exact match). limit defaults to 50 (max 200).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": s("string"), "limit": { "type": "integer" },
                    "offset": { "type": "integer" }, "sort": s("string"),
                    "filter": s("object")
                },
                "required": ["object"]
            }
        },
        {
            "name": "update_record",
            "description": "Merge fields into one record by id. Only the keys you send change.",
            "inputSchema": {
                "type": "object",
                "properties": { "object": s("string"), "id": s("string"), "data": s("object") },
                "required": ["object", "id", "data"]
            }
        },
        {
            "name": "delete_record",
            "description": "Delete one record by id. By default this is a SOFT delete: the row stays with deleted_at set. Pass hard:true to actually delete the row and its data (the audit trail keeps the id, never the data).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "object": s("string"), "id": s("string"),
                    "hard": { "type": "boolean" }
                },
                "required": ["object", "id"]
            }
        },
    ])
}

fn rpc_ok(id: Value, result: Value) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })),
    )
        .into_response()
}

fn rpc_err(id: Value, code: i64, message: &str) -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })),
    )
        .into_response()
}

/// A served GET on the same path: what this endpoint is, for a human who curls it. No credential
/// reaches this body — the auth layer has already decided the caller, and only the account is echoed.
pub async fn info(Extension(ctx): Extension<ObjCtx>) -> Response {
    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(json!({
            "service": "coreswift-custom-objects",
            "protocol": "json-rpc-2.0",
            "transport": "HTTP POST",
            "account": ctx.tenant_id,
            "tools": TOOL_NAMES,
        })),
    )
        .into_response()
}

pub async fn handle(
    State(st): State<AppState>,
    Extension(ctx): Extension<ObjCtx>,
    body: axum::body::Bytes,
) -> Response {
    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_err(Value::Null, -32700, "parse error"),
    };
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");

    match method {
        "initialize" => rpc_ok(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "coreswift-custom-objects",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        ),
        "notifications/initialized" => (StatusCode::ACCEPTED, ()).into_response(),
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({ "tools": tools() })),
        "tools/call" => {
            let name = req
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let args = req
                .get("params")
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            if !TOOL_NAMES.contains(&name.as_str()) {
                return rpc_err(id, -32601, &format!("unknown tool: {name}"));
            }
            match call_tool(&st, &ctx, &name, &args).await {
                Ok(result) => rpc_ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": result.to_string() }],
                        "isError": false
                    }),
                ),
                Err(e) => rpc_ok(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": format!("{e}") }],
                        "isError": true,
                        "status": status_of(&e),
                    }),
                ),
            }
        }
        other => rpc_err(id, -32601, &format!("method not found: {other}")),
    }
}

/// The HTTP status the same refusal would carry on the REST surface, so an agent can tell "you sent
/// the wrong thing" from "that row is not yours" without parsing prose.
fn status_of(e: &AppError) -> u16 {
    match e {
        AppError::Unauthorized | AppError::InvalidCredentials | AppError::UnauthorizedReason(_) => {
            401
        }
        AppError::Forbidden | AppError::ForbiddenReason(_) => 403,
        AppError::NotFound(_) => 404,
        AppError::Duplicate(_) | AppError::Conflict(_) => 409,
        AppError::Validation(_) => 422,
        AppError::BadRequest(_) => 400,
        AppError::RateLimited(_) => 429,
        AppError::UpgradeRequired(_) => 402,
        _ => 500,
    }
}

// ── argument helpers ───────────────────────────────────────────────────────────────────────────

/// Refuse an argument this tool does not read: an agent that typed `recrod` must be told, not handed
/// a 200 for a different operation than it asked for.
fn only_args(a: &Map<String, Value>, allowed: &[&str]) -> Result<(), AppError> {
    for k in a.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(AppError::BadRequest(format!(
                "unknown argument '{k}' — this tool accepts: {}",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn args_object(a: &Value, tool: &str) -> Result<Map<String, Value>, AppError> {
    a.as_object()
        .cloned()
        .ok_or_else(|| AppError::BadRequest(format!("{tool}: arguments must be a JSON object")))
}

fn arg_str(a: &Map<String, Value>, k: &str) -> Result<String, AppError> {
    a.get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::BadRequest(format!("'{k}' is required and must be a non-empty string"))
        })
}

fn arg_opt_str(a: &Map<String, Value>, k: &str) -> Result<Option<String>, AppError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(AppError::BadRequest(format!("'{k}' must be a string"))),
    }
}

fn arg_opt_bool(a: &Map<String, Value>, k: &str) -> Result<Option<bool>, AppError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(AppError::BadRequest(format!("'{k}' must be true or false"))),
    }
}

fn arg_opt_i32(a: &Map<String, Value>, k: &str) -> Result<Option<i32>, AppError> {
    match a.get(k) {
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

fn arg_window(a: &Map<String, Value>) -> Result<(i64, i64), AppError> {
    let limit = a.get("limit").and_then(|v| v.as_i64()).unwrap_or(50);
    let offset = a.get("offset").and_then(|v| v.as_i64()).unwrap_or(0);
    if limit < 1 {
        return Err(AppError::BadRequest("limit must be at least 1".into()));
    }
    if offset < 0 {
        return Err(AppError::BadRequest("offset cannot be negative".into()));
    }
    Ok((limit.min(200), offset))
}

async fn call_tool(
    st: &AppState,
    ctx: &ObjCtx,
    name: &str,
    args: &Value,
) -> Result<Value, AppError> {
    let a = args_object(args, name)?;

    match name {
        "list_objects" => {
            only_args(&a, &[])?;
            Ok(json!({ "objects": store::list_objects(&st.db, ctx.tenant_id).await? }))
        }
        "create_object" => {
            only_args(&a, &["slug", "name"])?;
            let slug = arg_str(&a, "slug")?;
            let label = arg_str(&a, "name")?;
            let obj =
                store::create_object(&st.db, ctx.tenant_id, &ctx.actor, &slug, &label).await?;
            Ok(json!({ "object": obj }))
        }
        "describe_object" => {
            only_args(&a, &["object"])?;
            let slug = arg_str(&a, "object")?;
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            let all = store::fields_for(&st.db, obj.id, true).await?;
            let (archived, active): (Vec<_>, Vec<_>) = all.iter().partition(|f| f.archived);
            Ok(json!({
                "object": obj.json(),
                "fields": active.iter().map(|f| f.json()).collect::<Vec<_>>(),
                "archived_fields": archived.iter().map(|f| f.json()).collect::<Vec<_>>(),
                "rest_base": format!("/api/objects/{}", obj.slug),
            }))
        }
        "archive_object" => {
            only_args(&a, &["object"])?;
            let slug = arg_str(&a, "object")?;
            store::archive_object(&st.db, ctx.tenant_id, &ctx.actor, &slug).await
        }
        "add_field" => {
            only_args(
                &a,
                &[
                    "object", "key", "label", "kind", "required", "unique", "options", "position",
                ],
            )?;
            let slug = arg_str(&a, "object")?;
            let key = arg_str(&a, "key")?;
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            let spec = FieldSpec {
                label: arg_opt_str(&a, "label")?.unwrap_or_else(|| key.clone()),
                key,
                kind: arg_str(&a, "kind")?,
                required: arg_opt_bool(&a, "required")?.unwrap_or(false),
                unique: arg_opt_bool(&a, "unique")?.unwrap_or(false),
                options: a.get("options").cloned().unwrap_or_else(|| json!([])),
                position: arg_opt_i32(&a, "position")?.unwrap_or(0),
            };
            let field = store::add_field(&st.db, ctx.tenant_id, &ctx.actor, &obj, spec).await?;
            Ok(json!({ "field": field }))
        }
        "archive_field" => {
            only_args(&a, &["object", "key", "field_id"])?;
            let slug = arg_str(&a, "object")?;
            let which = match (arg_opt_str(&a, "key")?, arg_opt_str(&a, "field_id")?) {
                (Some(k), None) => k,
                (None, Some(id)) => id,
                (Some(_), Some(_)) => {
                    return Err(AppError::BadRequest(
                        "pass either 'key' or 'field_id', not both".into(),
                    ))
                }
                (None, None) => {
                    return Err(AppError::BadRequest(
                        "archive_field needs 'key' (or 'field_id')".into(),
                    ))
                }
            };
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            store::archive_field(&st.db, ctx.tenant_id, &ctx.actor, &obj, &which).await
        }
        "insert_record" => {
            only_args(&a, &["object", "data", "upsert_on"])?;
            let slug = arg_str(&a, "object")?;
            let data = a
                .get("data")
                .and_then(|v| v.as_object())
                .cloned()
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "'data' is required and must be an object of field key -> value".into(),
                    )
                })?;
            let upsert_on = match a.get("upsert_on") {
                None | Some(Value::Null) => None,
                Some(v) => {
                    let list = v.as_array().ok_or_else(|| {
                        AppError::BadRequest(
                            "'upsert_on' must be an array of field keys, e.g. [\"booking_ref\",\"position\"]"
                                .into(),
                        )
                    })?;
                    let mut keys = Vec::with_capacity(list.len());
                    for item in list {
                        let Some(k) = item.as_str() else {
                            return Err(AppError::BadRequest(
                                "'upsert_on' must contain field keys (strings)".into(),
                            ));
                        };
                        keys.push(k.to_string());
                    }
                    if keys.is_empty() {
                        return Err(AppError::BadRequest(
                            "'upsert_on' needs at least one field key".into(),
                        ));
                    }
                    Some(keys)
                }
            };
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            store::insert_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, data, upsert_on).await
        }
        "set_unique_together" => {
            only_args(&a, &["object", "fields"])?;
            let slug = arg_str(&a, "object")?;
            let list = a.get("fields").and_then(|v| v.as_array()).ok_or_else(|| {
                AppError::BadRequest(
                    "'fields' is required and must be an array of field keys ([] clears the key)"
                        .into(),
                )
            })?;
            let mut keys = Vec::with_capacity(list.len());
            for item in list {
                let Some(k) = item.as_str() else {
                    return Err(AppError::BadRequest(
                        "'fields' must contain field keys (strings)".into(),
                    ));
                };
                keys.push(k.to_string());
            }
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            store::set_unique_together(&st.db, ctx.tenant_id, &ctx.actor, &obj, keys).await
        }
        "query_records" => {
            only_args(&a, &["object", "limit", "offset", "sort", "filter"])?;
            let slug = arg_str(&a, "object")?;
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            let active = store::fields_for(&st.db, obj.id, false).await?;
            let mut filters = Map::new();
            if let Some(f) = a.get("filter") {
                let f = f.as_object().ok_or_else(|| {
                    AppError::BadRequest("'filter' must be an object of field key -> value".into())
                })?;
                for (k, v) in f {
                    if !active.iter().any(|x| &x.key == k) {
                        return Err(AppError::BadRequest(format!(
                            "cannot filter on '{k}' — it is not a field of this object"
                        )));
                    }
                    filters.insert(k.clone(), v.clone());
                }
            }
            let (limit, offset) = arg_window(&a)?;
            let sort = arg_opt_str(&a, "sort")?.unwrap_or_else(|| "created_at".into());
            store::query_records(
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
            .await
        }
        "update_record" => {
            only_args(&a, &["object", "id", "data"])?;
            let slug = arg_str(&a, "object")?;
            let raw_id = arg_str(&a, "id")?;
            let id = Uuid::parse_str(&raw_id)
                .map_err(|_| AppError::BadRequest(format!("'{raw_id}' is not a record id")))?;
            let patch = a
                .get("data")
                .and_then(|v| v.as_object())
                .cloned()
                .filter(|m| !m.is_empty())
                .ok_or_else(|| {
                    AppError::BadRequest(
                        "'data' is required and must be a non-empty object of field key -> value"
                            .into(),
                    )
                })?;
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            store::update_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, id, patch).await
        }
        "delete_record" => {
            only_args(&a, &["object", "id", "hard"])?;
            let slug = arg_str(&a, "object")?;
            let raw_id = arg_str(&a, "id")?;
            let id = Uuid::parse_str(&raw_id)
                .map_err(|_| AppError::BadRequest(format!("'{raw_id}' is not a record id")))?;
            let hard = arg_opt_bool(&a, "hard")?.unwrap_or(false);
            let obj = store::object_by_slug(&st.db, ctx.tenant_id, &slug).await?;
            store::delete_record(&st.db, ctx.tenant_id, &ctx.actor, &obj, id, hard).await
        }
        other => Err(AppError::BadRequest(format!("unknown tool: {other}"))),
    }
}

/// The router mounted at `/api/mcp`. Kept as its own `router` function so the route census in
/// `auth::route_policy` can read its literals (an inline `Router::new()` there would be invisible).
pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::middleware;
    axum::Router::new()
        .route("/", axum::routing::get(info))
        .route("/", axum::routing::post(handle))
        .layer(middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            super::authenticate,
        ))
}
