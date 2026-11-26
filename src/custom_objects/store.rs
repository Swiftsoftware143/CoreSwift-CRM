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
    /// Field keys whose COMBINATION must be unique among live records (migration 208). Empty means
    /// "no composite key": every field behaves exactly as it did before.
    pub unique_together: Vec<String>,
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
            "unique_together": self.unique_together,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }
}

/// The `unique_together` column as a list of field keys. The CHECK constraint (migration 208) keeps
/// it an array; a non-string element cannot be produced by [`set_unique_together`], so it is dropped
/// rather than turned into an error on every read of an object row.
fn keys_from(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
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
    // LIVE objects only: an archived object is gone from every door (describe, records, fields,
    // audit). Its rows stay in the database, readable by id from support/SQL, which is what makes
    // archive a reversible-in-principle removal rather than a delete.
    let row = sqlx::query(
        "SELECT id, slug, name, unique_together, created_at, updated_at FROM custom_objects
         WHERE tenant_id = $1 AND slug = $2 AND archived_at IS NULL",
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
        unique_together: keys_from(&row.get::<Value, _>("unique_together")),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

pub async fn list_objects(db: &PgPool, tenant: Uuid) -> Result<Vec<Value>, AppError> {
    let rows = sqlx::query(
        "SELECT o.id, o.slug, o.name, o.unique_together, o.created_at, o.updated_at,
                (SELECT COUNT(*) FROM custom_fields f
                  WHERE f.object_id = o.id AND f.archived_at IS NULL)          AS field_count,
                (SELECT COUNT(*) FROM custom_records r
                  WHERE r.object_id = o.id AND r.deleted_at IS NULL)           AS record_count
           FROM custom_objects o
          WHERE o.tenant_id = $1 AND o.archived_at IS NULL
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
                "unique_together": keys_from(&r.get::<Value, _>("unique_together")),
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
    // A NUL pasted into the object's name is invisible and unstorable (`security::free_text`).
    let stripped = crate::security::free_text::strip_nul(name);
    let name = stripped.trim();
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
        // A brand-new object has no composite key: the caller sets one with set_unique_together.
        unique_together: Vec::new(),
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

/// Archive an object — the ONE removal this module offers for an object (kanban t_933dd588 §6a).
///
/// The object and its live fields get `archived_at`. Every door then answers 404 for the slug,
/// because [`object_by_slug`] and [`list_objects`] read live rows only. The RECORDS are untouched:
/// the customer's data stays in `custom_records`, readable by record id from support/SQL. That is
/// deliberate — archiving a shape must never be an accidental mass-erasure, and there is no object
/// DELETE anywhere in this module (account-level erasure stays a signed-in human's route).
///
/// Idempotent: a second call changes nothing, answers the same shape with `already_archived: true`,
/// and still leaves one audit row so the attempt itself is visible.
pub async fn archive_object(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    slug: &str,
) -> Result<Value, AppError> {
    let mut tx = db.begin().await.map_err(AppError::Database)?;

    // 404 for another account's slug, exactly like every other lookup: a refusal must not confirm
    // that someone else's object exists.
    let row = sqlx::query(
        "SELECT id, archived_at FROM custom_objects
          WHERE tenant_id = $1 AND slug = $2 FOR UPDATE",
    )
    .bind(tenant)
    .bind(slug)
    .fetch_optional(&mut *tx)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound(format!("no object '{slug}' in this workspace")))?;

    let id: Uuid = row.get("id");
    let already: Option<DateTime<Utc>> = row.get("archived_at");
    let mut fields_archived: i64 = 0;

    if already.is_none() {
        sqlx::query(
            "UPDATE custom_objects SET archived_at = NOW(), updated_at = NOW() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

        fields_archived = sqlx::query(
            "UPDATE custom_fields SET archived_at = NOW(), updated_at = NOW()
              WHERE object_id = $1 AND archived_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?
        .rows_affected() as i64;
    }

    tx.commit().await.map_err(AppError::Database)?;

    audit(
        db,
        tenant,
        actor,
        "object_archived",
        Some(slug),
        json!({
            "id": id,
            "already_archived": already.is_some(),
            "fields_archived": fields_archived,
        }),
    )
    .await?;

    Ok(json!({
        "archived": true,
        "already_archived": already.is_some(),
        "id": id,
        "slug": slug,
        "fields_archived": fields_archived,
        "records_kept": true,
    }))
}

/// Set (or clear) an object's COMPOSITE key (migration 208): the combination of field values that must
/// be unique among live records, and the thing a replay-safe upsert is keyed on.
///
/// Refuses while duplicates exist and NAMES them — the colliding values and the row ids — because
/// "which of these two is the duplicate?" is the caller's decision, not something the writer may guess
/// (deleting or overwriting a customer's row to satisfy a schema change would be far worse than a 409).
///
/// On success it also re-keys every live record IN THE SAME TRANSACTION, so an object that already has
/// rows obeys the new constraint immediately rather than only for rows written afterwards. Clearing it
/// (`[]`) nulls the stored keys, so a later, different key set cannot inherit an old one.
pub async fn set_unique_together(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    requested: Vec<String>,
) -> Result<Value, AppError> {
    let active = fields_for(db, object.id, false).await?;

    let mut keys: Vec<String> = Vec::new();
    for raw in &requested {
        let k = raw.trim();
        if k.is_empty() {
            return Err(AppError::BadRequest(
                "unique_together cannot contain an empty field key".into(),
            ));
        }
        if keys.iter().any(|c| c == k) {
            return Err(AppError::BadRequest(format!(
                "unique_together names '{k}' twice — list each field once"
            )));
        }
        keys.push(k.to_string());
    }
    if keys.len() > 8 {
        return Err(AppError::BadRequest(format!(
            "unique_together takes at most 8 fields — got {}",
            keys.len()
        )));
    }
    for k in &keys {
        let Some(f) = active.iter().find(|f| &f.key == k) else {
            return Err(AppError::BadRequest(format!(
                "field '{k}' is not a live field of '{}' — add it first with add_field (or it was \
                 archived, so it can no longer be part of a key)",
                object.slug
            )));
        };
        if f.kind == "json" {
            return Err(AppError::BadRequest(format!(
                "field '{k}' is a json field — a composite key must be made of text, number, bool, \
                 date or enum values"
            )));
        }
    }

    let mut tx = db.begin().await.map_err(AppError::Database)?;
    lock_object(&mut tx, object.id).await?;

    let live = sqlx::query(
        "SELECT id, data FROM custom_records
          WHERE object_id = $1 AND deleted_at IS NULL ORDER BY created_at",
    )
    .bind(object.id)
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    // One pass: derive every live record's key under the NEW key set, remembering what each row looks
    // like so a refusal can name the values as well as the ids.
    let mut keyed: Vec<(Uuid, Option<String>)> = Vec::with_capacity(live.len());
    let mut seen: std::collections::HashMap<String, Vec<(Uuid, String)>> =
        std::collections::HashMap::new();
    for rec in live.iter() {
        let id: Uuid = rec.get("id");
        let data: Value = rec.get("data");
        let Some(map) = data.as_object() else {
            continue;
        };
        let key = key_for(&keys, map)?;
        if let Some(k) = &key {
            let described = keys
                .iter()
                .map(|f| {
                    let v = map.get(f).map(scalar_text).unwrap_or_default();
                    format!("{f}='{v}'")
                })
                .collect::<Vec<_>>()
                .join(", ");
            seen.entry(k.clone()).or_default().push((id, described));
        }
        keyed.push((id, key));
    }

    let collisions: Vec<&Vec<(Uuid, String)>> = seen.values().filter(|v| v.len() > 1).collect();
    if !collisions.is_empty() {
        let mut examples: Vec<String> = Vec::new();
        for g in collisions.iter().take(3) {
            let ids: Vec<String> = g.iter().map(|(id, _)| id.to_string()).collect();
            examples.push(format!(
                "{} — {} live rows (ids {})",
                g[0].1,
                g.len(),
                ids.join(", ")
            ));
        }
        let more = if collisions.len() > 3 {
            format!(
                "; and {} more colliding combination(s)",
                collisions.len() - 3
            )
        } else {
            String::new()
        };
        tx.rollback().await.ok();
        return Err(AppError::Duplicate(format!(
            "cannot set unique_together [{}] on '{}': {} combination(s) of live records collide — \
             {}{}. Decide which row of each pair is the duplicate (update or delete it) and set the \
             key again; nothing was changed",
            keys.join(", "),
            object.slug,
            collisions.len(),
            examples.join("; "),
            more
        )));
    }

    let rekeyed = keyed.len() as i64;
    sqlx::query("UPDATE custom_objects SET unique_together = $2, updated_at = NOW() WHERE id = $1")
        .bind(object.id)
        .bind(json!(&keys))
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    // One bound statement per record. The object lock is held for the whole loop, so no writer can slip
    // a row in between the scan and the re-key (the unique index is the backstop if one ever does).
    for (id, key) in &keyed {
        sqlx::query("UPDATE custom_records SET uniq_key = $2 WHERE id = $1")
            .bind(id)
            .bind(key.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(|e| {
                map_db(
                    e,
                    &format!(
                        "cannot set unique_together on '{}': live records collide on this key",
                        object.slug
                    ),
                )
            })?;
    }

    tx.commit().await.map_err(AppError::Database)?;

    audit(
        db,
        tenant,
        actor,
        "unique_together_set",
        Some(&object.slug),
        json!({ "fields": keys, "rekeyed": rekeyed }),
    )
    .await?;

    Ok(json!({
        "object": object.slug,
        "unique_together": keys,
        "rekeyed": rekeyed,
    }))
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
    let label = crate::security::free_text::strip_nul(&spec.label)
        .trim()
        .to_string();
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

/// Every non-archived required field must be present. Separate from [`validate_written`] on purpose:
/// a PATCH validates the keys it WROTE, but its required check runs over the MERGED record, and that
/// record can legitimately still carry values of fields that were archived since — those keys are not
/// writable any more, but they are not errors either.
pub fn check_required(active: &[Field], merged: &Map<String, Value>) -> Result<(), AppError> {
    for f in active.iter().filter(|f| f.required) {
        let present = merged.get(&f.key).map(|v| !v.is_null()).unwrap_or(false);
        if !present {
            return Err(AppError::BadRequest(format!(
                "missing required field '{}'",
                f.key
            )));
        }
    }
    Ok(())
}

/// Validate the keys a caller wrote. `require_all` is true for an INSERT, so a fresh record cannot
/// omit a required value.
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
        check_required(active, written)?;
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
    // The bind is a STRING, not the uuid: `$1::text` makes Postgres declare the parameter as text,
    // and sqlx sends a `Uuid` with the uuid type OID — the driver refuses the mismatch at runtime
    // (the same reason `workspaces::handlers` binds `tid.to_string()` here).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))")
        .bind(object_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(AppError::Database)?;
    Ok(())
}

/// The canonical text form of a record's COMPOSITE key (migration 208), or `None` when the object has
/// no `unique_together` or the record does not carry every key value.
///
/// A JSON array of the values, never a concatenation: two different splits must not produce the same
/// string (`("a|b","c")` vs `("a","b|c")`), and the array is exactly what `custom_records.uniq_key`
/// stores. This is the ONE definition — insert, upsert, update and the re-key backfill all call it, so
/// a key written by one path matches the key another path looks for.
pub fn key_for(keys: &[String], data: &Map<String, Value>) -> Result<Option<String>, AppError> {
    if keys.is_empty() {
        return Ok(None);
    }
    let mut parts: Vec<String> = Vec::with_capacity(keys.len());
    for k in keys {
        match data.get(k) {
            // A record that does not carry every key value is not keyed at all. The unique index is
            // partial on `uniq_key IS NOT NULL`, so any number of such records may coexist.
            None | Some(Value::Null) => return Ok(None),
            Some(v) => parts.push(scalar_text(v)),
        }
    }
    serde_json::to_string(&parts).map(Some).map_err(|e| {
        AppError::BadRequest(format!("cannot derive this record's composite key: {e}"))
    })
}

fn composite_duplicate(object: &Obj, keys: &[String]) -> String {
    format!(
        "object '{}' already has a live record with this combination of {} — nothing was written. \
         Send the same write with ?upsert={} to update that row instead of adding a second one",
        object.slug,
        keys.join(" + "),
        keys.join(",")
    )
}

/// The `upsert=` keys of a write, checked against the object's `unique_together`. A caller may name
/// the keys in any order (the key itself is order-independent here because the whole set is sent),
/// but the SET must be exactly the object's composite key — anything else has no constraint behind it
/// and would silently insert a duplicate.
fn upsert_target(object: &Obj, requested: &[String]) -> Result<Vec<String>, AppError> {
    let mut clean: Vec<String> = Vec::new();
    for raw in requested {
        let k = raw.trim();
        if k.is_empty() {
            return Err(AppError::BadRequest(
                "upsert=... takes a comma-separated list of field keys — one of them was empty"
                    .into(),
            ));
        }
        if !clean.iter().any(|c| c == k) {
            clean.push(k.to_string());
        }
    }
    if object.unique_together.is_empty() {
        return Err(AppError::BadRequest(format!(
            "object '{}' has no unique_together, so there is no combination a replay could match. \
             Set one first: PATCH /api/objects/{} {{\"unique_together\":[\"k1\",\"k2\"]}}",
            object.slug, object.slug
        )));
    }
    let mut have: Vec<&str> = object.unique_together.iter().map(|s| s.as_str()).collect();
    have.sort_unstable();
    let mut want: Vec<&str> = clean.iter().map(|s| s.as_str()).collect();
    want.sort_unstable();
    if have != want {
        return Err(AppError::BadRequest(format!(
            "upsert can only be keyed on this object's unique_together [{}] — asked for [{}]",
            object.unique_together.join(", "),
            clean.join(", ")
        )));
    }
    Ok(clean)
}

/// Write ONE record. With `upsert_on` the write is a REPLAY-SAFE upsert keyed on the object's
/// `unique_together` (migration 208): a second call with the same key values updates that row and
/// merges the supplied fields into it, so a producer that retries never inserts a second row.
pub async fn insert_record(
    db: &PgPool,
    tenant: Uuid,
    actor: &str,
    object: &Obj,
    data: Map<String, Value>,
    upsert_on: Option<Vec<String>>,
) -> Result<Value, AppError> {
    let active: Vec<Field> = fields_for(db, object.id, false).await?;

    // A plain insert must carry the whole required set, exactly as before. An upsert is identified by
    // its key values, so those are mandatory while the rest of the required set is checked on the
    // MERGED row after the write — a replay that changes one field must not have to resend the record.
    let (keys, key) = match &upsert_on {
        None => {
            validate_written(&active, &data, true)?;
            (
                object.unique_together.clone(),
                key_for(&object.unique_together, &data)?,
            )
        }
        Some(requested) => {
            let keys = upsert_target(object, requested)?;
            let missing: Vec<&str> = keys
                .iter()
                .filter(|k| data.get(*k).map(|v| v.is_null()).unwrap_or(true))
                .map(|k| k.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(AppError::BadRequest(format!(
                    "upsert needs a value for every key field — missing: {}",
                    missing.join(", ")
                )));
            }
            validate_written(&active, &data, false)?;
            let key = key_for(&keys, &data)?;
            (keys, key)
        }
    };
    let duplicate = composite_duplicate(object, &keys);

    let mut tx = db.begin().await.map_err(AppError::Database)?;
    lock_object(&mut tx, object.id).await?;
    ensure_unique(&mut tx, object.id, &active, &data, None).await?;

    let (row, inserted) = if upsert_on.is_some() {
        // ONE literal statement. The conflict target is the partial unique index from migration 208
        // (its predicate repeated verbatim, which is what lets PostgreSQL infer it);
        // `data || EXCLUDED.data` merges the supplied fields into the existing row and leaves every
        // field the caller did not send untouched.
        let row = sqlx::query(
            "INSERT INTO custom_records (tenant_id, object_id, data, uniq_key)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (object_id, uniq_key) WHERE deleted_at IS NULL AND uniq_key IS NOT NULL
             DO UPDATE SET data = custom_records.data || EXCLUDED.data, updated_at = NOW()
             RETURNING id, data, created_at, updated_at, (xmax = 0) AS inserted",
        )
        .bind(tenant)
        .bind(object.id)
        .bind(Value::Object(data))
        .bind(key.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_db(e, &duplicate))?;
        let inserted: bool = row.get("inserted");
        if let Some(merged) = row.get::<Value, _>("data").as_object() {
            if let Err(e) = check_required(&active, merged) {
                // Nothing is written: the transaction is rolled back, so a replay that is missing a
                // required value cannot leave a half-valid row behind.
                tx.rollback().await.ok();
                return Err(e);
            }
        }
        (row, inserted)
    } else {
        let row = sqlx::query(
            "INSERT INTO custom_records (tenant_id, object_id, data, uniq_key)
             VALUES ($1, $2, $3, $4)
             RETURNING id, data, created_at, updated_at",
        )
        .bind(tenant)
        .bind(object.id)
        .bind(Value::Object(data))
        .bind(key.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| map_db(e, &duplicate))?;
        (row, true)
    };
    tx.commit().await.map_err(AppError::Database)?;

    let mut out = json!({
        "id": row.get::<Uuid, _>("id"),
        "object": object.slug,
        "data": row.get::<Value, _>("data"),
        "created_at": row.get::<DateTime<Utc>, _>("created_at"),
        "updated_at": row.get::<DateTime<Utc>, _>("updated_at"),
    });
    let action = if upsert_on.is_some() {
        // What a replay DID (updated vs inserted) is useful to the caller; the audit row still names
        // the record id only, exactly like every other record action.
        out["upsert"] = json!({ "on": keys, "inserted": inserted });
        "record_upserted"
    } else {
        "record_inserted"
    };
    audit(
        db,
        tenant,
        actor,
        action,
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
    // Validate what the caller WROTE (unknown keys / wrong kinds refused), then the merged row's
    // required set — the values of fields archived since are carried but never re-validated.
    validate_written(&active, &patch, false)?;
    check_required(&active, &merged)?;
    ensure_unique(&mut tx, object.id, &active, &patch, Some(id)).await?;

    // The composite key is recomputed from the MERGED row, because a patch can change one half of it.
    let uniq_key = key_for(&object.unique_together, &merged)?;
    let duplicate = format!(
        "object '{}' already has a live record with the same combination of {} — this change would \
         duplicate it, so nothing was written",
        object.slug,
        object.unique_together.join(" + ")
    );

    let row = sqlx::query(
        "UPDATE custom_records SET data = $2, uniq_key = $3, updated_at = NOW()
          WHERE id = $1 AND deleted_at IS NULL
          RETURNING id, data, created_at, updated_at",
    )
    .bind(id)
    .bind(Value::Object(merged))
    .bind(uniq_key.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| map_db(e, &duplicate))?;
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
    hard: bool,
) -> Result<Value, AppError> {
    if hard {
        // The actual DELETE (kanban t_933dd588 §6b). It matches a live OR an already
        // soft-deleted row on purpose: a customer who soft-deleted and then asked for the data to
        // be gone must still be able to get it removed. The audit row carries the record's ID and
        // NOTHING ELSE — the data is gone, so copying it into the trail would defeat the purge.
        let row = sqlx::query(
            "DELETE FROM custom_records
              WHERE id = $1 AND tenant_id = $2 AND object_id = $3
              RETURNING id",
        )
        .bind(id)
        .bind(tenant)
        .bind(object.id)
        .fetch_optional(db)
        .await
        .map_err(AppError::Database)?
        .ok_or_else(|| {
            AppError::NotFound(format!("no record '{id}' on object '{}'", object.slug))
        })?;

        audit(
            db,
            tenant,
            actor,
            "record_purged",
            Some(&object.slug),
            json!({ "id": row.get::<Uuid, _>("id") }),
        )
        .await?;
        return Ok(json!({ "deleted": true, "hard": true, "id": id }));
    }

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
