//! MissedCall Responder Connector
//!
//! Callback Pro SaaS — React + TypeScript app for handling missed calls.
//! Features: SMS auto-reply with hybrid LLM suite, lead kanban board, tenant management,
//! BYOK (bring your own key) for SMS provider, event bus.
//!
//! CRM Swift pushes contacts/leads into MissedCall Responder and pulls
//! conversation history and lead status back.
//!
//! Access: Admin + Tenant
//!
//! ── REAL ROUTES (measured live 2026-10-02, kanban t_b43e4604) ─────────────────────────
//! Every path this file built sat under a bare `/api/*`, which MissedCall Responder does not
//! serve: its routes are mounted under `/api/v1/*`. A bare `/api/*` path never reaches the
//! router at all — MissedCall's auth middleware answers 401 "Missing authorization header"
//! to EVERY unauthenticated `/api/*` request, bogus paths included, so a 401 there is not
//! evidence of anything. The table below is the token-calibrated run: a minted super-admin
//! JWT whose control (`GET /api/v1/auth/me`) answered 200 and whose bogus-path control
//! (`GET /api/v1/zzz-bogus`) answered 401 without a token and **404** with one — proving the
//! router decides once a credential is present.
//!
//!   test()                GET  /api/health          -> 404 (real cred) / 401 (bogus, none)
//!   push lead             POST /api/lead            -> 404
//!   push contact          POST /api/contact         -> 404
//!   push tenant_config    POST /api/tenant_config   -> 404
//!   push sms_reply        POST /api/sms/send        -> 404
//!   pull leads            GET  /api/leads           -> 404
//!   pull conversations    GET  /api/conversations   -> 404
//!   pull call_logs        GET  /api/call_logs       -> 404
//!   pull tenant_settings  GET  /api/tenant_settings -> 404
//!
//! `connect_app()` stores a connection only after `test_connection()` passes, so no
//! MissedCall connection could ever be created and `GET /api/native/apps` advertised nine
//! names, none of which a live request could reach.
//!
//! MissedCall Responder's REAL surface, measured live with the same token:
//!   GET|POST /api/v1/leads      -> 200 [] / 400 (needs `name`)
//!   GET|POST /api/v1/contacts   -> 200 [] / 400 (needs `name`)
//!   GET      /api/v1/messages   -> 200 []          (the conversation history)
//!   GET      /api/v1/call-logs  -> 200 []          (note the HYPHEN)
//!
//! Retired rather than re-pointed, because the far side serves no such entity:
//! `tenant_config` (push) and `tenant_settings` (pull) — MissedCall's SMS-provider
//! configuration is BYOK (`/api/v1/provider-keys`) and its Telnyx config is a
//! platform-admin surface, not a tenant entity; `sms_reply` (push) — there is no
//! `/api/v1/sms/send`, SMS replies are driven by `POST /api/v1/calls/{id}/respond` on a
//! specific call; `conversations` (pull) — the far side calls the same thing `messages`,
//! and the declaration must name a route the target serves.
//!
//! `api_key` holds a MissedCall **bearer token** and `base_url` the deployment's HOST
//! (e.g. `https://app.missedcallrespondr.com`). MissedCall issues no durable API key — its
//! protected routes are JWT-only — so `test()` probes the AUTHENTICATED `GET /api/v1/leads`
//! rather than a health route: `/api/v1/health` is public and would answer 200 to any
//! string, and `connect_app()` stores whatever its test accepted.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the MissedCall Responder connection.
///
/// Probes `GET {base}/api/v1/leads`, which sits behind MissedCall's auth middleware: a
/// usable credential answers 200, a rejected one 401/403. Deliberately NOT
/// `/api/v1/health` — see the module header.
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let (api_key, base_url) = match extract_creds(creds) {
        Ok(c) => c,
        Err(e) => return (false, e),
    };

    let url = format!("{}/api/v1/leads", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "MissedCall Responder connection successful".into())
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!(
                    "MissedCall Responder rejected the credential (HTTP {})",
                    resp.status()
                ),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "MissedCall Responder returned status {} — check the base URL points at the host, not a path",
                resp.status()
            ),
        ),
        Err(e) => (
            false,
            format!("MissedCall Responder connection failed: {}", e),
        ),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        // MissedCall's own collection names are plural; both accept POST to create.
        "lead" => {
            let url = format!("{}/api/v1/leads", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("MissedCall push failed: {}", e))?;
            read_response(resp, "push").await
        }
        "contact" => {
            let url = format!("{}/api/v1/contacts", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("MissedCall push failed: {}", e))?;
            read_response(resp, "push").await
        }
        _ => Err(format!(
            "MissedCall Responder does not support entity type: {}",
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
        // `call_logs` maps to the far side's hyphenated `/api/v1/call-logs`.
        "leads" | "contacts" | "messages" | "call_logs" => {
            let path = match entity_type {
                "call_logs" => "call-logs",
                other => other,
            };
            let url = format!("{}/api/v1/{}{}", base_url, path, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("MissedCall pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        _ => Err(format!(
            "MissedCall Responder does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Turn a MissedCall response into the JSON the sync log/modal shows, refusing honestly.
///
/// A non-2xx used to be parsed as if it were a payload, so a refusal surfaced as a parser
/// error ("expected value at line 1 column 1") with the far end's own reason thrown away.
async fn read_response(
    resp: reqwest::Response,
    direction: &str,
) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("MissedCall response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "MissedCall {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "MissedCall response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

/// Get app metadata.
///
/// The declared entity vocabulary IS the implemented one — nothing more. It used to
/// advertise `tenant_config`/`sms_reply` (push) and `conversations`/`tenant_settings` (pull)
/// while every path behind them answered 404 (kanban t_b43e4604). `contacts` (pull) is newly
/// declared because the far side serves `GET /api/v1/contacts` (measured 200), completing the
/// round-trip the push side already has.
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "MissedCall Responder",
        "slug": "missedcall-responder",
        "description": "Callback Pro SaaS — missed call handling with SMS auto-reply, hybrid LLM suite, lead kanban",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_tenant",
        "entities": {
            "push": ["lead", "contact"],
            "pull": ["leads", "contacts", "messages", "call_logs"]
        },
        "features": [
            "Push qualified CRM contacts as leads into MissedCall Responder",
            "Pull conversation history for enrichment scoring",
            "Sync lead and call-log status back into CRM"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or("MissedCall Responder API key (bearer token) is required")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("MissedCall Responder base URL is required")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
