//! FunnelSwift Connector
//!
//! FunnelSwift is a mobile (Expo/React Native) sales funnel builder.
//! Tenants connect their own FunnelSwift account to sync leads, funnels, and contacts.
//!
//! Access: Admin + Tenant

use std::collections::HashMap;

pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "FunnelSwift API key is required".into()),
    };

    let url = "https://api.funnelswift.app/v1/health";
    match reqwest::Client::new()
        .get(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "FunnelSwift connection successful".into())
        }
        Ok(resp) => (false, format!("FunnelSwift returned status {}", resp.status())),
        Err(e) => (false, format!("FunnelSwift connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let api_key = extract_key(creds)?;
    // Use api_url from credentials if provided, otherwise default
    let api_url = creds.get("api_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.funnelswift.app");

    match entity_type {
        "lead" | "contact" | "funnel" | "tag" => {
            let url = format!("{}/v1/{}", api_url, if entity_type == "tag" { "tags" } else { entity_type });
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("FunnelSwift push failed: {}", e))?;
            resp.json().await.map_err(|e| format!("FunnelSwift response: {}", e))
        }
        _ => Err(format!("FunnelSwift does not support entity type: {}", entity_type)),
    }
}

pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let api_key = extract_key(creds)?;
    let query = if filters.is_empty() {
        String::new()
    } else {
        let params: Vec<String> = filters.iter().map(|(k, v)| format!("{}={}", k, v)).collect();
        format!("?{}", params.join("&"))
    };

    let api_url = creds.get("api_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("https://api.funnelswift.app");

    match entity_type {
        "leads" | "contacts" | "funnels" | "tags" => {
            let url = format!("{}/v1/{}{}", api_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("FunnelSwift pull failed: {}", e))?;
            resp.json().await.map_err(|e| format!("FunnelSwift response: {}", e))
        }
        _ => Err(format!("FunnelSwift does not support pulling entity type: {}", entity_type)),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "FunnelSwift",
        "slug": "funnelswift",
        "description": "Mobile sales funnel builder (Expo/React Native)",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "webhook_secret"],
        "access_level": "admin_tenant",
        "entities": { "push": ["lead", "contact", "funnel", "tag", "product_selection"], "pull": ["leads", "contacts", "funnels", "tags", "affiliate_products", "my_products"] },
        "features": ["Push leads from CRM into FunnelSwift funnels", "Pull completed funnels back into CRM as contacts", "Affiliates select products to promote from FunnelSwift back-end", "Push product selections to sync with CRM Swift tags"]
    })
}

fn extract_key(creds: &serde_json::Value) -> Result<String, String> {
    creds.get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "FunnelSwift API key missing".into())
}
