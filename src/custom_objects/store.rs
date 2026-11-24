//! Custom Objects — the storage layer, shared by the REST surface and the MCP tools.
//!
//! One rule holds the whole module together: **`tenant_id` comes from the credential, never from the
//! body.** Every function here takes the tenant as an argument that the caller resolved from the
//! session or the API key, and every lookup binds it. That is why a foreign object id answers 404
//! rather than 403 — the refusal must not confirm that another account's row exists.
//!
//! The two surfaces exist deliberately: an agent speaks MCP, an application speaks REST, and both
//! must land on the same rows through the same validation. Nothing in this file knows which one
//! called it.

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Map, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use crate::errors::AppError;

/// The kinds a field may declare. The same list is a CHECK constraint in migration 204 — the Rust
/// copy is what produces a 400 sentence instead of a 500 constraint violation.
pub const KINDS: [&str; 6] = ["text", "number", "bool", "date", "enum", "json"];

/// The slug / key alphabet, mirrored from the CHECK constraints so a bad value is refused with a
/// sentence that names the character, not with a database error.
fn shape_ok(v: &str) -> bool {
    let b = v.as_bytes();
    if b.is_empty() || b.len() > 63 {
        return false;
    }
    if !b[0].is_ascii_lowercase() {
        return false;
    }
    b.iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

pub fn check_slug(slug: &str) -> Result<(), AppError> {
    if shape_ok(slug) {
        return Ok(());
    }
    Err(AppError::BadRequest(format!(
        "slug '{slug}' is not usable — use lowercase letters, digits and underscores, starting with a \
         letter (max 63 characters), e.g. 'customer_orders'"
    )))
}

/// Map a unique-index violation to the app's duplicate error, everything else to a database error.
fn map_db(e: sqlx::Error, duplicate: &str) -> AppError {
    if let sqlx::Error::Database(db) = &e {
        if db.code().as_deref() == Some("23505") {
            return AppError::Duplicate(duplicate.to_string());
        }
    }
    AppError::Database(e)
}

// ── rows ─────────────────────────────────────────────────────────────────────────────────────────

pub struct Obj {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct Field {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
    pub unique_value: bool,
    pub options: Value,
    pub position: i32,
    pub archived: bool,
}

impl Field {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "key": self.key,
            "label": self.label,
            "kind": self.kind,
            "required": self.required,
            "unique": self.unique_value,
            "options": self.options,
            "position": self.position,
            "archived": self.archived,
        })
    }
}

