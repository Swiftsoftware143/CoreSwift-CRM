//! The mapping runner: resolve a stored mapping against one source row and write it.
//!
//! Determinism is the whole point — the same row and the same mapping must produce the same payload
//! every time, which is why the UI preview and this runner share one resolver (`resolver` below is
//! called by both).
//!
//! The contact row is written by `external_api::upsert_contact` — the SAME function the public
//! `POST /api/external/contacts` endpoint uses. There is deliberately no second insert path: one
//! dedupe rule, one place where tenancy is enforced.

use std::collections::HashMap;

use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::errors::AppError;
use crate::external_api;
use crate::AppState;

use super::transforms::{self, Resolved};
use super::Mapping;

/// What the resolver produced for one row, before anything is written.
#[derive(Debug, Default)]
pub struct ResolvedPayload {
    /// Column values keyed by destination field (`first_name`, `email`, …).
    pub builtin: HashMap<String, String>,
    /// `metadata.<dotted path>` leaves that survived resolution.
    pub metadata: Vec<(String, String)>,
    /// Required destinations that produced nothing — each named, never silently dropped.
    pub refused: Vec<Value>,
    /// Tokens that resolved to nothing, for the Runs tab.
    pub missing_tokens: Vec<String>,
    /// The exact payload the writer would receive.
    pub preview: Value,
}

/// Resolve every `field_map` entry against `row`.
///
/// `skip_empty` decides what an empty resolution means: dropped (true) or written as `default`
/// (false). A `required` entry that ends up empty is always refused, with the field named.
pub fn resolve(mapping: &Mapping, row: &Value) -> ResolvedPayload {
    let skip_empty = mapping
        .options
        .get("skip_empty")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let mut out = ResolvedPayload::default();
    let mut meta_preview = Map::new();

    for entry in &mapping.field_map {
        let to = entry.to.trim();
        if to.is_empty() {
            continue;
        }
        let Resolved { value, missing } = transforms::resolve(&entry.template, row);
        for m in &missing {
            if !out.missing_tokens.contains(m) {
                out.missing_tokens.push(m.clone());
            }
        }

        // Transform first, then fall back to the declared default.
        let mut text = value
            .as_deref()
            .map(|v| transforms::apply_transform(entry.transform.as_deref(), v))
            .unwrap_or_default();

        // `regex_extract(pattern)` is parameterised and handled after the plain transforms.
        if let Some(t) = entry.transform.as_deref() {
            if t.trim().to_ascii_lowercase().starts_with("regex_extract(") {
                text = transforms::regex_extract(t, &text);
            }
        }
        let text = text.trim().to_string();

        let text = if text.is_empty() {
            match entry.default.clone() {
                Some(d) if !skip_empty => d,
                Some(d) => d,
                None => String::new(),
            }
        } else {
            text
        };

        if text.is_empty() {
            if entry.required {
                out.refused.push(json!({
                    "field": to,
                    "reason": "required but the source row produced nothing for it"
                }));
            } else if !skip_empty {
                out.refused.push(json!({
                    "field": to,
                    "reason": "resolved to nothing and skip_empty is off with no default"
                }));
            }
            continue;
        }

        if let Some(path) = to.strip_prefix("metadata.") {
            out.metadata.push((path.to_string(), text.clone()));
            set_nested(&mut meta_preview, path, Value::String(text));
        } else if to == "metadata" {
            out.refused.push(json!({
                "field": to,
                "reason": "target metadata needs a path, e.g. metadata.utm_source"
            }));
            continue;
        } else {
            out.builtin.insert(to.to_string(), text);
        }
    }

    let mut preview = Map::new();
    for (k, v) in &out.builtin {
        preview.insert(k.clone(), Value::String(v.clone()));
    }
    if !meta_preview.is_empty() {
        preview.insert("metadata".to_string(), Value::Object(meta_preview));
    }
    out.preview = Value::Object(preview);
    out
}

