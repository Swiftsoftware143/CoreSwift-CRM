//! AdaSwift Console Connector
//!
//! AdaSwift is a client viewing portal — clients see their reports, proposals,
//! and account status. This is the ONLY connector that replaces Mailgun/SMTP.com
//! for welcome emails and scan reports.
//!
//! When a new contact/client is created in CRM Swift, an automation rule can
//! trigger an Ada campaign (welcome email + scan report delivery).
//!
//! Access: Admin-only (AdaSwift is read-only portal for clients)

use std::collections::HashMap;

/// Test the AdaSwift connection
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "AdaSwift API key is required".into()),
    };
    let base_url = match creds.get("base_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.trim_end_matches('/'),
        _ => return (false, "AdaSwift base URL is required".into()),
    };

    let url = format!("{}/api/health", base_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => (true, "AdaSwift connection successful".into()),
        Ok(resp) => (false, format!("AdaSwift returned status {}", resp.status())),
        Err(e) => (false, format!("AdaSwift connection failed: {}", e)),
    }
}

/// Push entity to AdaSwift
pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "contact" | "client" => {
            let url = format!("{}/api/clients", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("AdaSwift push failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("AdaSwift response parse failed: {}", e))
        }
        "trigger_campaign" => {
            // Create a new client in AdaSwift AND trigger the welcome campaign
            let url = format!("{}/api/campaigns/trigger", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("AdaSwift campaign trigger failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("AdaSwift response parse failed: {}", e))
        }
        _ => Err(format!(
            "AdaSwift does not support entity type: {}",
            entity_type
        )),
    }
}

/// Pull entity from AdaSwift
pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let (api_key, base_url) = extract_creds(creds)?;

    match entity_type {
        "campaigns" => {
            let mut url = format!("{}/api/campaigns", base_url);
            if let Some(status) = filters.get("status") {
                url = format!("{}?status={}", url, status);
            }
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("AdaSwift pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("AdaSwift response parse failed: {}", e))
        }
        "reports" => {
            let client_id = filters
                .get("client_id")
                .ok_or("client_id filter required for reports")?;
            let url = format!("{}/api/clients/{}/reports", base_url, client_id);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("AdaSwift pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("AdaSwift response parse failed: {}", e))
        }
        _ => Err(format!(
            "AdaSwift does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Get app metadata
pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "AdaSwift Console",
        "slug": "adaswift",
        "description": "Client viewing portal — clients see reports, proposals, account status",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin",
        "entities": {
            "push": ["contact", "client", "trigger_campaign"],
            "pull": ["campaigns", "reports"]
        },
        "features": [
            "Create clients in AdaSwift from CRM contacts",
            "Trigger welcome campaigns on new client creation",
            "Trigger scan report delivery campaigns",
            "Pull campaign delivery status back into CRM Swift"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or("AdaSwift API key missing")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .ok_or("AdaSwift base URL missing")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
