//! Multi-Directory App Connector
//!
//! The multi-tenant business directory system that matches the "Flawless Follow-up"
//! design — SaaS + Directory + Agency business units. Businesses get listed across
//! multiple directories with automated follow-up sequences.
//!
//! This uses CRM Swift's own business_profiles table and followup_queue internally,
//! but with a separate API surface for directory-specific operations like:
//! - Listing management across directories
//! - Reputation monitoring
//! - Automated follow-up based on directory activity
//!
//! Access: Admin only — internal tool for creating directories, not sold to tenants

use std::collections::HashMap;

pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "Multi-Directory API key is required".into()),
    };
    let base_url = match creds.get("base_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.trim_end_matches('/'),
        _ => return (false, "Multi-Directory base URL is required".into()),
    };

    let url = format!("{}/api/health", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "Multi-Directory connection successful".into())
        }
        Ok(resp) => (
            false,
            format!("Multi-Directory returned status {}", resp.status()),
        ),
        Err(e) => (false, format!("Multi-Directory connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "business" | "listing" | "review_response" | "followup_rule" => {
            let url = format!("{}/api/{}", base_url, entity_type);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("Directory push failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("Directory response: {}", e))
        }
        _ => Err(format!(
            "Multi-Directory does not support entity type: {}",
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
        "businesses" | "listings" | "reviews" | "analytics" | "followup_status" => {
            let url = format!("{}/api/{}{}", base_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("Directory pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("Directory response: {}", e))
        }
        _ => Err(format!(
            "Multi-Directory does not support pulling entity type: {}",
            entity_type
        )),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "Multi-Directory App",
        "slug": "multi-directory",
        "description": "Multi-tenant business directory system with automated follow-up across directories",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin",
        "entities": {
            "push": ["business", "listing", "review_response", "followup_rule"],
            "pull": ["businesses", "listings", "reviews", "analytics", "followup_status"]
        },
        "features": [
            "Sync business profiles across multiple directories",
            "Push review responses triggered by CRM automation",
            "Pull directory analytics for enrichment scoring",
            "Manage follow-up rules per directory listing"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or("Multi-Directory API key missing")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .ok_or("Multi-Directory base URL missing")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
