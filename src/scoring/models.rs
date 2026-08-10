use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ScoreRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub event_type: String,
    pub points: i32,
    pub direction: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Score {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
    pub total_score: i32,
    pub category: String,
    pub last_event_type: Option<String>,
    pub last_event_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ScoreHistory {
    pub id: Uuid,
    pub score_id: Uuid,
    pub contact_id: Uuid,
    pub rule_id: Option<Uuid>,
    pub tenant_id: Uuid,
    pub points: i32,
    pub previous_score: i32,
    pub new_score: i32,
    pub event_type: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ScoreEventRequest {
    pub event_type: String,
    pub description: Option<String>,
}

pub const INTERESTED_MAX: i32 = 39;
pub const QUALIFIED_MAX: i32 = 69;
pub const HOT_MAX: i32 = 89;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ScoringWebhook {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub url: String,
    pub min_score: i32,
    pub max_score: Option<i32>,
    pub event_type: Option<String>,
    pub headers: serde_json::Value,
    pub is_active: bool,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub failure_count: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWebhookRequest {
    pub name: String,
    pub url: String,
    pub min_score: i32,
    pub max_score: Option<i32>,
    pub event_type: Option<String>,
    pub headers: Option<serde_json::Value>,
}

pub fn score_category(score: i32) -> &'static str {
    if score <= INTERESTED_MAX {
        "interested"
    } else if score <= QUALIFIED_MAX {
        "qualified"
    } else if score <= HOT_MAX {
        "hot"
    } else {
        "sold"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ScoringThreshold {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub pipeline_id: Uuid,
    pub min_score: i32,
    pub max_score: Option<i32>,
    pub target_stage_id: Uuid,
    pub action: String,
    pub action_config: serde_json::Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateThresholdRequest {
    pub pipeline_id: Uuid,
    pub min_score: i32,
    pub max_score: Option<i32>,
    pub target_stage_id: Uuid,
    pub action: Option<String>,
    pub action_config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub event_type: String,
    pub points: i32,
    pub direction: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub event_type: Option<String>,
    pub points: Option<i32>,
    pub direction: Option<String>,
    pub is_active: Option<bool>,
}
