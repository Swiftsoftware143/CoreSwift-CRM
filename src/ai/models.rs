//! AI models — input/output structs for all AI-powered endpoints.
//! These feed the event-driven orchestration engine with intelligent decisions.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ── Lead Prioritization ──

#[derive(Debug, Deserialize)]
pub struct PrioritizeRequest {
    pub tenant_id: Uuid,
    pub limit: Option<i32>,
}

pub struct PrioritizedContact {
    pub contact_id: Uuid,
    pub name: String,
    pub email: String,
    pub score: i32,
    pub health_score: i32,
    pub risk_level: String,
    pub last_activity: Option<String>,
    pub days_inactive: i32,
    pub priority_score: f64, // composite: (score * 0.4) + (100-health) * 0.3 + days_inactive * 0.2 + engagement * 0.1
    pub recommended_action: String, // "re-engage_email", "checklist_stage", "human_callback", "downgrade"
}

// ── Win Prediction ──

#[derive(Debug, Deserialize)]
pub struct PredictRequest {
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
}

pub struct WinPrediction {
    pub contact_id: Uuid,
    pub business_name: String,
    pub win_probability: f64,
    pub expected_value: f64,
    pub key_signals: Vec<String>, // positive signals: "email.opened", "feature_used", "login"
    pub warning_signals: Vec<String>, // negative: "days_inactive", "support_ticket", "failed_action"
    pub recommendation: String,
}

// ── Message Composition (AI-written follow-up copy) ──

#[derive(Debug, Deserialize)]
pub struct ComposeMessageRequest {
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
    pub context: String, // "abandoned_signup", "inactive_trial", "checklist_stage_2", "churn_risk", "renewal"
    pub channel: String, // "email" or "sms"
    pub tone: Option<String>, // "professional", "friendly", "urgent"
}

#[derive(Debug, Serialize)]
pub struct ComposedMessage {
    pub subject: Option<String>,
    pub body: String,
    pub message_id: Uuid,
}

// ── Channel Suggestion ──

#[derive(Debug, Deserialize)]
pub struct ChannelRequest {
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
    pub context: String,
}

#[derive(Debug, Serialize)]
pub struct ChannelSuggestion {
    pub recommended_channel: String,
    pub confidence: f64,
    pub reason: String,
}

// ── Timing Optimization ──

#[derive(Debug, Deserialize)]
pub struct TimingRequest {
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct TimingSuggestion {
    pub recommended_hour: u8,
    pub recommended_day: String,
    pub best_window: String,
    pub confidence: f64,
    pub reason: String,
}

// ── Churn Risk Assessment ──

#[derive(Debug, Deserialize)]
pub struct ChurnRequest {
    pub tenant_id: Uuid,
    pub contact_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct ChurnAssessment {
    pub contact_id: Uuid,
    pub churn_probability: f64,
    pub risk_tier: String,
    pub age_days: i64,
    pub inactivity_days: i64,
    pub signals_count: i32,
    pub intervention: String,
    pub priority: String,
}

// ── Campaign Recommendation ──

#[derive(Debug, Deserialize)]
pub struct CampaignRequest {
    pub tenant_id: Uuid,
    pub campaign_goal: String, // "trial_conversion", "reactivation", "upsell", "retention"
}

#[derive(Debug, Serialize)]
pub struct CampaignRecommendation {
    pub target_count: i64,
    pub recommended_template: String,
    pub ai_message: String,
    pub segments: Vec<CampaignSegment>,
}

#[derive(Debug, Serialize)]
pub struct CampaignSegment {
    pub name: String,
    pub count: i64,
    pub avg_health_score: f64,
    pub recommended_offer: String,
}
