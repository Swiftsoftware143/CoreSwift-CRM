//! Message models for the CoreSwift Unified Inbox.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Message stored in cs_messages table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub sender_name: String,
    pub sender_email: Option<String>,
    pub sender_phone: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub source: String,
    pub source_id: Option<String>,
    pub is_read: bool,
    pub is_archived: bool,
    pub is_replied: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Payload for creating a new message manually.
#[derive(Debug, Deserialize)]
pub struct CreateMessageRequest {
    pub contact_id: Option<Uuid>,
    pub sender_name: String,
    pub sender_email: Option<String>,
    pub sender_phone: Option<String>,
    pub subject: Option<String>,
    pub body: String,
}

/// Payload received from MultiDirectory webhook.
#[derive(Debug, Deserialize)]
pub struct WebhookMessagePayload {
    pub sender_name: String,
    pub sender_email: Option<String>,
    pub sender_phone: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub source: Option<String>,
    pub source_id: Option<String>,
}

/// Query parameters for listing messages.
#[derive(Debug, Deserialize)]
pub struct MessageListParams {
    pub page: Option<i64>,
    pub limit: Option<i64>,
    #[serde(rename = "status")]
    pub status: Option<String>,
    pub search: Option<String>,
}

/// Payload for updating a message.
#[derive(Debug, Deserialize)]
pub struct UpdateMessageRequest {
    pub is_read: Option<bool>,
    pub is_archived: Option<bool>,
}