impl Obj {
    pub fn json(&self) -> Value {
        json!({
            "id": self.id,
            "slug": self.slug,
            "name": self.name,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

fn field_from(row: &sqlx::postgres::PgRow) -> Field {
    let archived_at: Option<DateTime<Utc>> = row.get("archived_at");
    Field {
        id: row.get("id"),
        key: row.get("key"),
        label: row.get("label"),
        kind: row.get("kind"),
        required: row.get("required"),
        unique_value: row.get("unique_value"),
        options: row.get("options"),
        position: row.get("position"),
        archived: archived_at.is_some(),
    }
}

// ── objects ─────────────────────────────────────────────────────────────────────────────────────

pub async fn object_by_slug(db: &PgPool, tenant: Uuid, slug: &str) -> Result<Obj, AppError> {
    let row = sqlx::query(
        "SELECT id, slug, name, created_at, updated_at FROM custom_objects
         WHERE tenant_id = $1 AND slug = $2",
    )
    .bind(tenant)
    .bind(slug)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?
    // 404, not 403: a slug that belongs to another account must be indistinguishable from one that
    // was never created.
    .ok_or_else(|| AppError::NotFound(format!("no object '{slug}' in this workspace")))?;

    Ok(Obj {
        id: row.get("id"),
        slug: row.get("slug"),
        name: row.get("name"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

pub async fn list_objects(db: &PgPool, tenant: Uuid) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT o.id, o.slug, o.name, o.created_at, o.updated_at,
                (SELECT COUNT(*) FROM custom_fields f
                  WHERE f.object_id = o.id AND f.archived_at IS NULL)          AS field_count,
                (SELECT COUNT(*) FROM custom_records r
                  WHERE r.object_id = o.id AND r.deleted_at IS NULL)           AS record_count
           FROM custom_objects o
          WHERE o.tenant_id = $1
          ORDER BY o.slug",
    )
    .bind(tenant)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;

    Ok(rows
        .iter()
        .map(|r| {
            let mut v = json!({
                "id": r.get::<Uuid, _>("id"),
                "slug": r.get::<String, _>("slug"),
                "name": r.get::<String, _>("name"),
                "created_at": r.get::<DateTime<Utc>, _>("created_at"),
                "updated_at": r.get::<DateTime<Utc>, _>("updated_at"),
                "field_count": r.get::<i64, _>("field_count"),
                "record_count": r.get::<i64, _>("record_count"),
            });
            if let Some(m) = v.as_object_mut() {
                m.insert(
                    "schema_url".into(),
                    json!(format!(
                        "/api/objects/{}/schema",
                        r.get::<String, _>("slug")
                    )),
                );
            }
            v
        })
        .collect())
}

pub async fn create_object(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    slug: &str,
    name: &str,
) -> Result<Value, AppError> {
    check_slug(slug)?;
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("name is required".into()));
    }

    let row = sqlx::query(
        "INSERT INTO custom_objects (tenant_id, slug, name) VALUES ($1, $2, $3)
         RETURNING id, slug, name, created_at, updated_at",
    )
    .bind(tenant)
    .bind(slug)
    .bind(name)
    .fetch_one(db)
    .await
    .map_err(|e| {
        map_db(
            e,
            &format!("an object with slug '{slug}' already exists in this workspace"),
        )
    })?;

    let obj = Obj {
        id: row.get("id"),
        slug: row.get("slug"),
        name: row.get("name"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    };

    audit(
        db,
        tenant,
        actor,
        "object_created",
        Some(slug),
        json!({ "id": obj.id, "name": obj.name }),
    )
    .await?;

    Ok(obj.json())
}

pub async fn fields_for(
    db: &PgPool,
    object_id: Uuid,
    include_archived: bool,
) -> Result<Vec<Field>, AppError> {
    let rows = if include_archived {
        sqlx::query(
            "SELECT id, key, label, kind, required, unique_value, options, position, archived_at
               FROM custom_fields WHERE object_id = $1 ORDER BY position, key",
        )
        .bind(object_id)
        .fetch_all(db)
        .await
    } else {
        sqlx::query(
            "SELECT id, key, label, kind, required, unique_value, options, position, archived_at
               FROM custom_fields
              WHERE object_id = $1 AND archived_at IS NULL ORDER BY position, key",
        )
        .bind(object_id)
        .fetch_all(db)
        .await
    }
    .map_err(AppError::Database)?;

    Ok(rows.iter().map(field_from).collect())
}

// ── fields ─────────────────────────────────────────────────────────────────────────────────────

pub struct FieldSpec {
    pub key: String,
    pub label: String,
    pub kind: String,
    pub required: bool,
    pub unique: bool,
    pub options: Value,
    pub position: i32,
}

fn validate_kind_and_options(key: &str, kind: &str, options: &Value) -> Result<(), AppError> {
    if !KINDS.contains(&kind) {
        return Err(AppError::BadRequest(format!(
            "field '{key}' has kind '{kind}' — use one of: {}",
            KINDS.join(", ")
        )));
    }
    if kind == "enum" {
        let Some(list) = options.as_array() else {
            return Err(AppError::BadRequest(format!(
                "field '{key}' is an enum, so options must be a JSON array of allowed values"
            )));
        };
        if list.is_empty() {
            return Err(AppError::BadRequest(format!(
                "field '{key}' is an enum, so options must name at least one value"
            )));
        }
        if list.iter().any(|v| option_text(v).is_none()) {
            return Err(AppError::BadRequest(format!(
                "field '{key}': each option must be a string, or an object with a string \"value\""
            )));
        }
    }
    Ok(())
}

fn option_text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    v.get("value")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
}

pub async fn add_field(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    spec: FieldSpec,
) -> Result<Value, AppError> {
    check_slug(&spec.key).map_err(|_| {
        AppError::BadRequest(format!(
            "field key '{}' is not usable — use lowercase letters, digits and underscores, starting \
             with a letter, e.g. 'order_status'",
            spec.key
        ))
    })?;
    let label = spec.label.trim().to_string();
    if label.is_empty() {
        return Err(AppError::BadRequest(format!(
            "field '{}' needs a label",
            spec.key
        )));
    }
    validate_kind_and_options(&spec.key, &spec.kind, &spec.options)?;

    // Archiving keeps the row, so a re-added key must REVIVE its archived twin instead of colliding
    // on `custom_fields_object_key`. Its data (in `custom_records.data`) is untouched either way.
    let existing =
        sqlx::query("SELECT id, archived_at FROM custom_fields WHERE object_id = $1 AND key = $2")
            .bind(object.id)
            .bind(&spec.key)
            .fetch_optional(db)
            .await
            .map_err(AppError::Database)?;

    let row = if let Some(e) = existing {
        let archived_at: Option<DateTime<Utc>> = e.get("archived_at");
        if archived_at.is_none() {
            return Err(AppError::Duplicate(format!(
                "object '{}' already has an active field '{}'",
                object.slug, spec.key
            )));
        }
        sqlx::query(
            "UPDATE custom_fields
                SET label = $2, kind = $3, required = $4, unique_value = $5, options = $6,
                    position = $7, archived_at = NULL, updated_at = NOW()
              WHERE id = $1
              RETURNING id, key, label, kind, required, unique_value, options, position, archived_at",
        )
        .bind(e.get::<Uuid, _>("id"))
        .bind(&label)
        .bind(&spec.kind)
        .bind(spec.required)
        .bind(spec.unique)
        .bind(&spec.options)
        .bind(spec.position)
        .fetch_one(db)
        .await
        .map_err(AppError::Database)?
    } else {
        sqlx::query(
            "INSERT INTO custom_fields
                 (object_id, key, label, kind, required, unique_value, options, position)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             RETURNING id, key, label, kind, required, unique_value, options, position, archived_at",
        )
        .bind(object.id)
        .bind(&spec.key)
        .bind(&label)
        .bind(&spec.kind)
        .bind(spec.required)
        .bind(spec.unique)
        .bind(&spec.options)
        .bind(spec.position)
        .fetch_one(db)
        .await
        .map_err(|e| {
            map_db(
                e,
                &format!("object '{}' already has a field '{}'", object.slug, spec.key),
            )
        })?
    };

    let field = field_from(&row);
    audit(
        db,
        tenant,
        actor,
        "field_added",
        Some(&object.slug),
        json!({ "field": field.json() }),
    )
    .await?;
    Ok(field.json())
}

pub struct FieldPatch {
    pub key: Option<String>,
    pub label: Option<String>,
    pub position: Option<i32>,
    pub required: Option<bool>,
    pub unique: Option<bool>,
    pub options: Option<Value>,
}

pub async fn patch_field(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    field_id: Uuid,
    p: FieldPatch,
) -> Result<Value, AppError> {
    let row = sqlx::query(
        "SELECT id, key, label, kind, required, unique_value, options, position, archived_at
           FROM custom_fields WHERE id = $1 AND object_id = $2",
    )
    .bind(field_id)
    .bind(object.id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| {
        AppError::NotFound(format!("no field '{field_id}' on object '{}'", object.slug))
    })?;
    let current = field_from(&row);

    let kind = current.kind.clone();
    let options = p.options.unwrap_or_else(|| current.options.clone());
    validate_kind_and_options(&current.key, &kind, &options)?;

    let next_key = p.key.unwrap_or_else(|| current.key.clone());
    if next_key != current.key {
        check_slug(&next_key).map_err(|_| {
            AppError::BadRequest(format!(
                "field key '{next_key}' is not usable — lowercase letters, digits and underscores"
            ))
        })?;
        // Renaming a key with data behind it would silently orphan those values, so the rename is
        // refused while any live record carries it. Archive + add is the migration path in Phase 1.
        let carrying: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM custom_records
              WHERE object_id = $1 AND deleted_at IS NULL AND data ? $2",
        )
        .bind(object.id)
        .bind(&current.key)
        .fetch_one(db)
        .await
        .map_err(AppError::Database)?;
        if carrying > 0 {
            return Err(AppError::Conflict(format!(
                "field '{}' holds values on {carrying} record(s) — renaming it would orphan them. \
                 Archive it with archive_field and add '{next_key}' instead",
                current.key
            )));
        }
    }

    let label = p.label.unwrap_or_else(|| current.label.clone());
    let position = p.position.unwrap_or(current.position);
    let required = p.required.unwrap_or(current.required);
    let unique_value = p.unique.unwrap_or(current.unique_value);

    let row = sqlx::query(
        "UPDATE custom_fields
            SET key = $2, label = $3, required = $4, unique_value = $5, options = $6,
                position = $7, updated_at = NOW()
          WHERE id = $1
          RETURNING id, key, label, kind, required, unique_value, options, position, archived_at",
    )
    .bind(field_id)
    .bind(&next_key)
    .bind(&label)
    .bind(required)
    .bind(unique_value)
    .bind(&options)
    .bind(position)
    .fetch_one(db)
    .await
    .map_err(|e| {
        map_db(
            e,
            &format!("object '{}' already has a field '{next_key}'", object.slug),
        )
    })?;

    let field = field_from(&row);
    audit(
        db,
        tenant,
        actor,
        "field_updated",
        Some(&object.slug),
        json!({ "before": current.json(), "after": field.json() }),
    )
    .await?;
    Ok(field.json())
}

pub async fn archive_field(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    field: &str,
) -> Result<Value, AppError> {
    // Accept an id or a key, because an agent holding the schema usually has the key in hand.
    let row = match Uuid::parse_str(field) {
        Ok(id) => sqlx::query(
            "SELECT id, key, label, kind, required, unique_value, options, position, archived_at
               FROM custom_fields WHERE id = $1 AND object_id = $2",
        )
        .bind(id)
        .bind(object.id)
        .fetch_optional(db)
        .await,
        Err(_) => sqlx::query(
            "SELECT id, key, label, kind, required, unique_value, options, position, archived_at
                   FROM custom_fields WHERE object_id = $1 AND key = $2",
        )
        .bind(object.id)
        .bind(field)
        .fetch_optional(db)
        .await,
    }
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound(format!("no field '{field}' on object '{}'", object.slug)))?;

    let current = field_from(&row);
    if current.archived {
        return Ok(json!({ "field": current.json(), "archived": true, "already": true }));
    }

    sqlx::query("UPDATE custom_fields SET archived_at = NOW(), updated_at = NOW() WHERE id = $1")
        .bind(current.id)
        .execute(db)
        .await
        .map_err(AppError::Database)?;

    // The data is deliberately NOT touched: archiving is a schema change, and a record that already
    // carries the value keeps it (proof gate check 5).
    audit(
        db,
        tenant,
        actor,
        "field_archived",
        Some(&object.slug),
        json!({ "field_id": current.id, "key": current.key, "data_retained": true }),
    )
    .await?;

    Ok(json!({ "field": current.json(), "archived": true, "data_retained": true }))
}

// ── records ────────────────────────────────────────────────────────────────────────────────────

fn bad_kind(f: &Field, expected: &str) -> AppError {
    AppError::BadRequest(format!(
        "field '{}' must be {expected} (it is declared '{kind}')",
        f.key,
        kind = f.kind
    ))
}

fn check_value(f: &Field, v: &Value) -> Result<(), AppError> {
    match f.kind.as_str() {
        "text" => {
            if !v.is_string() {
                return Err(bad_kind(f, "a string"));
            }
        }
        "number" => {
            if !v.is_number() {
                return Err(bad_kind(f, "a number"));
            }
        }
        "bool" => {
            if !v.is_boolean() {
                return Err(bad_kind(f, "true or false"));
            }
        }
        "date" => {
            let Some(s) = v.as_str() else {
                return Err(bad_kind(
                    f,
                    "an ISO date (YYYY-MM-DD) or an RFC3339 timestamp",
                ));
            };
            let ok = NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
                || DateTime::parse_from_rfc3339(s).is_ok();
            if !ok {
                return Err(bad_kind(
                    f,
                    "an ISO date (YYYY-MM-DD) or an RFC3339 timestamp",
                ));
            }
        }
        "enum" => {
            let Some(s) = v.as_str() else {
                return Err(bad_kind(f, "one of the declared option strings"));
            };
            let allowed: Vec<String> = f
                .options
                .as_array()
                .map(|a| a.iter().filter_map(option_text).collect())
                .unwrap_or_default();
            if !allowed.iter().any(|o| o == s) {
                return Err(AppError::BadRequest(format!(
                    "field '{}' must be one of: {}",
                    f.key,
                    allowed.join(", ")
                )));
            }
        }
        // `json` accepts anything, and an unknown kind cannot occur (the CHECK constraint and
        // `validate_kind_and_options` both refuse it).
        _ => {}
    }
    Ok(())
}

/// Validate the keys a caller wrote. `require_all` is true for an INSERT and for the merged result
/// of a PATCH, so a patch can never drop a required value by omitting it.
pub fn validate_written(
    active: &[Field],
    written: &Map<String, Value>,
    require_all: bool,
) -> Result<(), AppError> {
    for (k, v) in written {
        let f = active.iter().find(|f| &f.key == k).ok_or_else(|| {
            AppError::BadRequest(format!(
                "unknown field '{k}' — object has no such active field. Add it first with add_field \
                 (or fix the spelling); nothing was written"
            ))
        })?;
        check_value(f, v)?;
    }
    if require_all {
        for f in active.iter().filter(|f| f.required) {
            let present = written.get(&f.key).map(|v| !v.is_null()).unwrap_or(false);
            if !present {
                return Err(AppError::BadRequest(format!(
                    "missing required field '{}'",
                    f.key
                )));
            }
        }
    }
    Ok(())
}

/// The text form `jsonb ->>` produces, so a uniqueness check in Rust and in SQL agree.
fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

async fn ensure_unique(
    tx: &mut Transaction<'_, Postgres>,
    object_id: Uuid,
    active: &[Field],
    data: &Map<String, Value>,
    exclude: Option<Uuid>,
) -> Result<(), AppError> {
    for f in active.iter().filter(|f| f.unique_value) {
        let Some(v) = data.get(&f.key) else { continue };
        if v.is_null() {
            continue;
        }
        let hit = sqlx::query(
            "SELECT id FROM custom_records
              WHERE object_id = $1 AND deleted_at IS NULL AND data->>$2 = $3
                AND ($4::uuid IS NULL OR id <> $4::uuid)
              LIMIT 1",
        )
        .bind(object_id)
        .bind(&f.key)
        .bind(scalar_text(v))
        .bind(exclude)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::Database)?;
        if hit.is_some() {
            return Err(AppError::Duplicate(format!(
                "field '{}' must be unique, and a record with that value already exists",
                f.key
            )));
        }
    }
    Ok(())
}

/// Serialise writes for ONE object so the uniqueness check cannot be raced by two concurrent
/// inserts (a read-then-write check without a lock is not a constraint). Transaction-scoped: it is
/// released by COMMIT/ROLLBACK, so a crashed caller cannot wedge the object.
async fn lock_object(tx: &mut Transaction<'_, Postgres>, object_id: Uuid) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(object_id)
        .execute(&mut **tx)
        .await
        .map_err(AppError::Database)?;
    Ok(())
}

pub async fn insert_record(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    data: Map<String, Value>,
) -> Result<Value, AppError> {
    let active: Vec<Field> = fields_for(db, object.id, false).await?;
    validate_written(&active, &data, true)?;

    let mut tx = db.begin().await.map_err(AppError::Database)?;
    lock_object(&mut tx, object.id).await?;
    ensure_unique(&mut tx, object.id, &active, &data, None).await?;

    let row = sqlx::query(
        "INSERT INTO custom_records (tenant_id, object_id, data) VALUES ($1, $2, $3)
         RETURNING id, data, created_at, updated_at",
    )
    .bind(tenant)
    .bind(object.id)
    .bind(Value::Object(data))
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Database)?;
    tx.commit().await.map_err(AppError::Database)?;

