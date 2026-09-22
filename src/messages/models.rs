//! Message models for the CoreSwift Unified Inbox.
//!
//! CS-5: these fields mirror the columns `cs_messages` actually has. Every column except
//! `id` and `tenant_id` is nullable, so the decode uses `Option` and a NULL never turns a
//! row into a 500.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Message stored in cs_messages table.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Message {
    pub id: Uuid,
    pub tenant_id: Uuid,
    /// The tenant user who sent it (NULL for an inbound webhook message).
    pub sender_id: Option<Uuid>,
    /// FK -> contacts: the contact on the other side of the thread.
    pub recipient_id: Option<Uuid>,
    pub subject: Option<String>,
    pub body: Option<String>,
    pub channel: Option<String>,
    pub status: Option<String>,
    /// Serialised as `is_read` so existing consumers keep their field name.
    #[serde(rename = "is_read")]
    pub read: Option<bool>,
    pub direction: Option<String>,
    pub thread_id: Option<Uuid>,
    pub parent_message_id: Option<Uuid>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub is_archived: Option<bool>,
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
