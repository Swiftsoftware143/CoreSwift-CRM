//! Webhook models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AutomationWebhook {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub webhook_token: String,
    pub allowed_actions: Vec<String>,
    pub rate_limit_per_minute: i32,
    pub last_used_at: Option<DateTime<Utc>>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AutomationWebhookLog {
    pub id: Uuid,
    pub webhook_id: Uuid,
    pub action: String,
    pub request_body: Option<serde_json::Value>,
    pub response_status: Option<i32>,
    pub response_body: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct WebhookRequestBody {
    pub params: Option<serde_json::Value>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct WebhookResponse {
    pub success: bool,
    pub action: String,
    pub data: Option<serde_json::Value>,
    pub error: Option<String>,
    pub elapsed_ms: i64,
}
