//! External API — HUB CONTRACT for the lead-capture spokes. Read this before writing a spoke.
//!
//! CoreSwift is the CRM hub and the single home for every lead. Data flows DOWNWARD: a
//! capture app (spoke) pushes the leads it captured INTO this tenant's CoreSwift account.
//! There is no symmetric two-way requirement, and nothing here trusts the caller for
//! identity.
//!
//! ## Auth
//!     Authorization: Bearer csk_<hex>
//! The tenant mints the key in its own Integration Center: `POST /api/personal-api-keys`
//! (returns the secret once), `GET /api/personal-api-keys` (prefixes only),
//! `POST /api/personal-api-keys/:id/rotate`, `DELETE /api/personal-api-keys/:id` revokes.
//! The key hash resolves the tenant — a `tenant_id` / `aid` field in the body is IGNORED,
//! never trusted. Missing or unknown key → 401.
//!
//! ## Base URL (spoke resolution order — never hardcode-only)
//!   1. `provider_keys.base_url` for provider `coreswift` (tenant override)
//!   2. `integration_provider_presets.base_url` where key = 'coreswift'
//!   3. default `https://coreswiftcrm.com`
//!
//! ## Endpoints (mounted at /api/external)
//!     GET  /api/external/lists     → { "lists": [ {id, name, description, list_type, member_count, is_active} ] }
//!     POST /api/external/contacts  → 201 { id, source_app, list_assigned, tags_applied, auto_created_fields }
//!
//! ## POST /api/external/contacts — field mapping
//!   Built-in keys map straight to contact columns: email, phone, first_name, last_name,
//!   company, title / job_title, gender, city, state, country, postal_code,
//!   address_line1, address_line2, notes, source.
//!   Reserved keys, never stored as data points: `list_id` (uuid, optional),
//!   `tags` (array of strings), `fields` (object of extra data points), `source_app`.
//!   Every other key — top level or inside `fields` — is auto-provisioned as a per-tenant
//!   custom field (`contact_custom_fields`, stamped with the source app) and its value is
//!   stored on the contact.
//!
//! ## Source attribution (required — this is how the hub reports "leads by capture app")
//!   `source_app` must be the spoke's canonical slug:
//!     adaswift · funnelswift · incentiveswift · workflowswift · missedcallrespondr ·
//!     multidirectory · funnel-swift-mobile
//!   Aliases (`missedcall_responder`, `multi-directory`, `IncentiveSwift`, …) are
//!   normalized — see `crate::integration_center::normalize_source_app`.
//!   The hub writes `contacts.metadata.source_app` (first touch — preserved when the same
//!   lead is pushed again) and `contacts.metadata.last_source_app` (every push), plus
//!   `contacts.source = <slug>` on first arrival unless the body set an explicit `source`.
//!   Both the column and the metadata are therefore first-touch, so "leads by capture app"
//!   never moves a lead from the app that found it to the app that re-sent it. An
//!   unrecognized slug is kept verbatim rather than dropped, so it still shows in reporting.
//!
//! ## Idempotency & errors
//!   Upsert key: the `(tenant_id, email)` unique index — re-pushing a lead updates it.
//!   Without an email every push inserts. 400 when the body is not an object or carries
//!   none of first_name / email / phone. 401 on a missing or unknown key.
//!
//! ```text
//! curl -X POST https://coreswiftcrm.com/api/external/contacts \
//!   -H 'Authorization: Bearer csk_…' -H 'Content-Type: application/json' \
//!   -d '{"email":"lead@example.com","first_name":"Ada","source_app":"funnelswift",
//!        "list_id":"<optional uuid>","tags":["kinetic"],"fields":{"plan":"pro"}}'
//! ```

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

fn key_hash(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}
/// Coerce any JSON scalar (string/number/bool) to its string form.
/// Returns None for null/object/array (nothing to store as a text field value).
fn value_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Resolve a personal API key -> (tenant_id). Returns Unauthorized on failure.
async fn resolve_key(s: &AppState, headers: &HeaderMap) -> Result<(Uuid, Uuid), AppError> {
    let key = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or(AppError::Unauthorized)?;

    let hash = key_hash(&key);
    let row = sqlx::query(
        "SELECT id, tenant_id FROM personal_api_keys WHERE key_hash = $1 AND is_active = true",
    )
    .bind(&hash)
    .fetch_optional(&s.db)
    .await
    .map_err(AppError::Database)?
    .ok_or(AppError::Unauthorized)?;

    let key_id: Uuid = row.get("id");
    let tenant_id: Uuid = row.get("tenant_id");

    // Update last_used_at (best-effort)
    let _ = sqlx::query("UPDATE personal_api_keys SET last_used_at = NOW() WHERE id = $1")
        .bind(key_id)
        .execute(&s.db)
        .await;

    Ok((tenant_id, key_id))
}

