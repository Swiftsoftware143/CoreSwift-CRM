//! Checklist models.
//!
//! Templates define onboarding stages; instances track progress per entity.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Checklist template — defines a staged onboarding flow.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChecklistTemplate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub stage_count: i32,
    pub days_per_stage: i32,
    pub is_active: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Checklist stage — one step in a checklist template.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChecklistStage {
    pub id: Uuid,
    pub template_id: Uuid,
    pub stage_order: i32,
    pub title: String,
    pub description: Option<String>,
    pub action_required: Option<String>,
    pub channel: Option<String>,
    pub message_template: Option<String>,
    pub delay_hours: Option<i32>,
    pub created_at: DateTime<Utc>,
}

/// Checklist instance — one entity's progress through a template.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChecklistInstance {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub template_id: Uuid,
    pub entity_type: String,
    pub entity_id: Uuid,
    pub current_stage: Option<i32>,
    pub completed: Option<bool>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Checklist progress — per-stage completion tracking.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ChecklistProgress {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub stage_order: i32,
    pub completed: Option<bool>,
    pub action_taken: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// A stage definition embedded in template creation.
#[derive(Debug, Deserialize)]
pub struct CreateStageRequest {
    pub stage_order: i32,
    pub title: String,
    pub description: Option<String>,
    pub action_required: Option<String>,
    pub channel: Option<String>,
    pub message_template: Option<String>,
    pub delay_hours: Option<i32>,
}

/// Request to create a new checklist template.
#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub stage_count: Option<i32>,
    pub days_per_stage: Option<i32>,
    pub stages: Option<Vec<CreateStageRequest>>,
}

/// Request to update a checklist template.
#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub trigger_type: Option<String>,
    pub stage_count: Option<i32>,
    pub days_per_stage: Option<i32>,
    pub is_active: Option<bool>,
}

/// Request to update checklist progress for a stage.
#[derive(Debug, Deserialize)]
pub struct UpdateProgressRequest {
    pub action_taken: Option<String>,
    pub completed: Option<bool>,
}
