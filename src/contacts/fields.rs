//! The contact custom-field manager: define a field once, fill it anywhere, show it only when it has a value.
//!
//! David, 2026-11-25: *"anywhere that there is a capture the user should be able to create a field … that is
//! now a new field that can be added to anybody's contact card. But contact cards only show cards that's
//! filled so that way we don't have these big contact cards … if a user opens a contact card and has to add
//! a field specifically there's a button to do it … first and last name at the top but the person's title
//! might be right under it even before phone number and email. And when creating a list you can add fields
//! too, so when a user comes in on that list that field is already there."*
//!
//! Why this module exists at all: the deals side has carried list/create/update/delete for custom fields for
//! months, and `contact_custom_fields` already held 33 definitions on ZaarHub — with no route that could
//! create, order or rename one. The tables were the engine; this is the steering wheel.
//!
//! Two rules worth stating because they are the ones that go wrong:
//!   * a value is validated against its field's TYPE, so a "number" field cannot quietly hold "TBC" and look
//!     filled on a card;
//!   * an empty value CLEARS the field — the card shows what is filled, so clearing must be a real operation,
//!     not a row left holding "".

use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The types a custom field may be. Kept in step with the deals manager so one vocabulary serves both.
const FIELD_TYPES: [&str; 7] = [
    "text", "number", "money", "date", "boolean", "url", "select",
];

#[derive(Debug, Deserialize)]
pub struct NewContactField {
    pub key: Option<String>,
    pub label: String,
    #[serde(default)]
    pub field_type: Option<String>,
    #[serde(default)]
    pub options: Option<Value>,
    #[serde(default)]
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContactField {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub field_type: Option<String>,
    #[serde(default)]
    pub options: Option<Value>,
    #[serde(default)]
    pub position: Option<i32>,
    #[serde(default)]
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SetContactFields {
    /// field key -> value. `null` or "" clears the field.
    pub values: std::collections::HashMap<String, Value>,
}

fn tenant_of(c: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)
}

/// A key derived from the label when none is given: lower, non-alphanumerics collapsed to one dash.
fn key_from(label: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Does this text make sense for that field type? Refused rather than stored: a card that says "not a number"
/// in a money field is worse than an empty one.
fn value_fits(field_type: &str, raw: &str) -> bool {
    match field_type {
        "number" | "money" => raw.parse::<f64>().is_ok(),
        "date" => chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").is_ok(),
        "boolean" => matches!(
            raw.to_ascii_lowercase().as_str(),
            "true" | "false" | "yes" | "no"
        ),
        "url" => raw.starts_with("https://") || raw.starts_with("http://"),
        _ => true,
    }
}

pub async fn list_fields(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    let rows: Vec<(Uuid, String, String, String, Value, i32, bool, String)> = sqlx::query_as(
        "SELECT id, key, label, field_type, options, position, is_active, source_app
           FROM contact_custom_fields WHERE tenant_id = $1 ORDER BY position, label",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        "fields": rows.into_iter().map(|(id, key, label, field_type, options, position, is_active, source_app)| json!({
            "id": id, "key": key, "label": label, "field_type": field_type, "options": options,
            "position": position, "is_active": is_active, "source_app": source_app,
        })).collect::<Vec<_>>()
    })))
}

pub async fn create_field(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<NewContactField>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    let label = req.label.trim();
    if label.is_empty() {
        return Err(AppError::Validation("label is required".into()));
    }
    let key = req
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| key_from(k))
        .unwrap_or_else(|| key_from(label));
    if key.is_empty() {
        return Err(AppError::Validation(
            "that label has no letters or numbers to make a field name from; send a key".into(),
        ));
    }
    let field_type = req
        .field_type
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("text");
    if !FIELD_TYPES.contains(&field_type) {
        return Err(AppError::Validation(format!(
            "\"{field_type}\" is not a field type; use one of: {}",
            FIELD_TYPES.join(", ")
        )));
    }
    // Defined from a capture screen or the console: `source_app` records which, so a field that appeared from
    // a booking form is distinguishable from one an operator typed in.
    let source = if c.role == "api_key" {
        "api"
    } else {
        "console"
    };
    let row: (Uuid, String, String, String, Value, i32, bool) = sqlx::query_as(
        "INSERT INTO contact_custom_fields (tenant_id, key, label, field_type, options, position, source_app)
         VALUES ($1,$2,$3,$4,$5,$6,$7)
         ON CONFLICT (tenant_id, key) DO UPDATE
           SET label = EXCLUDED.label, field_type = EXCLUDED.field_type,
               options = EXCLUDED.options, position = EXCLUDED.position,
               is_active = true, updated_at = now()
         RETURNING id, key, label, field_type, options, position, is_active",
    )
    .bind(tenant)
    .bind(&key)
    .bind(label)
    .bind(field_type)
    .bind(req.options.clone().unwrap_or_else(|| json!([])))
    .bind(req.position.unwrap_or(0))
    .bind(source)
    .fetch_one(&state.db)
    .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "id": row.0, "key": row.1, "label": row.2, "field_type": row.3,
            "options": row.4, "position": row.5, "is_active": row.6,
        })),
    ))
}

