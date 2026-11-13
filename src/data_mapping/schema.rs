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
/// The fleet's own admin credential for reading ONE app's field description.
///
/// `/api/v1/integration/schema` is admin-only on every source app by design, and a customer account
/// must never hold an admin key on an app that is not its own. The read returns field NAMES, TYPES
/// and MASKED samples — never a tenant's rows — so it is made with the operator credential from this
/// deployment's environment: `FUNNELSWIFT_SCHEMA_KEY`, `INCENTIVESWIFT_SCHEMA_KEY`,
/// `WORKFLOWSWIFT_SCHEMA_KEY`, `ADASWIFT_SCHEMA_KEY`, `MISSEDCALL_SCHEMA_KEY`.
///
/// Unset is not fatal: the account's own key is tried next, and when neither works the error names
/// the exact environment variable to add.
fn operator_schema_key(app_slug: &str) -> Option<String> {
    let var = match app_slug {
        "funnelswift" => "FUNNELSWIFT_SCHEMA_KEY",
        "incentiveswift" => "INCENTIVESWIFT_SCHEMA_KEY",
        "workflowswift" => "WORKFLOWSWIFT_SCHEMA_KEY",
        "adaswift" => "ADASWIFT_SCHEMA_KEY",
        "missedcall" | "missedcall-responder" => "MISSEDCALL_SCHEMA_KEY",
        _ => return None,
    };
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

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

    // Two credentials are tried, in order: the fleet's operator schema key (the app's schema is
    // admin-only, and a CUSTOMER account must never hold an admin key on someone else's app), then
    // the account's own stored key (which works when it happens to be admin).
    let mut candidates: Vec<(&str, String)> = Vec::new();
    if let Some(k) = operator_schema_key(app_slug) {
        candidates.push(("operator", k));
    }
    candidates.push(("account", api_key.clone()));

    let mut last_error: Option<String> = None;
    let mut body: Option<Value> = None;

    for (source, key) in candidates {
        let resp = client
            .get(&url)
            .bearer_auth(&key)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| {
                AppError::BadRequest(format!(
                    "Could not reach {app_slug} at {base_url} to read its fields: {e}"
                ))
            })?;

        let code = resp.status().as_u16();
        if code == 200 {
            let parsed: Value = resp.json().await.map_err(|e| {
                AppError::BadRequest(format!(
                    "{app_slug} returned something that is not JSON: {e}"
                ))
            })?;
            body = Some(parsed);
            break;
        }

        let detail = resp.text().await.unwrap_or_default();
        let detail = detail.chars().take(160).collect::<String>();
        let env_var = format!(
            "{}_SCHEMA_KEY",
            app_slug.to_ascii_uppercase().replace('-', "_")
        );
        let sentence = match code {
            401 => format!(
                "{app_slug} rejected the {source} key. Regenerate it and save it in Integrations."
            ),
            403 => {
                if source == "operator" {
                    format!("{app_slug} refused the operator schema key — it is not an admin key there. Fix {env_var}.")
                } else {
                    format!(
                        "{app_slug} says this account's key is not an admin key, and field discovery needs one. No operator schema key is configured either, so add {env_var} to this deployment to list the fields."
                    )
                }
            }
            404 => format!(
                "{app_slug} does not describe its fields yet (no /api/v1/integration/schema). Nothing can be mapped from it until it does."
            ),
            _ => format!("{app_slug} answered {code} while listing its fields."),
        };
        last_error = Some(format!("{sentence} {detail}"));
    }

    let Some(body) = body else {
        return Err(AppError::BadRequest(last_error.unwrap_or_else(|| {
            format!("{app_slug} did not describe its fields.")
        })));
    };

    // The describe contract is `{app, version, entities:[{name,label,sample_count,fields:[…]}]}`.
    // Anything that does not carry entities is refused rather than shown as an empty list.
    if body.get("entities").and_then(|v| v.as_array()).is_none() {
        return Err(AppError::BadRequest(format!(
            "{app_slug} answered without an entities list — it is not speaking the describe contract."
        )));
    }

    Ok(body)
}
