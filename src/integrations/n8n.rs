use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct N8nConfig {
    pub webhook_url: String,
    pub api_key: Option<String>,
}

pub async fn trigger_workflow(
    _config: &N8nConfig,
    event: &str,
    _payload: &serde_json::Value,
) -> Result<(), String> {
    tracing::info!("n8n workflow trigger: event={}", event);
    Ok(())
}

pub fn build_payload(
    event: &str,
    entity_type: &str,
    entity_id: &str,
    data: Option<&serde_json::Value>,
) -> serde_json::Value {
    json!({"event": event, "entity_type": entity_type, "entity_id": entity_id, "data": data, "timestamp": chrono::Utc::now().to_rfc3339()})
}
