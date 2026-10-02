//! WorkflowSwift Automation Connector
//!
//! WorkflowSwift is the n8n-based workflow automation engine (Postgres backend). It runs
//! scheduled workflows with credit tracking, custom workflow builder, and portfolio management.
//!
//! Accounts connect their WorkflowSwift instance and can trigger/pull workflows.
//!
//! Access: Admin + Account
//!
//! ── REAL ROUTES (measured live 2026-10-02, kanban t_b43e4604) ─────────────────────────
//! Every path this file built sat at the target's bare `/health` or under a bare `/api/*`,
//! neither of which WorkflowSwift serves: its routers are mounted under `/api/v1/*` and its
//! only bare-path route is `/`. Measured against `http://127.0.0.1:8085` with a minted
//! WorkflowSwift super_admin JWT whose control (`GET /api/v1/auth/me`) answered 200 and
//! whose bogus-path control (`GET /api/v1/zzz-bogus`) answered 404 with the same token:
//!
//!   test()                GET  /health          -> 404 (real cred, bogus cred and no cred alike)
//!   push workflow         POST /api/workflow    -> 404
//!   push trigger          POST /api/trigger     -> 404
//!   pull workflows        GET  /api/workflows   -> 404
//!   pull runs             GET  /api/runs        -> 404
//!   pull credits          GET  /api/credits     -> 404
//!
//! `connect_app()` stores a connection only after `test_connection()` passes, so no
//! WorkflowSwift connection could ever be created against the fleet's own deployment. On the
//! PUBLIC host the same test path answers 200 — but with the SPA shell, `text/html`, for ANY
//! credential (measured `https://app.workflowswift.com/health` -> 200 text/html with a bogus
//! bearer), so the connection test was vacuous there instead of failing.
//!
//! WorkflowSwift's REAL surface, measured live with the same token:
//!   GET  /api/v1/workflows         -> 200 {"workflows":[…]}                 (tenant-scoped)
//!   POST /api/v1/workflows         -> the create handler (422: needs `name`)
//!   POST /api/v1/workflows/trigger -> the trigger handler (422: needs `workflow_id`)
//!   GET  /api/v1/instances         -> 200 {"instances":[…]}                 (the run history)
//!   GET  /api/v1/credits/balance   -> 200 {"available":…,"balance":…}
//!   GET  /api/v1/api-keys          -> 200 {"api_keys":[]}                   (durable keys)
//!
//! The pull name `runs` is declared as `instances` — WorkflowSwift's own name for a run —
//! because the declaration must name routes the target serves (`/api/v1/runs` does not
//! exist). `api_key` holds a WorkflowSwift API key (the app mints them at `/api/v1/api-keys`)
//! and `base_url` the deployment's HOST (e.g. `https://app.workflowswift.com`): the connector
//! builds the `/api/v1/…` path itself, exactly as the Integration Center's base_url field
//! describes it. `test()` probes the AUTHENTICATED `GET /api/v1/workflows`, never a health
//! route — a public or static 200 answers any string and `connect_app()` stores whatever its
//! test accepted.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the WorkflowSwift connection.
///
/// Probes `GET {base}/api/v1/workflows`, which sits behind WorkflowSwift's auth middleware:
/// a usable credential answers 200, a rejected one 401/403. Deliberately NOT a health route —
/// see the module header.
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let (api_key, base_url) = match extract_creds(creds) {
        Ok(c) => c,
        Err(e) => return (false, e),
    };

    let url = format!("{}/api/v1/workflows", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "WorkflowSwift connection successful".into())
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!("WorkflowSwift rejected the credential (HTTP {})", resp.status()),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "WorkflowSwift returned status {} — check the base URL points at the host, not a path",
                resp.status()
            ),
        ),
        Err(e) => (false, format!("WorkflowSwift connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        // `workflow` creates one; `trigger` fires a deployed workflow.
        "workflow" => {
            let url = format!("{}/api/v1/workflows", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift push failed: {}", e))?;
            read_response(resp, "push").await
        }
        "trigger" => {
            let url = format!("{}/api/v1/workflows/trigger", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift trigger failed: {}", e))?;
            read_response(resp, "push").await
        }
        _ => Err(format!(
            "WorkflowSwift does not support entity type: {}",
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
        // `credits` is WorkflowSwift's balance view, not a bare `/credits` collection.
        "workflows" => {
            let url = format!("{}/api/v1/workflows{}", base_url, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        "instances" => {
            let url = format!("{}/api/v1/instances{}", base_url, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        "credits" => {
            let url = format!("{}/api/v1/credits/balance{}", base_url, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        _ => Err(format!(
            "WorkflowSwift does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Turn a WorkflowSwift response into the JSON the sync log/modal shows, refusing honestly.
///
/// A non-2xx used to be parsed as if it were a payload, so a refusal (or an HTML SPA shell)
/// surfaced as a parser error with the far end's own reason thrown away.
async fn read_response(
    resp: reqwest::Response,
    direction: &str,
) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("WorkflowSwift response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "WorkflowSwift {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "WorkflowSwift response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

/// Get app metadata.
///
/// The declared entity vocabulary IS the implemented one — nothing more. It used to advertise
/// `runs`, whose route `/api/runs` answers 404 (kanban t_b43e4604).
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "WorkflowSwift Automation",
        "slug": "workflowswift",
        "description": "n8n-based workflow automation engine (Postgres backend)",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_account",
        "entities": { "push": ["workflow", "trigger"], "pull": ["workflows", "instances", "credits"] },
        "features": [
            "Trigger n8n workflows from CRM automation rules",
            "Pull workflow execution results into CRM",
            "Track credit usage across workflows"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .ok_or("WorkflowSwift API key is required")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .ok_or("WorkflowSwift base URL is required")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
