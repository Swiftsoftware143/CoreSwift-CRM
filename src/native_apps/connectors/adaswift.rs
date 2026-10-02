//! AdaSwift Console Connector
//!
//! AdaSwift is a client viewing portal: an operator adds a client, AdaSwift scans the
//! client's domain and mails the client their scan report. The portal is where the CLIENT
//! signs in; the console is where the operator works.
//!
//! When a new contact/client is created in CRM Swift, an operator can push it into
//! AdaSwift as a client, and pull AdaSwift's scan reports back.
//!
//! Access: Admin-only (AdaSwift is platform-operated, not a per-tenant account).
//!
//! ── REAL ROUTES (measured live 2026-10-02, kanban t_8b81b1dd) ─────────────────────────
//! Every path this file built sat under AdaSwift's `/api/*`, which is OUTSIDE AdaSwift's
//! `/api/v1/*` router, so the router fallback answered an empty-bodied **404** to all five
//! of them — while a minted AdaSwift admin JWT's control (`GET /api/v1/dashboard/stats`)
//! answered **200**, proving the credential and client path:
//!
//!   test()                 GET  /api/health                -> 404
//!   push contact|client    POST /api/clients               -> 404
//!   push trigger_campaign  POST /api/campaigns/trigger     -> 404
//!   pull campaigns         GET  /api/campaigns             -> 404
//!   pull reports           GET  /api/clients/{id}/reports  -> 404
//!
//! `connect_app()` stores a connection only after `test_connection()` passes, so no AdaSwift
//! connection could ever be created (`app_connections` and `app_sync_logs` were both 0 rows),
//! and `GET /api/native/apps` advertised the five names above to every operator.
//!
//! AdaSwift's REAL surface, measured live with the same token:
//!   GET  /api/v1/clients       -> 200 {"clients":[…]}   (tenant-scoped)
//!   POST /api/v1/clients       -> the create-client handler (JSON `CreateClientRequest`)
//!   GET  /api/v1/scan-reports  -> 200 {"scan_reports":[…]}  (tenant-scoped)
//!
//! AdaSwift serves **no campaign entity at all** — its only campaigns handler is archived
//! (`src/handlers/.archive/campaigns_handler.rs`) — so `trigger_campaign` (push) and
//! `campaigns` (pull) are RETIRED, not re-pointed: a name with no counterpart on the far
//! side would be the same false promise this change exists to remove. The declaration in
//! `get_meta()` names only what a live request can reach (the rule the FunnelSwift connector
//! follows, kanban t_e8a7f651 / t_7225a94d).
//!
//! `api_key` holds an AdaSwift **bearer token** and `base_url` the deployment AdaSwift
//! answers on. AdaSwift issues no long-lived API key — its protected routes are JWT-only and
//! its login token lives 24 h — so `test()` probes an AUTHENTICATED route rather than
//! `/api/v1/health`: the health route is public and would answer 200 to any string, and
//! `connect_app()` would then store whatever the operator typed.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the AdaSwift connection.
///
/// Probes `GET {base}/api/v1/clients`, which sits behind AdaSwift's auth middleware: a
/// usable credential answers 200, a rejected one 401/403. Deliberately NOT `/api/v1/health`
/// — that route is public, so it would report success for any credential at all and
/// `connect_app()` stores exactly what its test accepted.
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let (api_key, base_url) = match extract_creds(creds) {
        Ok(c) => c,
        Err(e) => return (false, e),
    };

    let url = format!("{}/api/v1/clients", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => (true, "AdaSwift connection successful".into()),
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!("AdaSwift rejected the credential (HTTP {})", resp.status()),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "AdaSwift returned status {} — check the base URL points at the API",
                resp.status()
            ),
        ),
        Err(e) => (false, format!("AdaSwift connection failed: {}", e)),
    }
}

/// Push an entity into AdaSwift.
///
/// AdaSwift's own entity is a CLIENT (`POST /api/v1/clients`) and it has no contact entity,
/// so this accepts `client` only. `contact` and `trigger_campaign` are retired — see the
/// module header for the live measurement behind each.
pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "client" => {
            let url = format!("{}/api/v1/clients", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("AdaSwift push failed: {}", e))?;

            read_response(resp, "push").await
        }
        _ => Err(format!(
            "AdaSwift does not support entity type: {}",
            entity_type
        )),
    }
}

/// Pull an entity from AdaSwift.
///
/// AdaSwift's only report entity is the SCAN REPORT, listed tenant-wide at
/// `GET /api/v1/scan-reports` — there is no `/api/clients/{id}/reports`. That handler takes
/// no query extractor, so `filters` are forwarded as query parameters and do not narrow the
/// tenant-scoped list; they are passed through rather than silently dropped.
pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "reports" => {
            let mut url = format!("{}/api/v1/scan-reports", base_url);
            if !filters.is_empty() {
                let params: Vec<String> = filters
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                url = format!("{}?{}", url, params.join("&"));
            }
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("AdaSwift pull failed: {}", e))?;

            read_response(resp, "pull").await
        }
        _ => Err(format!(
            "AdaSwift does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Turn an AdaSwift response into the JSON the sync log/modal shows, refusing honestly.
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
        .map_err(|e| format!("AdaSwift response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "AdaSwift {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "AdaSwift response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

/// Get app metadata.
///
/// The declared entity vocabulary IS the implemented one — nothing more. It used to
/// advertise `contact` + `trigger_campaign` (push) and `campaigns` + `reports` (pull) while
/// every path behind them answered 404 (see the module header); `contact`, `trigger_campaign`
/// and `campaigns` had no AdaSwift counterpart to point at, and are gone.
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "AdaSwift Console",
        "slug": "adaswift",
        "description": "Client viewing portal — clients see their scan reports and account status",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin",
        "entities": {
            "push": ["client"],
            "pull": ["reports"]
        },
        "features": [
            "Create AdaSwift clients from CRM contacts",
            "Pull AdaSwift scan reports back into CRM Swift"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or("AdaSwift API key (bearer token) is required")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("AdaSwift base URL is required")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