    let out = json!({
        "id": row.get::<Uuid, _>("id"),
        "object": object.slug,
        "data": row.get::<Value, _>("data"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
    });
    audit(
        db,
        tenant,
        actor,
        "record_inserted",
        Some(&object.slug),
        json!({ "id": out["id"] }),
    )
    .await?;
    Ok(out)
}

pub struct Query {
    pub limit: i64,
    pub offset: i64,
    /// `created_at` | `updated_at` | `id` | a field key, optionally prefixed with `-` for descending.
    pub sort: String,
    pub filters: Map<String, Value>,
}

pub async fn query_records(
    db: &PgPool,
    tenant: Uuid,
    object: &Obj,
    q: Query,
) -> Result<Value, AppError> {
    let active = fields_for(db, object.id, false).await?;
    let mut desc = false;
    let mut sort = q.sort.trim().to_string();
    if let Some(rest) = sort.strip_prefix('-') {
        desc = true;
        sort = rest.to_string();
    }
    if sort.is_empty() {
        sort = "created_at".into();
        desc = true;
    }

    let named_field = !["created_at", "updated_at", "id"].contains(&sort.as_str());
    if named_field && !active.iter().any(|f| f.key == sort) {
        return Err(AppError::BadRequest(format!(
            "cannot sort by '{sort}' — it is not a field of '{}' (or created_at / updated_at / id)",
            object.slug
        )));
    }

    // One literal statement per branch. The deploy gate refuses SQL built by concatenation or
    // `format!`, and a dynamic column name is exactly the shape that produces an injection — so the
    // sortable identifiers are enumerated here and a field key is bound as a parameter (`data->>$6`).
    let (sql, bind_field) = match (sort.as_str(), desc) {
        ("created_at", false) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY created_at ASC LIMIT $4 OFFSET $5",
            false,
        ),
        ("created_at", true) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY created_at DESC LIMIT $4 OFFSET $5",
            false,
        ),
        ("updated_at", false) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY updated_at ASC LIMIT $4 OFFSET $5",
            false,
        ),
        ("updated_at", true) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY updated_at DESC LIMIT $4 OFFSET $5",
            false,
        ),
        ("id", false) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY id ASC LIMIT $4 OFFSET $5",
            false,
        ),
        ("id", true) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY id DESC LIMIT $4 OFFSET $5",
            false,
        ),
        (_, false) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY data->>$6 ASC, created_at DESC LIMIT $4 OFFSET $5",
            true,
        ),
        (_, true) => (
            "SELECT id, data, created_at, updated_at FROM custom_records
              WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb
              ORDER BY data->>$6 DESC, created_at DESC LIMIT $4 OFFSET $5",
            true,
        ),
    };

    let mut stmt = sqlx::query(sql)
        .bind(tenant)
        .bind(object.id)
        .bind(Value::Object(q.filters.clone()))
        .bind(q.limit)
        .bind(q.offset);
    if bind_field {
        stmt = stmt.bind(&sort);
    }
    let rows = stmt.fetch_all(db).await.map_err(AppError::Database)?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM custom_records
          WHERE tenant_id = $1 AND object_id = $2 AND deleted_at IS NULL AND data @> $3::jsonb",
    )
    .bind(tenant)
    .bind(object.id)
    .bind(Value::Object(q.filters))
    .fetch_one(db)
    .await
    .map_err(AppError::Database)?;

    let records: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "data": r.get::<Value, _>("data"),
                "created_at": r.get::<DateTime<Utc>, _>("created_at"),
                "updated_at": r.get::<DateTime<Utc>, _>("updated_at"),
            })
        })
        .collect();

    Ok(json!({
        "object": object.slug,
        "records": records,
        "count": records.len(),
        "total": total,
        "limit": q.limit,
        "offset": q.offset,
    }))
}

