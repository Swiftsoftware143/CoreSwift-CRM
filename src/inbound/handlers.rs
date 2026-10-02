//! Inbound webhook handlers — receive events from satellite apps via API key
//!
//! Satellite apps (FunnelSwift, IncentiveSwift, WorkflowSwift, MissedCall Respondr)
//! push data via these endpoints.
//!
//! Authentication (kanban t_588a15d1): the URL carries only `key_prefix`, which is an
//! INDEX — it is short and is documented as living in webhook URLs, so it is not a
//! credential. The credential is the full key, presented in the `X-Satellite-Key`
//! header; its SHA-256 hex must equal `satellite_api_keys.key_hash`. Before this
//! change the prefix alone authenticated the call and `key_hash` was read nowhere.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// SHA-256 hex of a presented key — the format `satellite_api_keys.key_hash` must carry.
/// Same construction as the personal API-key hasher (`src/personal_api_keys.rs`,
/// `src/external_api.rs`): key material is never stored, only this digest.
fn sha256_hex(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time byte equality — no early return on the first differing byte.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Authenticate an inbound satellite call -> `(api_key_id, tenant_id, key_name)`.
///
/// `key_prefix` selects candidate rows (index); the presented `X-Satellite-Key` must
/// hash to a row's `key_hash`. Every failure — missing/blank header, unknown prefix,
/// inactive row, hash mismatch, legacy row whose `key_hash` is not a SHA-256 hex —
/// answers the SAME 401, so the response cannot be used to probe which prefixes exist.
/// Fails closed: a row this server cannot verify authenticates nothing.
async fn authenticate_satellite_key(
    s: &AppState,
    headers: &HeaderMap,
    key_prefix: &str,
) -> ApiResult<(Uuid, Uuid, String)> {
    let presented = headers
        .get("x-satellite-key")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(AppError::Unauthorized)?;

    let rows = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
        r#"SELECT id, tenant_id, name, key_hash FROM satellite_api_keys
           WHERE key_prefix = $1 AND is_active = true"#,
    )
    .bind(key_prefix)
    .fetch_all(&s.db)
    .await?;

    let presented_hash = sha256_hex(presented);
    let matched = rows
        .iter()
        .find(|(_, _, _, key_hash)| ct_eq(presented_hash.as_bytes(), key_hash.as_bytes()))
        .ok_or(AppError::Unauthorized)?;

    let (key_id, tenant_id, key_name, _) = matched.clone();

    // Best-effort: the key card documents "Last used"; a failed timestamp update must
    // never refuse an otherwise valid delivery.
    let _ = sqlx::query("UPDATE satellite_api_keys SET last_used_at = NOW() WHERE id = $1")
        .bind(key_id)
        .execute(&s.db)
        .await;

    Ok((key_id, tenant_id, key_name))
}

/// POST /inbound/{key_prefix}/{event_type}
/// Receive an event from a satellite app; authenticated by `X-Satellite-Key`.
pub async fn receive(
    Path((key_prefix, event_type)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let (key_id, tenant_id, key_name) =
        authenticate_satellite_key(&state, &headers, &key_prefix).await?;

    // Record inbound event
    let event_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO inbound_webhook_events (id, tenant_id, source_app, event_type, event_payload, api_key_id, status)
           VALUES ($1, $2, $3, $4, $5, $6, 'received')
           RETURNING id"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&key_name)
    .bind(&event_type)
    .bind(&payload)
    .bind(key_id)
    .fetch_one(&state.db)
    .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "received",
            "event_id": event_id.to_string()
        })),
    ))
}

