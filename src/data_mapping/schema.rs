//! Field discovery for the mapper: what the destination accepts, and what the source describes.
//!
//! Destination side is a fixed catalogue derived from the `contacts` table — the mapper must never
//! offer a field the writer cannot store, and must never silently drop one (spec §5, §7.7).
//!
//! Source side is a proxy to the sibling app's `GET /api/v1/integration/schema` (spec §2). Field
//! lists are never hardcoded here: if the app cannot describe itself the mapper says so and offers
//! nothing, rather than inventing names that would resolve to nothing at run time.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::errors::AppError;
use crate::native_apps::credentials;
use crate::AppState;

use super::transforms::TRANSFORMS;

/// Every contact column the writer will accept, with the type the UI renders.
/// `(path, label, type)` — `required` is decided per mapping, not here.
pub const CONTACT_FIELDS: &[(&str, &str, &str)] = &[
    ("first_name", "First name", "string"),
    ("last_name", "Last name", "string"),
    ("email", "Email", "email"),
    ("phone", "Phone", "phone"),
    ("company", "Company", "string"),
    ("job_title", "Job title", "string"),
    ("title", "Title (legacy job_title)", "string"),
    ("city", "City", "string"),
    ("state", "State", "string"),
    ("country", "Country", "string"),
    ("postal_code", "Postal code", "string"),
    ("address_line1", "Address line 1", "string"),
    ("address_line2", "Address line 2", "string"),
    ("notes", "Notes", "string"),
    ("source", "Source", "string"),
    ("gender", "Gender", "string"),
];

/// Sibling apps the mapper can read from. The describe endpoint decides what they actually expose.
pub const SOURCE_APPS: &[(&str, &str)] = &[
    ("funnelswift", "FunnelSwift"),
    ("incentiveswift", "IncentiveSwift"),
    ("workflowswift", "WorkflowSwift"),
    ("missedcall-responder", "MissedCall Respondr"),
];

/// Is `to` a field the contact writer can store? `metadata.<path>` is open-ended by design — that is
/// how a mapping adds data without a migration.
pub fn is_writable_destination(to: &str) -> bool {
    let to = to.trim();
    if to == "metadata" {
        return true;
    }
    if let Some(rest) = to.strip_prefix("metadata.") {
        return !rest.trim().is_empty() && !rest.contains("..");
    }
    CONTACT_FIELDS.iter().any(|(path, _, _)| *path == to)
}

/// The destination catalogue: what can be written, what cannot (with the reason), the transform
/// vocabulary, the on-match policies and this tenant's lists.
pub async fn destinations(state: &AppState, tenant: Uuid) -> Result<Value, AppError> {
    let lists: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM lists WHERE tenant_id = $1 AND is_active ORDER BY name",
    )
    .bind(tenant)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let fields: Vec<Value> = CONTACT_FIELDS
        .iter()
        .map(|(path, label, ty)| json!({"path": path, "label": label, "type": ty}))
        .collect();

    Ok(json!({
        "entities": [
            {
                "entity": "contact",
                "label": "Contact",
                "available": true,
                "fields": fields,
                "open_prefix": "metadata.",
                "open_note": "Any metadata.<path> is accepted — extra fields never need a migration."
            },
            {
                "entity": "company",
                "label": "Company",
                "available": false,
                "reason": "Company mapping ships after contacts — this milestone writes contacts only (spec §7.8)."
            }
        ],
        "list_membership": {
            "available": true,
            "field": "destination.list_id",
            "lists": lists.into_iter().map(|(id, name)| json!({"id": id, "name": name})).collect::<Vec<_>>()
        },
        "transforms": TRANSFORMS,
        "on_match": ["update", "skip", "create_new"],
        "sources": SOURCE_APPS.iter().map(|(k, l)| json!({"app": k, "label": l})).collect::<Vec<_>>()
    }))
}

/// Fetch a sibling app's field description using this tenant's own stored connection key.
///
/// Every failure is returned as a sentence the operator can act on — an empty field list with no
/// reason is how a mapper ends up silently mapping nothing.
pub async fn source_schema(
    state: &AppState,
    tenant: Uuid,
    app_slug: &str,
) -> Result<Value, AppError> {
    let row: Option<(Value, String)> = sqlx::query_as(
        "SELECT credentials, status FROM app_connections WHERE tenant_id = $1 AND app_slug = $2",
    )
    .bind(tenant)
    .bind(app_slug)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let Some((credentials_value, status)) = row else {
        return Err(AppError::BadRequest(format!(
            "This account has no {app_slug} connection. Connect it in Integrations first — the mapper reads that app's real fields, it never guesses them."
        )));
    };
    if status != "connected" {
        return Err(AppError::BadRequest(format!(
            "The {app_slug} connection is '{status}'. Reconnect it before mapping."
        )));
    }

    let opened = credentials::open_for_use(tenant, &credentials_value)?;
    let api_key = opened
        .get("api_key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let base_url = opened
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();

    if api_key.is_empty() || base_url.is_empty() {
        return Err(AppError::BadRequest(format!(
            "The {app_slug} connection is missing its key or address. Reconnect it."
        )));
    }

    let url = format!("{base_url}/api/v1/integration/schema");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| AppError::Internal(format!("http client: {e}")))?;

    let resp = client
        .get(&url)
        .bearer_auth(&api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| {
            AppError::BadRequest(format!(
                "Could not reach {app_slug} at {base_url} to read its fields: {e}"
            ))
        })?;

    let code = resp.status().as_u16();
    if code != 200 {
        let detail = resp.text().await.unwrap_or_default();
        let detail = detail.chars().take(200).collect::<String>();
        let sentence = match code {
            401 => format!(
                "{app_slug} rejected this account's key. Regenerate it in {app_slug} and save it in Integrations."
            ),
            403 => format!(
                "{app_slug} says this key is not an admin key. Field discovery needs an admin key on that app."
            ),
            404 => format!(
                "{app_slug} does not describe its fields yet (no /api/v1/integration/schema). Nothing can be mapped from it until it does."
            ),
            _ => format!("{app_slug} answered {code} while listing its fields."),
        };
        return Err(AppError::BadRequest(format!("{sentence} {detail}")));
    }

    let body: Value = resp.json().await.map_err(|e| {
        AppError::BadRequest(format!(
            "{app_slug} returned something that is not JSON: {e}"
        ))
    })?;

    // The describe contract is `{app, version, entities:[{name,label,sample_count,fields:[…]}]}`.
    // Anything that does not carry entities is refused rather than shown as an empty list.
    if body.get("entities").and_then(|v| v.as_array()).is_none() {
        return Err(AppError::BadRequest(format!(
            "{app_slug} answered without an entities list — it is not speaking the describe contract."
        )));
    }

    Ok(body)
}
