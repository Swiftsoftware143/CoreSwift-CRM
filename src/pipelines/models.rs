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
    pub color: Option<String>,
    pub position: i32,
    pub is_won: bool,
    pub is_lost: bool,
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
    pub moved_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineWithStages {
    pub pipeline: Pipeline,
    pub stages: Vec<PipelineStage>,
}

/// Money for ONE currency. Every money figure in this module is delivered in this shape, because
/// adding two currencies together produces a number that is not money at all (David, 2026-11-20:
/// multi-currency is core). A workspace selling in USD and EUR gets two of these, never one total.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct CurrencyAmount {
    pub currency: String,
    pub deal_count: i64,
    pub total_value: f64,
    pub weighted_value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageAnalytics {
    pub stage_id: Uuid,
    pub stage_name: String,
    pub count: i64,
    pub avg_time_in_days: f64,
    pub conversion_rate: f64,
    /// One entry per currency held in this stage.
    pub by_currency: Vec<CurrencyAmount>,
    /// This stage in the workspace's reporting currency, computed from each deal's stored rate.
    pub reporting: ReportingMoney,
}

/// One money amount converted into the workspace's reporting currency. `unconverted_count` counts
/// deals whose stored rate does not reach that currency — they are named, never silently added at
/// face value.
#[derive(Debug, Clone, Serialize)]
pub struct ReportingMoney {
    pub currency: String,
    pub total_value: f64,
    pub weighted_value: f64,
    pub unconverted_count: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineAnalytics {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub total_opportunities: i64,
    pub won_count: i64,
    pub lost_count: i64,
    /// won / (won + lost), 0.0 when nothing has closed.
    pub win_rate: f64,
    /// One entry per currency held in this pipeline — the ONLY place the pipeline's money lives.
    pub by_currency: Vec<CurrencyAmount>,
    /// The whole pipeline in the reporting currency (David's condition: totals never mix raw).
    pub reporting: ReportingMoney,
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
    pub color: Option<String>,
    pub position: Option<i32>,
    pub probability: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStageRequest {
    pub name: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub is_won: Option<bool>,
    pub is_lost: Option<bool>,
    pub probability: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct MoveOpportunityRequest {
    pub reason: Option<String>,
}
