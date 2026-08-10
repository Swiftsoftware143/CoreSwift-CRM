use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Integration {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub provider: String,
    pub config: serde_json::Value,
    pub is_active: bool,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TagMapping {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub integration_id: Uuid,
    pub local_tag_id: Uuid,
    pub external_system: String,
    pub external_id: String,
    pub direction: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Webhook {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub url: String,
    pub secret: Option<String>,
    pub events: Option<Vec<String>>,
    pub retry_count: i32,
    pub timeout_seconds: i32,
    pub is_active: bool,
    pub last_triggered_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateIntegrationRequest {
    pub name: String,
    pub provider: String,
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateIntegrationRequest {
    pub name: Option<String>,
    pub config: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMappingRequest {
    pub local_tag_id: Uuid,
    pub external_system: String,
    pub external_id: String,
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    pub secret: Option<String>,
    pub events: Option<Vec<String>>,
    pub retry_count: Option<i32>,
    pub timeout_seconds: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWebhookRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub secret: Option<String>,
    pub events: Option<Vec<String>>,
    pub retry_count: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub is_active: Option<bool>,
}
