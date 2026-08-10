use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Pipeline {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PipelineStage {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub position: i32,
    pub is_won_stage: bool,
    pub is_lost_stage: bool,
    pub probability: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct StageHistory {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub from_stage_id: Option<Uuid>,
    pub to_stage_id: Uuid,
    pub moved_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWithStages {
    pub pipeline: Pipeline,
    pub stages: Vec<PipelineStage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageAnalytics {
    pub stage_id: Uuid,
    pub stage_name: String,
    pub count: i64,
    pub total_value: f64,
    pub avg_time_in_days: f64,
    pub conversion_rate: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineAnalytics {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub total_opportunities: i64,
    pub total_value: f64,
    pub won_count: i64,
    pub won_value: f64,
    pub lost_count: i64,
    pub lost_value: f64,
    pub stages: Vec<StageAnalytics>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePipelineRequest {
    pub name: String,
    pub description: Option<String>,
    pub is_default: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePipelineRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub is_default: Option<bool>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStageRequest {
    pub name: String,
    pub description: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub probability: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStageRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub is_won_stage: Option<bool>,
    pub is_lost_stage: Option<bool>,
    pub probability: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct MoveOpportunityRequest {
    pub reason: Option<String>,
}
