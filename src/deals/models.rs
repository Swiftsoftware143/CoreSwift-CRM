//! Deal Writer — models.
//!
//! The DEAL is `opportunities` (the existing system of record: pipeline, stage, value, history).
//! This module adds what the writer and the deal tab need on top of it — owner, external
//! reference, collaborators, custom fields, timeline, tasks, files — WITHOUT forking a second
//! deal table. See `migrations/186_deal_writer.sql`.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

// ── reads ───────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealCollaborator {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub email: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealFieldDefinition {
    pub id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    #[sqlx(json)]
    pub options: Value,
    pub position: i32,
    pub is_active: bool,
    pub source_app: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealFieldValue {
    pub field_id: Uuid,
    pub key: String,
    pub label: String,
    pub field_type: String,
    pub value: Option<String>,
    /// A `money` field carries the amount and its OWN currency code beside the display text, plus
    /// the same stored rate/date the deal itself keeps (David, 2026-11-20).
    pub money_amount: Option<f64>,
    pub money_currency: Option<String>,
    pub money_exchange_rate: Option<f64>,
    pub money_reporting_currency: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealActivity {
    pub id: Uuid,
    pub kind: String,
    pub body: Option<String>,
    pub actor_id: Option<Uuid>,
    pub actor_label: Option<String>,
    #[sqlx(json)]
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealTask {
    pub id: Uuid,
    pub title: String,
    pub notes: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub assigned_to: Option<Uuid>,
    pub assignee_name: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct DealAttachment {
    pub id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i32,
    pub uploaded_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct LostReason {
    pub id: Uuid,
    pub label: String,
    pub position: i32,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StageAutomation {
    pub id: Uuid,
    pub pipeline_id: Uuid,
    pub stage_id: Uuid,
    pub stage_name: String,
    pub action_kind: String,
    pub tag_id: Option<Uuid>,
    pub tag_name: Option<String>,
    pub is_active: bool,
}

/// The whole deal screen in one payload: the row, the per-workspace fields, the people, the
/// timeline, the follow-ups and the files. One request, so a detail screen cannot show a
/// half-refreshed deal.
#[derive(Debug, Serialize)]
pub struct DealDetail {
    pub deal: crate::pipelines::opportunity::OpportunityFull,
    pub stage: Option<crate::pipelines::models::PipelineStage>,
    pub pipeline_name: Option<String>,
    pub owner: Option<PersonRef>,
    pub contact: Option<PersonRef>,
    pub company_name: Option<String>,
    pub collaborators: Vec<DealCollaborator>,
    pub custom_fields: Vec<DealFieldValue>,
    pub activities: Vec<DealActivity>,
    pub tasks: Vec<DealTask>,
    pub attachments: Vec<DealAttachment>,
    pub stage_history: Vec<StageMove>,
    pub weighted_value: f64,
    pub days_in_stage: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct PersonRef {
    pub id: Uuid,
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StageMove {
    pub from_stage_id: Option<Uuid>,
    pub to_stage_id: Uuid,
    pub moved_at: DateTime<Utc>,
    pub moved_by_name: Option<String>,
}

// ── writes ──────────────────────────────────────────────────────────────────────────────────

/// Everything the ONE deal writer accepts — from the console AND from an outside system.
///
/// A field left `None` is "not sent" and is left alone on an update (the writer only writes what
/// the caller said). `external_ref` is what makes an API push idempotent: the writer finds the
/// deal by `(tenant, external_ref)` and updates it instead of inserting a second row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DealUpsert {
    pub external_ref: Option<String>,
    /// `title` is accepted as a synonym: outside systems call it a title, the CRM calls it a name.
    #[serde(alias = "title")]
    pub name: Option<String>,
    pub pipeline_id: Option<Uuid>,
    pub pipeline: Option<String>,
    pub stage_id: Option<Uuid>,
    pub stage: Option<String>,
    pub contact_id: Option<Uuid>,
    pub contact_email: Option<String>,
    pub contact_name: Option<String>,
    pub contact_phone: Option<String>,
    pub company_id: Option<Uuid>,
    pub company_name: Option<String>,
    pub value: Option<f64>,
    pub currency: Option<String>,
    pub probability: Option<i32>,
    pub expected_close_date: Option<NaiveDate>,
    pub owner_id: Option<Uuid>,
    pub owner_email: Option<String>,
    pub source: Option<String>,
    pub notes: Option<String>,
    /// open | won | lost. `won`/`lost` also moves the deal to the pipeline's won/lost stage when
    /// the pipeline has one, so the board and the flags can never disagree.
    pub status: Option<String>,
    pub lost_reason: Option<String>,
    /// Rate used to record this deal's value, captured WITH the deal so a past period never moves.
    /// Optional on the wire; see `writer::resolve_currency` for what happens when it is absent.
    pub exchange_rate: Option<f64>,
    pub collaborator_ids: Option<Vec<Uuid>>,
    pub custom_fields: Option<Map<String, Value>>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct DealListParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub q: Option<String>,
    pub pipeline_id: Option<Uuid>,
    pub stage_id: Option<Uuid>,
    pub owner_id: Option<Uuid>,
    pub source: Option<String>,
    pub status: Option<String>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// `true` = only deals with no activity for `rot_days`.
    pub rotting: Option<bool>,
    pub rot_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MoveDealRequest {
    pub stage_id: Option<Uuid>,
    pub stage: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DealStatusRequest {
    pub status: String,
    pub lost_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewActivityRequest {
    pub body: String,
    pub kind: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewTaskRequest {
    pub title: String,
    pub notes: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub assigned_to: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTaskRequest {
    pub title: Option<String>,
    pub notes: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub assigned_to: Option<Uuid>,
    pub completed: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct NewAttachmentRequest {
    pub filename: String,
    pub content_type: Option<String>,
    /// Base64 of the file bytes. Cap enforced by the handler (5 MiB decoded).
    pub content_base64: String,
}

#[derive(Debug, Deserialize)]
pub struct NewFieldRequest {
    pub key: Option<String>,
    pub label: String,
    pub field_type: Option<String>,
    pub options: Option<Value>,
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateFieldRequest {
    pub label: Option<String>,
    pub field_type: Option<String>,
    pub options: Option<Value>,
    pub position: Option<i32>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct NewLostReasonRequest {
    pub label: String,
    pub position: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct NewStageAutomationRequest {
    pub stage_id: Uuid,
    pub action_kind: String,
    pub tag_id: Option<Uuid>,
    pub webhook_url: Option<String>,
}

/// Result of an upsert — the caller is told which of create/update happened, so a repeated push
/// is visibly an update rather than a silent second deal.
#[derive(Debug, Serialize)]
pub struct UpsertOutcome {
    pub deal: crate::pipelines::opportunity::OpportunityFull,
    pub created: bool,
    pub matched_by: &'static str,
    pub stage_moved: bool,
    pub pipeline_created: bool,
}

// ── reporting ───────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct StageTotal {
    pub stage_id: Uuid,
    pub stage_name: String,
    pub position: i32,
    pub probability: i32,
    pub is_won: bool,
    pub is_lost: bool,
    /// Money is NEVER added across currencies: this row is one stage IN ONE CURRENCY.
    pub currency: String,
    pub deal_count: i64,
    pub total_value: f64,
    pub weighted_value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DealReport {
    pub pipeline_id: Uuid,
    pub pipeline_name: String,
    pub currency: String,
    pub open_count: i64,
    pub open_value: f64,
    pub open_weighted_value: f64,
    pub won_count: i64,
    pub won_value: f64,
    pub lost_count: i64,
    pub lost_value: f64,
    /// won / (won + lost), 0.0 when nothing has closed yet.
    pub win_rate: f64,
    /// Mean (created_at -> first won stamp) over closed-won deals, in days.
    pub avg_days_to_close: f64,
    pub rotting_count: i64,
    pub rot_days: i64,
    pub stages: Vec<StageTotal>,
    /// The SAME pipeline in the workspace's reporting currency, computed from each deal's STORED
    /// rate — the number David's amendment asks for (condition 6). Repeated on every currency row
    /// of this pipeline because it is a property of the pipeline, not of one currency line.
    pub reporting: ReportingTotals,
}

/// Money converted into the workspace's reporting currency, from the rate each deal stored when it
/// was written. `unconverted_count` is the honest escape hatch: deals whose stored rate does not
/// point at this reporting currency are counted, never silently folded in at face value.
#[derive(Debug, Clone, Serialize)]
pub struct ReportingTotals {
    pub currency: String,
    pub open_count: i64,
    pub open_value: f64,
    pub open_weighted_value: f64,
    pub won_count: i64,
    pub won_value: f64,
    pub lost_count: i64,
    pub lost_value: f64,
    pub win_rate: f64,
    pub unconverted_count: i64,
}