fn set_nested(target: &mut Map<String, Value>, path: &str, value: Value) {
    let mut parts = path.split('.').filter(|p| !p.is_empty()).peekable();
    let Some(first) = parts.next() else { return };
    if parts.peek().is_none() {
        target.insert(first.to_string(), value);
        return;
    }
    let entry = target
        .entry(first.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    if let Value::Object(inner) = entry {
        let rest = parts.collect::<Vec<_>>().join(".");
        set_nested(inner, &rest, value);
    }
}

/// Outcome of one run, as recorded in `integration_mapping_runs`.
pub struct RunOutcome {
    pub status: &'static str,
    pub contact_id: Option<Uuid>,
    pub refused: Vec<Value>,
    pub reason: Option<String>,
    pub payload: Value,
    pub ms: i64,
}

/// Run `mapping` against one source `row`.
///
/// `event_id` is the caller's idempotency key: a second call with the same `(mapping, event_id)` is
/// a recorded no-op, never a second contact. `dry_run` resolves and reports without writing.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    state: &AppState,
    mapping: &Mapping,
    row: &Value,
    event_id: Option<&str>,
    _source_row_id: Option<&str>,
    dry_run: bool,
) -> Result<RunOutcome, AppError> {
    let started = std::time::Instant::now();
    let tenant = mapping.tenant_id;

    // Idempotency first: a replay must cost nothing and change nothing.
    if let Some(eid) = event_id {
        let seen: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM integration_mapping_runs WHERE mapping_id = $1 AND event_id = $2",
        )
        .bind(mapping.id)
        .bind(eid)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?;
        if seen.is_some() {
            return Ok(RunOutcome {
                status: "replayed",
                contact_id: None,
                refused: vec![],
                reason: Some(format!(
                    "event '{eid}' was already processed for this mapping — nothing was written"
                )),
                payload: json!({}),
                ms: started.elapsed().as_millis() as i64,
            });
        }
    }

    let resolved = resolve(mapping, row);

    if !resolved.refused.is_empty() {
        let names: Vec<String> = resolved
            .refused
            .iter()
            .map(|r| {
                r.get("field")
                    .and_then(|f| f.as_str())
                    .unwrap_or("?")
                    .to_string()
            })
            .collect();
        return Ok(RunOutcome {
            status: "refused",
            contact_id: None,
            refused: resolved.refused.clone(),
            reason: Some(format!(
                "Refused before writing: {} {} empty",
                names.join(", "),
                if names.len() == 1 { "is" } else { "are" }
            )),
            payload: resolved.preview,
            ms: started.elapsed().as_millis() as i64,
        });
    }

    // Dedupe by the mapping's own keys, inside this tenant only. Two literal statements rather than
    // one built string: the query text stays static, which is what the parameterised-SQL rule wants.
    let on_match = mapping
        .dedupe
        .get("on_match")
        .and_then(|v| v.as_str())
        .unwrap_or("update");
    let mut existing: Option<Uuid> = None;
    for key in mapping.dedupe_keys() {
        let Some(value) = resolved.builtin.get(&key) else {
            continue;
        };
        let found: Option<(Uuid,)> = match key.as_str() {
            "email" => sqlx::query_as(
                "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
            )
            .bind(tenant)
            .bind(value)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?,
            "phone" => sqlx::query_as(
                "SELECT id FROM contacts WHERE tenant_id = $1 AND phone = $2 LIMIT 1",
            )
            .bind(tenant)
            .bind(value)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?,
            _ => None,
        };
        if let Some((id,)) = found {
            existing = Some(id);
            break;
        }
    }

    if existing.is_some() && on_match == "skip" {
        return Ok(RunOutcome {
            status: "skipped",
            contact_id: existing,
            refused: vec![],
            reason: Some(
                "a contact already matched this mapping's dedupe keys and on-match is 'skip'"
                    .into(),
            ),
            payload: resolved.preview,
            ms: started.elapsed().as_millis() as i64,
        });
    }

    if existing.is_some() && on_match == "create_new" {
        // The destination itself refuses a second contact with the same email
        // (unique index idx_contacts_tenant_email). Saying so is the honest answer; inventing a
        // second row or silently downgrading to an update would both be lies.
        let keys = mapping.dedupe_keys().join(", ");
        return Ok(RunOutcome {
            status: "refused",
            contact_id: existing,
            refused: vec![json!({
                "field": keys,
                "reason": "this account dedupes contacts on this key, so a second contact cannot exist — choose update or skip"
            })],
            reason: Some(
                "on-match is 'create_new' but the matched key is unique per account".into(),
            ),
            payload: resolved.preview,
            ms: started.elapsed().as_millis() as i64,
        });
    }

    if dry_run {
        return Ok(RunOutcome {
            status: if existing.is_some() {
                "updated"
            } else {
                "created"
            },
            contact_id: existing,
            refused: vec![],
            reason: Some("dry run — nothing was written".into()),
            payload: resolved.preview,
            ms: started.elapsed().as_millis() as i64,
        });
    }

    // ---- write, through the one contact writer ----
    let source_app = mapping
        .source
        .get("app")
        .and_then(|v| v.as_str())
        .unwrap_or("data-mapping");
    let source_raw = row.to_string();
    let contact_id =
        external_api::upsert_contact(state, tenant, &resolved.builtin, source_app, &source_raw)
            .await?;

    // metadata.<path> leaves land precisely, without clobbering sibling keys.
    for (path, value) in &resolved.metadata {
        let parts: Vec<String> = path
            .split('.')
            .filter(|p| !p.is_empty())
            .map(str::to_string)
            .collect();
        if parts.is_empty() {
            continue;
        }
        sqlx::query(
            "UPDATE contacts
                SET metadata = jsonb_set(COALESCE(metadata, '{}'::jsonb), $2::text[], to_jsonb($3::text), true),
                    updated_at = NOW()
              WHERE id = $1 AND tenant_id = $4",
        )
        .bind(contact_id)
        .bind(&parts)
        .bind(value)
        .bind(tenant)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;
    }

    // List membership uses the same helper the external endpoint uses.
    if let Some(list_id) = mapping.destination_list_id() {
        external_api::assign_list_membership(state, tenant, contact_id, list_id).await?;
    }

    let status = if existing.is_some() {
        "updated"
    } else {
        "created"
    };
    Ok(RunOutcome {
        status,
        contact_id: Some(contact_id),
        refused: vec![],
        reason: None,
        payload: resolved.preview,
        ms: started.elapsed().as_millis() as i64,
    })
}

