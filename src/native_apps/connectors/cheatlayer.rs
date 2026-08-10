//! CheatLayer Connector
//!
//! CheatLayer is a drag-and-drop RPA (Robotic Process Automation) tool
//! built with PySide6. It automates browser actions, scraping, form filling,
//! and custom workflows.
//!
//! Access: Admin-only (security-sensitive automation engine)

use std::collections::HashMap;

pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "CheatLayer API key is required".into()),
    };
    let base_url = match creds.get("base_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.trim_end_matches('/'),
        _ => return (false, "CheatLayer base URL is required".into()),
    };

    let url = format!("{}/api/health", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("X-API-Key", api_key)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => (true, "CheatLayer connection successful".into()),
        Ok(resp) => (
            false,
            format!("CheatLayer returned status {}", resp.status()),
        ),
        Err(e) => (false, format!("CheatLayer connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "workflow" | "job" | "template" => {
            let url = format!("{}/api/{}", base_url, entity_type);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("X-API-Key", &api_key)
                .json(data)
                .send()
                .await
                .map_err(|e| format!("CheatLayer push failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("CheatLayer response: {}", e))
        }
        _ => Err(format!(
            "CheatLayer does not support entity type: {}",
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
        "workflows" | "jobs" | "templates" | "logs" => {
            let url = format!("{}/{}{}", base_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("X-API-Key", &api_key)
                .send()
                .await
                .map_err(|e| format!("CheatLayer pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("CheatLayer response: {}", e))
        }
        _ => Err(format!(
            "CheatLayer does not support pulling entity type: {}",
            entity_type
        )),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "CheatLayer",
        "slug": "cheatlayer",
        "description": "Drag-and-drop RPA automation engine (PySide6)",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin",
        "entities": { "push": ["workflow", "job", "template"], "pull": ["workflows", "jobs", "templates", "logs"] },
        "features": [
            "Trigger RPA workflows from CRM automation rules",
            "Scrape external data and push results back as contacts/companies"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or("CheatLayer API key missing")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .ok_or("CheatLayer base URL missing")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
