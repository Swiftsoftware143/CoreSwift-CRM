use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct EmailCampaign {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub status: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CampaignStep {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub step_order: i32,
    pub template_name: String,
    pub subject: Option<String>,
    pub body: String,
    pub delay_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CampaignTrigger {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub tag_id: Uuid,
    pub trigger_type: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CampaignEnrollment {
    pub id: Uuid,
    pub campaign_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub current_step: i32,
    pub total_steps: i32,
    pub status: String,
    pub next_send_at: Option<DateTime<Utc>>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

// Request types

#[derive(Debug, Deserialize)]
pub struct CreateCampaignRequest {
    pub name: String,
    pub description: Option<String>,
    /// Optional tag name to auto-create trigger and sync with FunnelSwift
    pub funnelswift_tag: Option<String>,
    pub funnelswift_sync: Option<bool>,
    /// Steps to build (template_name, subject, body, delay_days)
    pub steps: Option<Vec<StepDefinition>>,
}

#[derive(Debug, Deserialize)]
pub struct StepDefinition {
    pub template_name: String,
    pub subject: Option<String>,
    pub body: String,
    pub delay_days: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCampaignRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddStepRequest {
    pub step_order: Option<i32>,
    pub template_name: String,
    pub subject: Option<String>,
    pub body: String,
    pub delay_days: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStepRequest {
    pub template_name: Option<String>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub delay_days: Option<i32>,
    pub step_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct AddTriggerRequest {
    pub tag_id: Uuid,
    pub trigger_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BuildCampaignRequest {
    pub name: String,
    pub description: Option<String>,
    pub funnelswift_tag: Option<String>,
    pub funnelswift_sync: Option<bool>,
    pub steps: Vec<StepDefinition>,
}

#[derive(Debug, Deserialize)]
pub struct EnrollContactRequest {
    pub entity_type: Option<String>,
    pub entity_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEnrollmentRequest {
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SyncTagRequest {
    pub tag_name: String,
    pub action: String,
    pub source: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct BuildResult {
    pub campaign: EmailCampaign,
    pub steps: Vec<CampaignStep>,
    pub tag_id: Option<Uuid>,
    pub funnelswift_synced: bool,
    pub message: String,
}