pub async fn update_record(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    id: Uuid,
    patch: Map<String, Value>,
) -> Result<Value, AppError> {
    let active = fields_for(db, object.id, false).await?;

    let mut tx = db.begin().await.map_err(AppError::Database)?;
    lock_object(&mut tx, object.id).await?;

    let row = sqlx::query(
        "SELECT data FROM custom_records
          WHERE id = $1 AND tenant_id = $2 AND object_id = $3 AND deleted_at IS NULL",
    )
    .bind(id)
    .bind(tenant)
    .bind(object.id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound(format!("no record '{id}' on object '{}'", object.slug)))?;

    let current: Value = row.get("data");
    let mut merged: Map<String, Value> = current.as_object().cloned().unwrap_or_default();
    // Keys whose field is archived keep their stored value but are never re-validated: the schema
    // moved on, the data did not.
    for (k, v) in patch.iter() {
        merged.insert(k.clone(), v.clone());
    }
    // Validate what the caller WROTE (unknown keys refused), then the merged row's required set.
    validate_written(&active, &patch, false)?;
    validate_written(&active, &merged, true)?;
    ensure_unique(&mut tx, object.id, &active, &patch, Some(id)).await?;

    let row = sqlx::query(
        "UPDATE custom_records SET data = $2, updated_at = NOW()
          WHERE id = $1 AND deleted_at IS NULL
          RETURNING id, data, created_at, updated_at",
    )
    .bind(id)
    .bind(Value::Object(merged))
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Database)?;
    tx.commit().await.map_err(AppError::Database)?;

    let out = json!({
        "id": row.get::<Uuid, _>("id"),
        "object": object.slug,
        "data": row.get::<Value, _>("data"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
    });
    audit(
        db,
        tenant,
        actor,
        "record_updated",
        Some(&object.slug),
        json!({ "id": out["id"], "fields": patch.keys().cloned().collect::<Vec<_>>() }),
    )
    .await?;
    Ok(out)
}

