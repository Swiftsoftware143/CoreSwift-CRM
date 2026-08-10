use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AccountHealth {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub score: i32,
    pub last_active_at: Option<DateTime<Utc>>,
    pub risk_level: String,
    pub signals: serde_json::Value,
    pub last_intervention_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct HealthThreshold {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub entity_type: String,
    pub metric: String,
    pub operator: String,
    pub value: i32,
    pub risk_level: String,
    pub intervention_action: String,
    pub intervention_config: serde_json::Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct SignalRequest {
    pub entity_type: String,
    pub entity_id: Uuid,
    pub signal: String,
    pub value: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct CreateThresholdRequest {
    pub name: String,
    pub entity_type: String,
    pub metric: String,
    pub operator: String,
    pub value: i32,
    pub risk_level: String,
    pub intervention_action: Option<String>,
    pub intervention_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateThresholdRequest {
    pub name: Option<String>,
    pub value: Option<i32>,
    pub risk_level: Option<String>,
    pub intervention_action: Option<String>,
    pub is_active: Option<bool>,
}