/// Known built-in contact columns we can map directly (lowercase key -> column).
const BUILTIN_FIELDS: &[&str] = &[
    "email",
    "phone",
    "first_name",
    "last_name",
    "company",
    "title",
    "job_title",
    "gender",
    "city",
    "state",
    "country",
    "postal_code",
    "address_line1",
    "address_line2",
    "notes",
    "source",
];

fn is_builtin(key: &str) -> bool {
    BUILTIN_FIELDS.contains(&key)
}

/// Infer a custom-field type from a JSON value.
fn infer_type(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::String(s) => {
            // date heuristic
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

fn normalize_key(label: &str) -> String {
    let mut out = String::new();
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if ch == ' ' || ch == '-' {
            out.push('_');
        }
    }
    if out.is_empty() {
        "field".to_string()
    } else {
        out
    }
}

/// Auto-provision a custom field for a tenant if it doesn't exist; return its id.
async fn ensure_field(
    s: &AppState,
    tenant_id: Uuid,
    key: &str,
    label: &str,
    field_type: &str,
    source_app: &str,
) -> Result<Uuid, AppError> {
    // Try existing
    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM contact_custom_fields WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(key)
    .fetch_optional(&s.db)
    .await
    .map_err(AppError::Database)?;

    if let Some(id) = existing {
        return Ok(id);
    }

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO contact_custom_fields (id, tenant_id, key, label, field_type, source_app)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (tenant_id, key) DO NOTHING",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(key)
    .bind(label)
    .bind(field_type)
    .bind(source_app)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;

    // If a conflict happened (race), re-fetch
    let id = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM contact_custom_fields WHERE tenant_id = $1 AND key = $2",
    )
    .bind(tenant_id)
    .bind(key)
    .fetch_one(&s.db)
    .await
    .map_err(AppError::Database)?;

    Ok(id)
}