pub async fn delete_record(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    id: Uuid,
) -> Result<Value, AppError> {
    let row = sqlx::query(
        "UPDATE custom_records SET deleted_at = NOW(), updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2 AND object_id = $3 AND deleted_at IS NULL
          RETURNING id",
    )
    .bind(id)
    .bind(tenant)
    .bind(object.id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound(format!("no record '{id}' on object '{}'", object.slug)))?;

    audit(
        db,
        tenant,
        actor,
        "record_deleted",
        Some(&object.slug),
        json!({ "id": row.get::<Uuid, _>("id"), "hard": false }),
    )
    .await?;
    Ok(json!({ "deleted": true, "id": id, "hard": false }))
}

// ── audit ─────────────────────────────────────────────────────────────────────────────────────

pub async fn audit(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    action: &str,
    slug: Option<&str>,
    detail: Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO custom_object_audit (tenant_id, actor, action, object_slug, detail)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(tenant)
    .bind(actor)
    .bind(action)
    .bind(slug)
    .bind(detail)
    .execute(db)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

pub async fn audit_rows(
    db: &PgPool,
    tenant: Uuid,
    slug: &str,
    limit: i64,
) -> Result<Value, AppError> {
    let rows = sqlx::query(
        "SELECT id, actor, action, object_slug, detail, at FROM custom_object_audit
          WHERE tenant_id = $1 AND object_slug = $2 ORDER BY at DESC LIMIT $3",
    )
    .bind(tenant)
    .bind(slug)
    .bind(limit)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?;

    Ok(json!(rows
        .iter()
        .map(|r| json!({
            "id": r.get::<i64, _>("id"),
            "actor": r.get::<String, _>("actor"),
            "action": r.get::<String, _>("action"),
            "object": r.get::<Option<String>, _>("object_slug"),
            "detail": r.get::<Value, _>("detail"),
            "at": r.get::<DateTime<Utc>, _>("at"),
        }))
        .collect::<Vec<_>>()))
}

/// Coerce a query-string filter value to the field's declared kind, so `filter.amount=100` matches
/// a NUMBER and `filter.paid=true` matches a BOOLEAN. An unknown field is refused, not ignored.
pub fn coerce_filter(active: &[Field], key: &str, raw: &str) -> Result<Value, AppError> {
    let f = active.iter().find(|f| f.key == key).ok_or_else(|| {
        AppError::BadRequest(format!(
            "cannot filter on '{key}' — it is not a field of this object"
        ))
    })?;
    match f.kind.as_str() {
        "number" => raw
            .parse::<f64>()
            .map(|n| json!(n))
            .map_err(|_| AppError::BadRequest(format!("filter.{key} must be a number"))),
        "bool" => match raw {
            "true" | "1" => Ok(json!(true)),
            "false" | "0" => Ok(json!(false)),
            _ => Err(AppError::BadRequest(format!(
                "filter.{key} must be true or false"
            ))),
        },
        _ => Ok(json!(raw)),
    }
}

/// Parse `limit` / `offset` out of a string parameter, clamped to something a database should serve.
pub fn clamp_window(limit: Option<&str>, offset: Option<&str>) -> Result<(i64, i64), AppError> {
    let limit = match limit {
        None => 50,
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| AppError::BadRequest("limit must be a whole number".into()))?,
    };
    let offset = match offset {
        None => 0,
        Some(s) => s
            .parse::<i64>()
            .map_err(|_| AppError::BadRequest("offset must be a whole number".into()))?,
    };
    if limit < 1 {
        return Err(AppError::BadRequest("limit must be at least 1".into()));
    }
    if offset < 0 {
        return Err(AppError::BadRequest("offset cannot be negative".into()));
    }
    Ok((limit.min(200), offset))
}