pub async fn update_field(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(field_id): Path<Uuid>,
    Json(req): Json<UpdateContactField>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    if let Some(t) = req.field_type.as_deref() {
        if !FIELD_TYPES.contains(&t) {
            return Err(AppError::Validation(format!(
                "\"{t}\" is not a field type; use one of: {}",
                FIELD_TYPES.join(", ")
            )));
        }
    }
    let row: Option<(Uuid, String, String, String, Value, i32, bool)> = sqlx::query_as(
        "UPDATE contact_custom_fields SET
            label = COALESCE($3, label),
            field_type = COALESCE($4, field_type),
            options = COALESCE($5, options),
            position = COALESCE($6, position),
            is_active = COALESCE($7, is_active),
            updated_at = now()
          WHERE id = $1 AND tenant_id = $2
          RETURNING id, key, label, field_type, options, position, is_active",
    )
    .bind(field_id)
    .bind(tenant)
    .bind(
        req.label
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty()),
    )
    .bind(req.field_type.as_deref())
    .bind(req.options.clone())
    .bind(req.position)
    .bind(req.is_active)
    .fetch_optional(&state.db)
    .await?;
    let row = row.ok_or_else(|| AppError::NotFound(format!("Field {field_id} not found")))?;
    Ok(Json(json!({
        "id": row.0, "key": row.1, "label": row.2, "field_type": row.3,
        "options": row.4, "position": row.5, "is_active": row.6,
    })))
}

/// Retire a field: deactivated, never deleted, because its values may already be on contacts and a card that
/// silently loses a column is worse than one that stops offering it.
pub async fn retire_field(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(field_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    let done = sqlx::query(
        "UPDATE contact_custom_fields SET is_active = false, updated_at = now()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(field_id)
    .bind(tenant)
    .execute(&state.db)
    .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Field {field_id} not found")));
    }
    Ok(Json(json!({ "retired": true, "id": field_id })))
}

/// One contact's custom fields, FILLED ONES ONLY, in the order the owner set.
pub async fn get_values(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(contact_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    let owns: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM contacts WHERE id = $1 AND tenant_id = $2")
            .bind(contact_id)
            .bind(tenant)
            .fetch_optional(&state.db)
            .await?;
    if owns.is_none() {
        return Err(AppError::NotFound(
            "that contact is not on this account".into(),
        ));
    }
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT f.key, f.label, f.field_type, v.value
           FROM contact_field_values v
           JOIN contact_custom_fields f ON f.id = v.field_id
          WHERE v.contact_id = $1 AND f.tenant_id = $2
            AND COALESCE(btrim(v.value), '') <> ''
          ORDER BY f.position, f.label",
    )
    .bind(contact_id)
    .bind(tenant)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(json!({
        // Only what is filled: the card renders this list and nothing else.
        "fields": rows.into_iter().map(|(key, label, field_type, value)| json!({
            "key": key, "label": label, "field_type": field_type, "value": value
        })).collect::<Vec<_>>()
    })))
}

/// Fill fields on one contact. A key that is not defined on this account is refused by name — an unknown key
/// would otherwise be a value nobody can ever read back.
pub async fn set_values(
    State(state): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(contact_id): Path<Uuid>,
    Json(req): Json<SetContactFields>,
) -> ApiResult<impl IntoResponse> {
    let tenant = tenant_of(&c)?;
    let owns: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM contacts WHERE id = $1 AND tenant_id = $2")
            .bind(contact_id)
            .bind(tenant)
            .fetch_optional(&state.db)
            .await?;
    if owns.is_none() {
        return Err(AppError::NotFound(
            "that contact is not on this account".into(),
        ));
    }

    let mut written = 0usize;
    let mut cleared = 0usize;
    for (key, value) in req.values {
        let field: Option<(Uuid, String, String)> = sqlx::query_as(
            "SELECT id, field_type, label FROM contact_custom_fields
              WHERE tenant_id = $1 AND key = $2 AND is_active",
        )
        .bind(tenant)
        .bind(&key)
        .fetch_optional(&state.db)
        .await?;
        let Some((field_id, field_type, label)) = field else {
            return Err(AppError::Validation(format!(
                "\"{key}\" is not a field on this account; create it first (POST contacts/custom-fields)"
            )));
        };
        let raw = match value {
            Value::Null => String::new(),
            Value::String(s) => s.trim().to_string(),
            other => other.to_string(),
        };
        if raw.is_empty() {
            sqlx::query("DELETE FROM contact_field_values WHERE contact_id = $1 AND field_id = $2")
                .bind(contact_id)
                .bind(field_id)
                .execute(&state.db)
                .await?;
            cleared += 1;
            continue;
        }
        if !value_fits(&field_type, &raw) {
            return Err(AppError::Validation(format!(
                "\"{label}\" expects a {field_type} value; \"{raw}\" is not one"
            )));
        }
        sqlx::query(
            "INSERT INTO contact_field_values (contact_id, field_id, value)
             VALUES ($1,$2,$3)
             ON CONFLICT (contact_id, field_id) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
        )
        .bind(contact_id)
        .bind(field_id)
        .bind(&raw)
        .execute(&state.db)
        .await?;
        written += 1;
    }
    Ok(Json(json!({ "written": written, "cleared": cleared })))
}
