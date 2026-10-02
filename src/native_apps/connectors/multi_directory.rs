//! Multi-Directory App Connector
//!
//! The multi-tenant business directory system that matches the "Flawless Follow-up"
//! design — SaaS + Directory + Agency business units. Businesses get listed across
//! multiple directories with automated follow-up sequences.
//!
//! This connector reads the directory's own data into CRM Swift: listings (the directory's
//! business collection), reviews and analytics.
//!
//! Access: Admin only — internal tool for creating directories, not sold to tenants
//!
//! ── REAL ROUTES (measured live 2026-10-02, kanban t_b43e4604) ─────────────────────────
//! Every path this file built sat under a bare `/api/*`, which Multi-Directory does not
//! serve: its routes are mounted under `/api/v1/*` and everything else falls through to the
//! SPA. Measured against `http://127.0.0.1:8089` with a minted Multi-Directory super_admin
//! JWT whose control (`GET /api/v1/auth/me`) answered 200 and whose bogus-path control
//! (`GET /api/v1/zzz-bogus`) answered 404 in both directions:
//!
//!   test()                 GET  /api/health          -> 404 (real cred, bogus cred, no cred)
//!   push business          POST /api/business        -> 404
//!   push listing           POST /api/listing         -> 404
//!   push review_response   POST /api/review_response -> 404
//!   push followup_rule     POST /api/followup_rule   -> 404
//!   pull businesses        GET  /api/businesses      -> 404
//!   pull listings          GET  /api/listings        -> 404
//!   pull reviews           GET  /api/reviews         -> 404
//!   pull analytics         GET  /api/analytics       -> 404
//!   pull followup_status   GET  /api/followup_status -> 404
//!
//! `connect_app()` stores a connection only after `test_connection()` passes, so no
//! Multi-Directory connection could ever be created; on the PUBLIC host the same test path
//! answers 404 from the app, so it failed there too.
//!
//! Multi-Directory's REAL surface, measured live with the same token:
//!   GET /api/v1/health           -> 200 {"service":"multidirectory-api"}   (public)
//!   GET /api/v1/listings         -> 200 […]  the business collection        (public)
//!   GET /api/v1/reviews          -> 200 {"data":[…]}                        (public)
//!   GET /api/v1/analytics/summary-> 200 {"total_deal_claims":…}             (auth-gated)
//!   GET /api/v1/api-keys         -> 200 []                                  (auth-gated)
//!
//! Two honesty notes. `/api/v1/listings` and `/api/v1/reviews` are PUBLIC: measured with a
//! bogus bearer they answer the same 200 as with a real one, so the credential is not
//! checked on them. And the push direction is retired entirely: the only business create
//! route is `POST /api/v1/directories/{slug}/businesses`, which needs a directory slug that
//! the credentials the Integration Center collects (`api_key`, `base_url`) do not carry —
//! inventing a slug contract would be a product decision, not a repair (the FunnelSwift
//! precedent declined exactly that move). `businesses` (pull) is retired for the same reason
//! the rule exists: there is no `/api/v1/businesses` route — the far side's business
//! collection IS `/api/v1/listings`. `followup_status` has no route or table on the far side
//! at all.
//!
//! `api_key` holds a Multi-Directory **bearer token** and `base_url` the deployment's HOST
//! (e.g. `https://directory.swiftsoftware.net`). `test()` probes the AUTHENTICATED
//! `GET /api/v1/analytics/summary` — never `/api/v1/health` (public) and never the two
//! public collections, which would accept any string and let `connect_app()` store whatever
//! its test accepted.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the Multi-Directory connection.
///
/// Probes `GET {base}/api/v1/analytics/summary`, which is auth-gated: a usable credential
/// answers 200, a rejected one 401/403 (measured). Deliberately NOT `/api/v1/health`
/// (public, 200 to any string) and NOT the public `/api/v1/listings` / `/api/v1/reviews`.
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let (api_key, base_url) = match extract_creds(creds) {
        Ok(c) => c,
        Err(e) => return (false, e),
    };

    let url = format!("{}/api/v1/analytics/summary", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "Multi-Directory connection successful".into())
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!(
                    "Multi-Directory rejected the credential (HTTP {})",
                    resp.status()
                ),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "Multi-Directory returned status {} — check the base URL points at the host, not a path",
                resp.status()
            ),
        ),
        Err(e) => (false, format!("Multi-Directory connection failed: {}", e)),
    }
}

/// Multi-Directory serves no caller-reachable create route for the credentials the
/// Integration Center collects, so the whole push direction is retired — see the module
/// header. Kept as an explicit arm so the refusal carries this app's own reason.
pub async fn push_entity(
    _creds: &serde_json::Value,
    entity_type: &str,
    _data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    Err(format!(
        "Multi-Directory does not support entity type: {}",
        entity_type
    ))
}

pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    let query = if filters.is_empty() {
        String::new()
    } else {
        let params: Vec<String> = filters
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        format!("?{}", params.join("&"))
    };

    match entity_type {
        // `listings` is the far side's business collection (`businesses::list_all_businesses`);
        // `analytics` is its summary view.
        "listings" | "reviews" => {
            let url = format!("{}/api/v1/{}{}", base_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("Multi-Directory pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        "analytics" => {
            let url = format!("{}/api/v1/analytics/summary{}", base_url, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("Multi-Directory pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        _ => Err(format!(
            "Multi-Directory does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Turn a Multi-Directory response into the JSON the sync log/modal shows, refusing honestly.
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
        .map_err(|e| format!("Multi-Directory response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "Multi-Directory {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "Multi-Directory response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

/// Get app metadata.
///
/// The declared entity vocabulary IS the implemented one — nothing more. The push list is
/// EMPTY: no route a live request can reach can create anything (kanban t_b43e4604), so the
/// sync modal disables Push instead of offering a name this connector would refuse.
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "Multi-Directory App",
        "slug": "multi-directory",
        "description": "Multi-tenant business directory system — pull listings, reviews and directory analytics into CRM",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin",
        "entities": {
            "push": [],
            "pull": ["listings", "reviews", "analytics"]
        },
        "features": [
            "Pull directory listings (businesses) into CRM",
            "Pull reviews and reputation data from the directory",
            "Pull directory analytics for enrichment scoring"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or("Multi-Directory API key (bearer token) is required")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("Multi-Directory base URL is required")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
