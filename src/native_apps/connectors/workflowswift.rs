//! WorkflowSwift Automation Connector
//!
//! WorkflowSwift is the n8n-based workflow automation engine with a Supabase backend.
//! It runs scheduled workflows with credit tracking, custom workflow builder,
//! and portfolio management.
//!
//! Accounts connect their WorkflowSwift instance and can trigger/pull workflows.
//!
//! Access: Admin + Account

use std::collections::HashMap;

pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "WorkflowSwift API key is required".into()),
    };
    let base_url = match creds.get("base_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.trim_end_matches('/'),
        _ => return (false, "WorkflowSwift base URL is required".into()),
    };

    let url = format!("{}/health", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "WorkflowSwift connection successful".into())
        }
        Ok(resp) => (false, format!("WorkflowSwift returned status {}", resp.status())),
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
        "workflow" | "trigger" => {
            let url = format!("{}/api/{}", base_url, entity_type);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift push failed: {}", e))?;
            resp.json().await.map_err(|e| format!("WorkflowSwift response: {}", e))
        }
        _ => Err(format!("WorkflowSwift does not support entity type: {}", entity_type)),
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
        let params: Vec<String> = filters.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        format!("?{}", params.join("&"))
    };

    match entity_type {
        "workflows" | "runs" | "credits" => {
            let url = format!("{}/api/{}{}", base_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("WorkflowSwift pull failed: {}", e))?;
            resp.json().await.map_err(|e| format!("WorkflowSwift response: {}", e))
        }
        _ => Err(format!("WorkflowSwift does not support pulling entity type: {}", entity_type)),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "WorkflowSwift Automation",
        "slug": "workflowswift",
        "description": "n8n-based workflow automation engine with Supabase backend",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_account",
        "entities": { "push": ["workflow", "trigger"], "pull": ["workflows", "runs", "credits"] },
        "features": [
            "Trigger n8n workflows from CRM automation rules",
            "Pull workflow execution results into CRM",
            "Track credit usage across workflows"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds.get("api_key").and_then(|v| v.as_str()).ok_or("WorkflowSwift API key missing")?.to_string();
    let base_url = creds.get("base_url").and_then(|v| v.as_str()).ok_or("WorkflowSwift base URL missing")?.trim_end_matches('/').to_string();
    Ok((api_key, base_url))
}
