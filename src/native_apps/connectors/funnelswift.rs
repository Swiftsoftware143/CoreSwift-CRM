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

    let url = format!("{}/v1/health", api_base(creds));
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "FunnelSwift connection successful".into())
        }
        Ok(resp) => (
            false,
            format!("FunnelSwift returned status {}", resp.status()),
        ),
        Err(e) => (false, format!("FunnelSwift connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let api_key = extract_key(creds)?;
    // Use the tenant's base_url from credentials (the Integration Center collects it),
    // then the legacy api_url key, then the default.
    let api_url = api_base(creds);

    match entity_type {
        "lead" | "contact" | "funnel" | "tag" => {
            let url = format!(
                "{}/v1/{}",
                api_url,
                if entity_type == "tag" {
                    "tags"
                } else {
                    entity_type
                }
            );
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .send()
                .await
                .map_err(|e| format!("FunnelSwift push failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("FunnelSwift response: {}", e))
        }
        _ => Err(format!(
            "FunnelSwift does not support entity type: {}",
            entity_type
        )),
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
        let params: Vec<String> = filters
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        format!("?{}", params.join("&"))
    };

    let api_url = api_base(creds);

    match entity_type {
        "leads" | "contacts" | "funnels" | "tags" => {
            let url = format!("{}/v1/{}{}", api_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .send()
                .await
                .map_err(|e| format!("FunnelSwift pull failed: {}", e))?;
            resp.json()
                .await
                .map_err(|e| format!("FunnelSwift response: {}", e))
        }
        _ => Err(format!(
            "FunnelSwift does not support pulling entity type: {}",
            entity_type
        )),
    }
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "FunnelSwift",
        "slug": "funnelswift",
        "description": "Mobile sales funnel builder (Expo/React Native)",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_tenant",
        // The declared entity vocabulary IS the implemented one — nothing more (kanban t_7225a94d).
        // This list used to advertise `product_selection` (push) and `affiliate_products` +
        // `my_products` (pull) while `push_entity()`/`pull_entity()` below matched only
        // lead|contact|funnel|tag and leads|contacts|funnels|tags — so all three fell to the
        // "does not support ..." arm. An advertised capability the connector itself refuses.
        //
        // MEASURED 2026-10-02 before removing them:
        //   * `grep -rn "affiliate_products|my_products|product_selection" src/` -> the declaration
        //     itself and nothing else: 0 reads, 0 writes, no route, no consumer surface.
        //   * `app_connections` = 0 rows and `app_sync_logs` = 0 rows in coreswift_crm: no tenant had
        //     ever connected a native app, so no pull or push had ever run for ANY entity.
        //   * the shipped console's sync modal drives its own list (contacts|lists|tags), so the
        //     declared names were never reachable from the UI either.
        //   * the names could not have resolved even if wired: FunnelSwift serves
        //     `/api/v1/affiliate-products` (hyphen) and has no `/api/v1/contacts` at all, while this
        //     connector builds `{base}/v1/{entity_type}` from the literal below.
        //
        // BUILDING THE SYNC WAS DECLINED, NOT DEFERRED. FunnelSwift owns the affiliate programme and
        // its commissionable catalogue (ARCHITECTURE.md Rules 2 and 4; migration 108 retires
        // CoreSwift's dead copy of that schema), the products are managed in the FunnelSwift admin
        // (docs/ADMIN_GUIDE.md), and CoreSwift has no surface that would show them — so wiring a pull
        // would mean inventing a product decision, not repairing a claim. The direction stays correct
        // if someone later builds a real consumer, in which case the declaration comes back together
        // with the arm that serves it.
        "entities": { "push": ["lead", "contact", "funnel", "tag"], "pull": ["leads", "contacts", "funnels", "tags"] },
        "features": ["Push leads from CRM into FunnelSwift funnels", "Pull completed funnels back into CRM as contacts"]
    })
}

fn extract_key(creds: &serde_json::Value) -> Result<String, String> {
    creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "FunnelSwift API key missing".into())
}

/// The API base this tenant's FunnelSwift answers on. The credential the tenant
/// enters in the Integration Center is authoritative (`base_url`); `api_url` is the
/// legacy key; the public host is only a fallback so a missing field degrades to a
/// failed test rather than a panic.
fn api_base(creds: &serde_json::Value) -> String {
    creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            creds
                .get("api_url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("https://api.funnelswift.app")
        .trim_end_matches('/')
        .to_string()
}
