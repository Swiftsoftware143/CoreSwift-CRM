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

use std::collections::HashMap;

pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match creds.get("api_key").and_then(|v| v.as_str()) {
        Some(k) if !k.is_empty() => k,
        _ => return (false, "MissedCall Responder API key is required".into()),
    };
    let base_url = match creds.get("base_url").and_then(|v| v.as_str()) {
        Some(u) if !u.is_empty() => u.trim_end_matches('/'),
        _ => return (false, "MissedCall Responder base URL is required".into()),
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
            (true, "MissedCall Responder connection successful".into())
        }
        Ok(resp) => (
            false,
            format!("MissedCall Responder returned status {}", resp.status()),
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
        "lead" | "contact" | "tenant_config" => {
            let url = format!("{}/api/{}", base_url, entity_type);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("MissedCall push failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("MissedCall response: {}", e))
        }
        "sms_reply" => {
            // Trigger an automated SMS reply through MissedCall Responder's engine
            let url = format!("{}/api/sms/send", base_url);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("MissedCall SMS trigger failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("MissedCall response: {}", e))
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
        "leads" | "conversations" | "call_logs" | "tenant_settings" => {
            let url = format!("{}/api/{}{}", base_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("MissedCall pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("MissedCall response: {}", e))
        }
        _ => Err(format!(
            "MissedCall Responder does not support pulling entity type: {}",
            entity_type
        )),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "MissedCall Responder",
        "slug": "missedcall-responder",
        "description": "Callback Pro SaaS — missed call handling with SMS auto-reply, hybrid LLM suite, lead kanban",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_tenant",
        "entities": {
            "push": ["lead", "contact", "tenant_config", "sms_reply"],
            "pull": ["leads", "conversations", "call_logs", "tenant_settings"]
        },
        "features": [
            "Push qualified CRM contacts as leads into MissedCall Responder",
            "Pull conversation history for enrichment scoring",
            "Trigger automated SMS replies from CRM automation rules",
            "Sync tenant SMS provider settings between systems"
        ]
    })
}

fn extract_creds(creds: &serde_json::Value) -> Result<(String, String), String> {
    let api_key = creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .ok_or("MissedCall Responder API key missing")?
        .to_string();
    let base_url = creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .ok_or("MissedCall Responder base URL missing")?
        .trim_end_matches('/')
        .to_string();
    Ok((api_key, base_url))
}