/// POST /inbound/v2/{key_prefix}/{event_type}
/// Alias/version for receive — same handler
pub async fn receive_v2(
    Path((key_prefix, event_type)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    receive(
        Path((key_prefix, event_type)),
        State(state),
        headers,
        Json(payload),
    )
    .await
}

/// POST /inbound/v3/{key_prefix}/contact-sync
/// Smart contact upsert with survey answers + auto-tagging + city-prefixed tags.
///
/// Expects JSON:
/// {
///   "email": "user@example.com",
///   "first_name": "John",
///   "last_name": "Doe",
///   "phone": "+1234567890",
///   "city": "Palm Bay",
///   "city_prefix": "PB",            // for tag naming
///   "state": "FL",
///   "source": "incentiveswift",
///   "survey_answers": {              // auto-stored in metadata
///     "What is your business?": "Plumber",
///     "How many employees?": "5"
///   },
///   "tags": ["lead", "survey_complete"],     // auto-prefixed: PB_lead, PB_survey_complete
///   "metadata": {}
/// }
pub async fn receive_v3_contact_sync(
    Path((key_prefix, event_type)): Path<(String, String)>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Validate event type
    if event_type != "contact-sync"
        && event_type != "survey-complete"
        && event_type != "contact-upsert"
    {
        return Err(AppError::BadRequest(format!(
            "Unsupported event type: {}. Use contact-sync, survey-complete, or contact-upsert",
            event_type
        )));
    }

    // Authenticate: the prefix selects the row, the X-Satellite-Key header is the credential
    let (key_id, tenant_id, key_name) =
        authenticate_satellite_key(&state, &headers, &key_prefix).await?;

    // Extract contact fields
    let email = payload.get("email").and_then(|v| v.as_str()).unwrap_or("");
    let first_name = payload
        .get("first_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let last_name = payload
        .get("last_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let phone = payload.get("phone").and_then(|v| v.as_str());
    let city = payload.get("city").and_then(|v| v.as_str()).unwrap_or("");
    let city_prefix = payload
        .get("city_prefix")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let us_state = payload.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let source = payload
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("inbound");
    let company = payload
        .get("company")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let title = payload.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let notes = payload.get("notes").and_then(|v| v.as_str());

    if email.is_empty() && first_name.is_empty() {
        return Err(AppError::BadRequest("email or first_name required".into()));
    }

    // Merge survey answers + existing metadata into metadata jsonb
    let survey_answers = payload.get("survey_answers").and_then(|v| v.as_object());
    let existing_meta = payload.get("metadata").and_then(|v| v.as_object());

    let mut metadata = serde_json::Map::new();

    // Copy existing metadata first
    if let Some(meta) = existing_meta {
        for (k, v) in meta {
            metadata.insert(k.clone(), v.clone());
        }
    }

    // Merge survey answers into metadata — field name = question, value = answer
    if let Some(answers) = survey_answers {
        for (question, answer) in answers {
            metadata.insert(question.clone(), answer.clone());
        }
    }

    // Add source tracking
    metadata.insert("_source".to_string(), json!(source));
    metadata.insert(
        "_synced_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    if !city.is_empty() {
        metadata.insert("_city".to_string(), json!(city));
    }
    if !city_prefix.is_empty() {
        metadata.insert("_city_prefix".to_string(), json!(city_prefix));
    }

    let metadata_value = Value::Object(metadata);

    // Upsert contact by email — COALESCE merge keeps existing values
    let contact_id = if !email.is_empty() {
        // Check for existing contact by email
        let existing = sqlx::query_as::<_, (Uuid,)>(
            "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
        )
        .bind(tenant_id)
        .bind(email)
        .fetch_optional(&state.db)
        .await?;

        if let Some((existing_id,)) = existing {
            // UPDATE — merge fields
            sqlx::query(
                r#"UPDATE contacts SET
                    phone = COALESCE($1, phone),
                    first_name = COALESCE($2, first_name),
                    last_name = COALESCE($3, last_name),
                    title = COALESCE($4, title),
                    city = COALESCE($5, city),
                    state = COALESCE($6, state),
                    company = COALESCE($7, company),
                    source = CASE WHEN $8::text IS NOT NULL AND $8::text != '' THEN $8 ELSE source END,
                    metadata = metadata || $9::jsonb,
                    notes = COALESCE($10, notes),
                    updated_at = NOW()
                   WHERE id = $11 AND tenant_id = $12"#,
            )
            .bind(phone)
            .bind(first_name)
            .bind(last_name)
            .bind(if title.is_empty() { None } else { Some(title) })
            .bind(if city.is_empty() { None } else { Some(city) })
            .bind(if us_state.is_empty() { None } else { Some(&us_state) })
            .bind(if company.is_empty() { None } else { Some(company) })
            .bind(if source.is_empty() { None } else { Some(source) })
            .bind(&metadata_value)
            .bind(notes)
            .bind(existing_id)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;

            existing_id
        } else {
            // INSERT
            let new_id = Uuid::new_v4();
            sqlx::query(
                r#"INSERT INTO contacts (id, tenant_id, email, phone, first_name, last_name, title, city, state, company, source, metadata, notes)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
            )
            .bind(new_id)
            .bind(tenant_id)
            .bind(email)
            .bind(phone)
            .bind(first_name)
            .bind(last_name)
            .bind(if title.is_empty() { None } else { Some(title) })
            .bind(if city.is_empty() { None } else { Some(city) })
            .bind(if us_state.is_empty() { None } else { Some(&us_state) })
            .bind(if company.is_empty() { None } else { Some(company) })
            .bind(source)
            .bind(&metadata_value)
            .bind(notes)
            .execute(&state.db)
            .await?;

            new_id
        }
    } else {
        // No email — insert by name only
        let new_id = Uuid::new_v4();
        sqlx::query(
            r#"INSERT INTO contacts (id, tenant_id, phone, first_name, last_name, title, city, state, company, source, metadata, notes)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)"#,
        )
        .bind(new_id)
        .bind(tenant_id)
        .bind(phone)
        .bind(first_name)
        .bind(last_name)
        .bind(if title.is_empty() { None } else { Some(title) })
        .bind(if city.is_empty() { None } else { Some(city) })
        .bind(if us_state.is_empty() { None } else { Some(&us_state) })
        .bind(if company.is_empty() { None } else { Some(company) })
        .bind(source)
        .bind(&metadata_value)
        .bind(notes)
        .execute(&state.db)
        .await?;

        new_id
    };

    // Process tags — auto-prefix with city prefix if provided
    let mut assigned_tags: Vec<Value> = Vec::new();
    if let Some(tags) = payload.get("tags").and_then(|v| v.as_array()) {
        for tag_name_val in tags {
            let raw_name = tag_name_val.as_str().unwrap_or("");
            if raw_name.is_empty() {
                continue;
            }

            // Apply city prefix if provided and not already prefixed
            let final_tag_name =
                if !city_prefix.is_empty() && !raw_name.starts_with(&format!("{}_", city_prefix)) {
                    format!("{}_{}", city_prefix, raw_name)
                } else {
                    raw_name.to_string()
                };

            // Create or get tag
            let tag_id = create_tag_or_get_id(&state.db, tenant_id, &final_tag_name).await?;

            // Assign tag to contact (skip if already assigned). `TagAdded` <=> a row came into
            // existence, so the fan-out is gated on `rows_affected()`: the statement is
            // `ON CONFLICT DO NOTHING` and a repeat sync must not re-fire the tenant's rule
            // (kanban t_56dddec2).
            let assigned = sqlx::query(
                "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id, assigned_by)
                 VALUES ($1, $2, 'contact', $3, $4, NULL)
                 ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"
            )
            .bind(Uuid::new_v4())
            .bind(tag_id)
            .bind(contact_id)
            .bind(tenant_id)
            .execute(&state.db)
            .await?;
            if assigned.rows_affected() > 0 {
                // A satellite app that captures a lead already carrying tags is assigning them to
                // this tenant's contact, so the tenant's `TagAdded` automation must see it.
                crate::automation::engine::fire_tag_trigger(
                    &state.db, tenant_id, "contact", contact_id, tag_id, "TagAdded",
                )
                .await;
            }

            assigned_tags.push(json!({
                "name": final_tag_name,
                "tag_id": tag_id.to_string()
            }));
        }
    }

    // Record inbound event for audit trail
    let event_id = sqlx::query_scalar::<_, Uuid>(
        r#"INSERT INTO inbound_webhook_events (id, tenant_id, source_app, event_type, event_payload, api_key_id, status)
           VALUES ($1, $2, $3, $4, $5, $6, 'processed')
           RETURNING id"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&key_name)
    .bind(&event_type)
    .bind(&payload)
    .bind(key_id)
    .fetch_one(&state.db)
    .await?;

    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "synced",
            "event_id": event_id.to_string(),
            "contact_id": contact_id.to_string(),
            "merged": true,
            "tags_assigned": assigned_tags.len(),
            "tags": assigned_tags,
            "survey_answers_stored": survey_answers.map(|a| a.len()).unwrap_or(0),
        })),
    ))
}

/// Helper: create tag if it doesn't exist, return its ID
async fn create_tag_or_get_id(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    name: &str,
) -> Result<Uuid, AppError> {
    // Check if tag already exists
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM tags WHERE tenant_id = $1 AND name = $2")
            .bind(tenant_id)
            .bind(name)
            .fetch_optional(db)
            .await?;

    if let Some((tag_id,)) = existing {
        return Ok(tag_id);
    }

    // Create new tag
    let tag_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tags (id, tenant_id, name, color, is_active)
         VALUES ($1, $2, $3, '#6366f1', true)
         ON CONFLICT (tenant_id, name) DO UPDATE SET updated_at = NOW() RETURNING id",
    )
    .bind(tag_id)
    .bind(tenant_id)
    .bind(name)
    .execute(db)
    .await?;

    Ok(tag_id)
}
