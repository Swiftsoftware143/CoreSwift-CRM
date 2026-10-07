use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Normalized incoming event from any external source
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Event {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// Human title. `events` began as the calendar table, so `title` is NOT NULL; the webhook
    /// ingest path (src/events/handlers.rs::ingest) derives it from the payload when absent.
    pub title: String,
    pub description: Option<String>,
    pub event_date: Option<NaiveDate>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub location: Option<String>,
    pub source: String,              // "landing_page", "directory", "saas", "api"
    pub event_type: String, // "form.submitted", "trial.started", "listing.viewed", "payment.received"
    pub status: Option<String>, // "scheduled" | … (calendar rows; NULL on ingested webhook events)
    pub entity_type: Option<String>, // "contact", "company", "opportunity"
    pub entity_id: Option<Uuid>,
    pub payload: serde_json::Value,
    pub raw_headers: Option<serde_json::Value>,
    pub processed: bool,
    pub processed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Body for POST /api/events — create a calendar event by hand.
#[derive(Debug, Deserialize)]
pub struct CreateEventRequest {
    pub title: String,
    pub description: Option<String>,
    pub event_date: Option<String>, // "YYYY-MM-DD"
    pub start_time: Option<String>, // "HH:MM"
    pub end_time: Option<String>,
    pub location: Option<String>,
    pub event_type: Option<String>,
}

/// Delayed action — the "If-Not-Then" engine
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DelayedAction {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub trigger_event_id: Option<Uuid>,
    pub condition_type: Option<String>, // NULL = the auto-reply writer's row (073: nullable on purpose)
    pub condition_config: serde_json::Value, // {"wait_hours": 2, "expected_event": "form.completed"}
    pub action_type: String,                 // "send_email", "send_sms", "webhook", "tag_contact"
    pub action_config: serde_json::Value,
    pub execute_at: DateTime<Utc>,
    pub executed: bool,
    pub cancelled: bool,
    pub result: Option<serde_json::Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Incoming webhook body (raw from external service)
#[derive(Debug, Deserialize)]
pub struct IngestPayload {
    pub event_type: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct ScheduleDelayedRequest {
    pub trigger_event_id: Option<Uuid>,
    pub condition_type: String,
    pub condition_config: serde_json::Value,
    pub action_type: String,
    pub action_config: serde_json::Value,
    pub execute_at: String, // ISO-8601 timestamp
}