async fn upsert_contact(
    s: &AppState,
    tenant_id: Uuid,
    builtin: &std::collections::HashMap<String, String>,
    source_app: &str,
    source_raw: &str,
) -> Result<Uuid, AppError> {
    let email = builtin.get("email").cloned().filter(|e| !e.is_empty());
    let first_name = builtin.get("first_name").cloned().unwrap_or_default();
    let last_name = builtin.get("last_name").cloned().unwrap_or_default();
    let phone = builtin.get("phone").cloned();
    let company = builtin.get("company").cloned();
    let title = builtin
        .get("title")
        .cloned()
        .or_else(|| builtin.get("job_title").cloned());
    let city = builtin.get("city").cloned();
    let state = builtin.get("state").cloned();
    let country = builtin.get("country").cloned();
    let notes = builtin.get("notes").cloned();
    let source = builtin.get("source").cloned();

    if first_name.is_empty() && email.is_none() && phone.is_none() {
        return Err(AppError::BadRequest(
            "At least one of first_name, email, or phone is required".into(),
        ));
    }

    // Upsert by (tenant_id, email) when email present; otherwise insert new.
    if let Some(em) = &email {
        let existing = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2",
        )
        .bind(tenant_id)
        .bind(em)
        .fetch_optional(&s.db)
        .await
        .map_err(AppError::Database)?;

        if let Some(id) = existing {
            sqlx::query(
                "UPDATE contacts SET
                    first_name = COALESCE(NULLIF($3,''), first_name),
                    last_name  = COALESCE(NULLIF($4,''), last_name),
                    phone      = COALESCE($5, phone),
                    company    = COALESCE($6, company),
                    title      = COALESCE($7, title),
                    city       = COALESCE($8, city),
                    state      = COALESCE($9, state),
                    country    = COALESCE($10, country),
                    notes      = COALESCE($11, notes),
                    -- Attribution is first-touch on BOTH paths so the plain column and
                    -- the leads-by-capture-app rollup cannot disagree: the source app
                    -- that found the lead keeps it, and every later push only moves
                    -- metadata.last_source_app.
                    source     = COALESCE(NULLIF(source, ''), NULLIF($12, '')),
                    metadata   = COALESCE(metadata, '{}'::jsonb)
                                 || jsonb_build_object('last_source_app', $13::text, 'source_raw', $14::text)
                                 || CASE WHEN COALESCE(metadata, '{}'::jsonb) ? 'source_app'
                                         THEN '{}'::jsonb
                                         ELSE jsonb_build_object('source_app', $13::text) END,
                    updated_at = NOW()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(tenant_id)
            .bind(&first_name)
            .bind(&last_name)
            .bind(&phone)
            .bind(&company)
            .bind(&title)
            .bind(&city)
            .bind(&state)
            .bind(&country)
            .bind(&notes)
            .bind(&source)
            .bind(source_app)
            .bind(source_raw)
            .execute(&s.db)
            .await
            .map_err(AppError::Database)?;
            return Ok(id);
        }
    }

    let id = Uuid::new_v4();
    let attribution = json!({
        "source_app": source_app,
        "source_raw": source_raw,
        "received_via": "external_api",
    });
    sqlx::query(
        "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, company, title, city, state, country, notes, source, metadata)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&first_name)
    .bind(&last_name)
    .bind(&email)
    .bind(&phone)
    .bind(&company)
    .bind(&title)
    .bind(&city)
    .bind(&state)
    .bind(&country)
    .bind(&notes)
    .bind(&source)
    .bind(&attribution)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;

    Ok(id)
}

async fn assign_list_membership(
    s: &AppState,
    tenant_id: Uuid,
    contact_id: Uuid,
    list_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO list_members (id, list_id, contact_id, tenant_id, added_manually)
         VALUES ($1, $2, $3, $4, false)
         ON CONFLICT (list_id, contact_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(list_id)
    .bind(contact_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

async fn apply_tags(
    s: &AppState,
    tenant_id: Uuid,
    contact_id: Uuid,
    tags: &[String],
) -> Result<(), AppError> {
    for raw in tags {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        // Ensure tag exists
        let tag_id: Uuid = sqlx::query_scalar(
            "INSERT INTO tags (id, tenant_id, name)
             VALUES ($1, $2, $3)
             ON CONFLICT (tenant_id, name) DO UPDATE SET name = EXCLUDED.name
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(name)
        .fetch_one(&s.db)
        .await
        .map_err(AppError::Database)?;

        // Assign to contact. The statement is `ON CONFLICT DO NOTHING`, so re-pushing the same
        // lead for an already-tagged contact writes no row. `TagAdded` means a row came into
        // existence, therefore it is gated on `rows_affected()` — a no-op push must not re-fire
        // the tenant's automation (kanban t_56dddec2; same semantics as POST /api/tags/assign,
        // which answers 409 Duplicate on the repeat).
        let assigned = sqlx::query(
            "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id)
             VALUES ($1, $2, 'contact', $3, $4)
             ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(tag_id)
        .bind(contact_id)
        .bind(tenant_id)
        .execute(&s.db)
        .await
        .map_err(AppError::Database)?;
        if assigned.rows_affected() > 0 {
            // The hub's lead intake is a tenant-facing tag assignment like any other surface, so
            // it fans out. Before this line, a spoke could push a lead with `tags: ["hot"]` and
            // the tenant's `TagAdded` rule never fired — the same defect the webhook arm had.
            crate::automation::engine::fire_tag_trigger(
                &s.db, tenant_id, "contact", contact_id, tag_id, "TagAdded",
            )
            .await;
        }
    }
    Ok(())
}

async fn set_field_value(
    s: &AppState,
    contact_id: Uuid,
    field_id: Uuid,
    value: &str,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO contact_field_values (id, contact_id, field_id, value)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (contact_id, field_id) DO UPDATE SET value = EXCLUDED.value, updated_at = NOW()",
    )
    .bind(Uuid::new_v4())
    .bind(contact_id)
    .bind(field_id)
    .bind(value)
    .execute(&s.db)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

// ---------------- Handlers ----------------

/// GET /api/external/lists — list the tenant's lists.
pub async fn external_list_lists(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    let (tenant_id, _) = resolve_key(&s, &headers).await?;

    let rows = sqlx::query(
        "SELECT id, name, description, list_type, member_count, is_active
         FROM lists WHERE tenant_id = $1 ORDER BY name",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;

    let lists: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid,_>("id").to_string(),
                "name": r.get::<String,_>("name"),
                "description": r.get::<Option<String>,_>("description"),
                "list_type": r.get::<String,_>("list_type"),
                "member_count": r.get::<i32,_>("member_count"),
                "is_active": r.get::<bool,_>("is_active"),
            })
        })
        .collect();

    Ok(Json(json!({ "lists": lists })))
}

/// POST /api/external/contacts — upsert contact + list + tags + field mapping.
///
/// Body:
/// {
///   "first_name": "...", "last_name": "...", "email": "...", "phone": "...",
///   // ... any other built-in fields ...
///   "list_id": "uuid",           // optional — target list
///   "tags": ["a", "b"],          // optional
///   "fields": { "budget": "5000", "preferred_product": "Pro" },  // custom data points
///   "source_app": "incentiveswift"
/// }
pub async fn external_push_contact(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let (tenant_id, _) = resolve_key(&s, &headers).await?;
    let obj = body
        .as_object()
        .ok_or_else(|| AppError::BadRequest("body must be an object".into()))?;
    let source_raw = body
        .get("source_app")
        .and_then(|v| v.as_str())
        .unwrap_or("external")
        .trim()
        .to_string();
    // Attribution is the hub's job, not the caller's: canonicalize whatever the spoke
    // sent (aliases included) and keep the raw wire value for audit. Unrecognized slugs
    // are preserved rather than dropped, so they still appear in "leads by capture app".
    let source_app = crate::integration_center::normalize_source_app(&source_raw);

    // Split into built-in vs custom
    let mut builtin = std::collections::HashMap::new();
    let mut custom = std::collections::HashMap::new();
    for (k, v) in obj.iter() {
        // `tenant_id` / `aid` are explicitly IGNORED: the key resolves the tenant, and a
        // body field must never be able to steer a lead into another account. Ignoring it
        // (rather than storing it as a custom field) keeps that visible in the contract.
        if k == "list_id" || k == "tags" || k == "fields" || k == "source_app" {
            continue;
        }
        if k.eq_ignore_ascii_case("tenant_id") || k.eq_ignore_ascii_case("aid") {
            continue;
        }
        let norm = k.to_lowercase();
        if is_builtin(&norm) {
            if let Some(s) = value_to_string(v) {
                builtin.insert(norm.clone(), s);
            }
        } else if let Some(s) = value_to_string(v) {
            custom.insert(norm.clone(), s);
        }
    }

    // Also merge any fields under "fields"
    if let Some(fields) = body.get("fields").and_then(|v| v.as_object()) {
        for (k, v) in fields.iter() {
            let norm = k.to_lowercase();
            if is_builtin(&norm) {
                if let Some(s) = value_to_string(v) {
                    builtin.insert(norm.clone(), s);
                }
            } else if let Some(s) = value_to_string(v) {
                custom.insert(norm.clone(), s);
            }
        }
    }

    // The hub always attributes the lead: when the caller sent no explicit `source`, the
    // canonical capture-app slug becomes the contact's source, so both the plain column
    // (`contacts.source`) and `metadata->>'source_app'` report "leads by capture app".
    builtin
        .entry("source".to_string())
        .or_insert_with(|| source_app.clone());

    let contact_id = upsert_contact(&s, tenant_id, &builtin, &source_app, &source_raw).await?;

    // Optional list assignment
    if let Some(list_id) = body.get("list_id").and_then(|v| v.as_str()) {
        if let Ok(lid) = Uuid::parse_str(list_id) {
            assign_list_membership(&s, tenant_id, contact_id, lid).await?;
        }
    }

    // Optional tags
    if let Some(tags) = body.get("tags").and_then(|v| v.as_array()) {
        let tags: Vec<String> = tags
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        apply_tags(&s, tenant_id, contact_id, &tags).await?;
    }

    // Custom data points -> auto-provision fields + store values
    let mut created_fields: Vec<Value> = vec![];
    for (label, val) in custom.iter() {
        let key = normalize_key(label);
        let field_type = infer_type(&Value::String(val.clone()));
        let field_id = ensure_field(&s, tenant_id, &key, label, field_type, &source_app).await?;
        set_field_value(&s, contact_id, field_id, val).await?;
        created_fields.push(json!({ "key": key, "label": label, "type": field_type }));
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": contact_id.to_string(),
            "source_app": source_app,
            "source_raw": source_raw,
            "list_assigned": body.get("list_id").is_some(),
            "tags_applied": body.get("tags").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
            "auto_created_fields": created_fields,
        })),
    ))
}

pub fn router() -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/lists", get(external_list_lists))
        .route("/contacts", post(external_push_contact))
}
