//! IncentiveSwift Connector
//!
//! IncentiveSwift is the fleet's loyalty / rewards engine (Postgres backend): programmes,
//! campaigns, members, points, rewards and the CRM contacts those campaigns capture.
//!
//! ── REAL ROUTES (measured live 2026-11-11, kanban t_4d518841) ─────────────────────────
//! IncentiveSwift runs a flat `/api/v1/*` router behind a DEFAULT-DENY middleware
//! (`security::route_policy`): a matched route that is not on its public allowlist needs a
//! credential, and an IncentiveSwift-issued API key travels as
//! `Authorization: Bearer is_key_…`. Measured against `http://127.0.0.1:8083` with the
//! ZaarHub account's own key (`is_key_4wYM…`, active, sealed) — and with a bogus
//! `is_key_…` string as the negative control, on every path below:
//!
//!   GET  /api/v1/campaigns         -> 200 {"campaigns":[…]}      | bogus 401
//!   GET  /api/v1/contacts          -> 200 {"contacts":[…]}       | bogus 401
//!   GET  /api/v1/loyalty/programs  -> 200 {"programs":[…]}       | bogus 401
//!   GET  /api/v1/credits/balance   -> 200 {"balance":…}          | bogus 401
//!   POST /api/v1/contacts          -> the create handler         | bogus 401
//!
//! `POST /api/v1/api-keys/verify` is deliberately NOT used here: by contract it answers
//! **HTTP 200 for every well-formed request** (`{"valid": false}` for a key it rejects),
//! because the sibling services paste a key and read the body. This connector's contract is
//! the opposite — a rejected credential must answer 401/403 so `test()`'s refusal arm fires —
//! so `test()` probes the AUTHENTICATED `GET /api/v1/campaigns`, which answers 401 for a bad
//! key and returns the account's real campaigns for a good one.
//!
//! `api_key` holds an IncentiveSwift API key (minted at `POST /api/v1/api-keys`, prefix
//! `is_key_…`) and `base_url` the deployment's HOST (e.g. `https://app.incentiveswift.com`,
//! or `http://127.0.0.1:8083` for host networking on the fleet box): the connector builds
//! the `/api/v1/…` path itself, exactly as the Integration Center's base_url field describes.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the IncentiveSwift connection.
///
/// Probes `GET {base}/api/v1/campaigns`, which sits behind IncentiveSwift's default-deny
/// policy and its `AuthenticatedUser` extractor: a usable key answers 200 with the account's
/// own campaigns, a rejected one 401. Deliberately NOT `/api/v1/api-keys/verify`, which
/// answers 200 either way — see the module header.
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let (api_key, base_url) = match extract_creds(creds) {
        Ok(c) => c,
        Err(e) => return (false, e),
    };

    let url = format!("{}/api/v1/campaigns", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "IncentiveSwift connection successful".into())
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!(
                    "IncentiveSwift rejected the credential (HTTP {})",
                    resp.status()
                ),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "IncentiveSwift returned status {} — check the base URL points at the host, not a path",
                resp.status()
            ),
        ),
        Err(e) => (false, format!("IncentiveSwift connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        // A CRM contact becomes an IncentiveSwift contact, linked to the key's own account.
        // IncentiveSwift answers 422 for a contact carrying neither an email nor a phone.
        "contact" => {
            let url = format!("{}/api/v1/contacts", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("IncentiveSwift push failed: {}", e))?;
            read_response(resp, "push").await
        }
        _ => Err(format!(
            "IncentiveSwift does not support entity type: {}",
            entity_type
        )),
    }
}

pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    let path = match entity_type {
        "campaigns" => "/api/v1/campaigns",
        "contacts" => "/api/v1/contacts",
        "loyalty-programs" => "/api/v1/loyalty/programs",
        "credits" => "/api/v1/credits/balance",
        _ => {
            return Err(format!(
                "IncentiveSwift does not support pulling entity type: {}",
                entity_type
            ))
        }
    };

    let mut req = reqwest::Client::new()
        .get(format!("{}{}", base_url, path))
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    if !filters.is_empty() {
        req = req.query(filters);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("IncentiveSwift pull failed: {}", e))?;
    read_response(resp, "pull").await
}

/// Turn an IncentiveSwift response into the JSON the sync log/modal shows, refusing honestly.
///
/// A non-2xx used to be parsed as if it were a payload, so a refusal surfaced as a parser
/// error with the far end's own reason thrown away.
async fn read_response(
    resp: reqwest::Response,
    direction: &str,
) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("IncentiveSwift response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "IncentiveSwift {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "IncentiveSwift response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

/// Get app metadata.
///
/// The declared entity vocabulary IS the implemented one — every name below has a match arm
/// in `pull_entity`/`push_entity`, and every route it names was measured live.
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "IncentiveSwift",
        "slug": "incentiveswift",
        "description": "Loyalty and rewards engine — programmes, campaigns, members and points",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_tenant",
        "entities": {
            "push": ["contact"],
            "pull": ["campaigns", "contacts", "loyalty-programs", "credits"]
        },
        "features": [
            "Pull loyalty campaigns and programmes into CRM",
            "Push CRM contacts into a loyalty programme",
            "Read the account's credit balance"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or("IncentiveSwift API key is required")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("IncentiveSwift base URL is required")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
