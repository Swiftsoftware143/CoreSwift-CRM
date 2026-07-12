//! Native App Connectors — actual API clients for each first-party app
//!
//! Each app gets a module here that knows how to talk to that app's API.
//! Connectors are loaded dynamically based on the `slug` field.

use std::collections::HashMap;

pub mod adaswift;
pub mod funnelswift;
pub mod cheatlayer;
pub mod workflowswift;
pub mod missedcall_responder;
pub mod multi_directory;

/// Registered connector metadata
pub struct AppConnector {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub auth_type: &'static str,
    pub auth_fields: &'static [&'static str],
    pub access_level: &'static str,  // "admin" | "admin_tenant"
}

/// All known native apps
pub static NATIVE_APPS: &[AppConnector] = &[
    AppConnector {
        slug: "adaswift",
        name: "AdaSwift Console",
        description: "Client viewing portal — clients see their reports, proposals, and account status. Admin-only connection since AdaSwift is a read-only portal for clients.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin",
    },
    AppConnector {
        slug: "cheatlayer",
        name: "CheatLayer",
        description: "RPA automation engine — browser automation, scraping, form filling. Admin-only connection.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin",
    },
    AppConnector {
        slug: "funnelswift",
        name: "FunnelSwift",
        description: "Sales funnel builder — mobile (Expo) app for building and managing sales funnels. Tenants connect their own FunnelSwift account.",
        auth_type: "api_key",
        auth_fields: &["api_key", "webhook_secret"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "workflowswift",
        name: "WorkflowSwift Automation",
        description: "n8n-based workflow automation engine with Supabase backend. Tenants connect their workflow instance.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "missedcall-responder",
        name: "MissedCall Responder",
        description: "Callback Pro SaaS — missed call handling with SMS auto-reply, hybrid LLM suite, lead kanban board.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "multi-directory",
        name: "Multi-Directory App",
        description: "Multi-account business directory system with automated follow-up sequences across directories. Admin-only — internal tool.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin",
    },
];

/// Test a connection to the given app with the provided credentials.
/// Returns (success, message, latency_ms).
pub async fn test_connection(
    slug: &str,
    credentials: &serde_json::Value,
) -> (bool, String, Option<i64>) {
    let start = std::time::Instant::now();

    let result = match slug {
        "adaswift" => adaswift::test(credentials).await,
        "cheatlayer" => cheatlayer::test(credentials).await,
        "funnelswift" => funnelswift::test(credentials).await,
        "workflowswift" => workflowswift::test(credentials).await,
        "missedcall-responder" => missedcall_responder::test(credentials).await,
        "multi-directory" => multi_directory::test(credentials).await,
        _ => (false, format!("Unknown app: {}", slug)),
    };

    let latency = Some(start.elapsed().as_millis() as i64);
    (result.0, result.1, latency)
}

/// Push data into a native app from CRM Swift.
pub async fn push_data(
    slug: &str,
    credentials: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match slug {
        "adaswift" => adaswift::push_entity(credentials, entity_type, data).await,
        "cheatlayer" => cheatlayer::push_entity(credentials, entity_type, data).await,
        "funnelswift" => funnelswift::push_entity(credentials, entity_type, data).await,
        "workflowswift" => workflowswift::push_entity(credentials, entity_type, data).await,
        "missedcall-responder" => missedcall_responder::push_entity(credentials, entity_type, data).await,
        "multi-directory" => multi_directory::push_entity(credentials, entity_type, data).await,
        _ => Err(format!("Unknown app: {}", slug)),
    }
}

/// Pull data from a native app into CRM Swift.
pub async fn pull_data(
    slug: &str,
    credentials: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    match slug {
        "adaswift" => adaswift::pull_entity(credentials, entity_type, filters).await,
        "cheatlayer" => cheatlayer::pull_entity(credentials, entity_type, filters).await,
        "funnelswift" => funnelswift::pull_entity(credentials, entity_type, filters).await,
        "workflowswift" => workflowswift::pull_entity(credentials, entity_type, filters).await,
        "missedcall-responder" => missedcall_responder::pull_entity(credentials, entity_type, filters).await,
        "multi-directory" => multi_directory::pull_entity(credentials, entity_type, filters).await,
        _ => Err(format!("Unknown app: {}", slug)),
    }
}

/// Get app metadata (labels, available entities, etc.)
pub fn get_app_meta(slug: &str) -> Option<serde_json::Value> {
    
    match slug {
        "adaswift" => Some(adaswift::get_meta()),
        "cheatlayer" => Some(cheatlayer::get_meta()),
        "funnelswift" => Some(funnelswift::get_meta()),
        "workflowswift" => Some(workflowswift::get_meta()),
        "missedcall-responder" => Some(missedcall_responder::get_meta()),
        "multi-directory" => Some(multi_directory::get_meta()),
        _ => None,
    }
}