/// Record one run. A unique index on `(mapping_id, event_id)` makes a concurrent replay collide
/// here instead of writing a second contact — the insert is simply skipped when that happens.
#[allow(clippy::too_many_arguments)]
pub async fn record(
    state: &AppState,
    mapping: &Mapping,
    outcome: &RunOutcome,
    event_id: Option<&str>,
    source_row_id: Option<&str>,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO integration_mapping_runs
            (id, tenant_id, mapping_id, event_id, source_row_id, status, contact_id, refused, reason, ms, payload)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
         ON CONFLICT (mapping_id, event_id) WHERE event_id IS NOT NULL DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(mapping.tenant_id)
    .bind(mapping.id)
    .bind(event_id)
    .bind(source_row_id)
    .bind(outcome.status)
    .bind(outcome.contact_id)
    .bind(json!(outcome.refused))
    .bind(outcome.reason.clone())
    .bind(outcome.ms as i32)
    .bind(outcome.payload.clone())
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_mapping::FieldMapEntry;

    fn mapping(field_map: Vec<FieldMapEntry>, on_match: &str) -> Mapping {
        Mapping {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            name: "t".into(),
            source: json!({"app":"funnelswift","entity":"leads","schema_version":1}),
            destination: json!({"entity":"contact","list_id":null}),
            field_map,
            dedupe: json!({"keys":["email"],"on_match":on_match}),
            options: json!({"skip_empty":true}),
            enabled: true,
        }
    }

    fn entry(to: &str, template: &str) -> FieldMapEntry {
        FieldMapEntry {
            to: to.into(),
            template: template.into(),
            transform: None,
            required: false,
            default: None,
        }
    }

    #[test]
    fn resolves_builtins_and_metadata() {
        let m = mapping(
            vec![
                entry("first_name", "{{first_name}}"),
                entry("email", "{{email}}"),
                entry("metadata.utm_source", "{{meta.utm_source}}"),
            ],
            "update",
        );
        let row = json!({"first_name":"Ann","email":"a@b.com","meta":{"utm_source":"google"}});
        let r = resolve(&m, &row);
        assert_eq!(r.fresh_first_name(), "Ann");
        assert_eq!(r.builtin.get("email").map(String::as_str), Some("a@b.com"));
        assert_eq!(
            r.metadata,
            vec![("utm_source".to_string(), "google".to_string())]
        );
        assert!(r.refused.is_empty());
    }

    #[test]
    fn required_empty_is_refused_and_named() {
        let mut e = entry("email", "{{email}}");
        e.required = true;
        let r = resolve(&mapping(vec![e], "update"), &json!({"first_name":"Ann"}));
        assert_eq!(r.refused.len(), 1);
        assert_eq!(r.refused[0]["field"], "email");
    }

    #[test]
    fn empty_optional_is_dropped_not_written() {
        let r = resolve(
            &mapping(vec![entry("company", "{{company}}")], "update"),
            &json!({"email":"a@b.com"}),
        );
        assert!(r.builtin.is_empty());
        assert!(r.refused.is_empty());
    }

    #[test]
    fn nested_metadata_paths_do_not_flatten_each_other() {
        let m = mapping(
            vec![
                entry("metadata.agent.name", "{{agent.name}}"),
                entry("metadata.agent.id", "{{agent.id}}"),
            ],
            "update",
        );
        let r = resolve(&m, &json!({"agent":{"name":"Ada","id":"7"}}));
        assert_eq!(r.preview["metadata"]["agent"]["name"], "Ada");
        assert_eq!(r.preview["metadata"]["agent"]["id"], "7");
    }

    impl ResolvedPayload {
        fn fresh_first_name(&self) -> &str {
            self.builtin
                .get("first_name")
                .map(String::as_str)
                .unwrap_or("")
        }
    }
}
