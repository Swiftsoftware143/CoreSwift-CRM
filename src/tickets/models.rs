use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Ticket {
    pub id: uuid::Uuid,
    pub tenant_id: uuid::Uuid,
    pub subject: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: Option<uuid::Uuid>,
    pub contact_id: Option<uuid::Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTicketRequest {
    pub subject: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub contact_id: Option<uuid::Uuid>,
    pub assigned_to: Option<uuid::Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTicketRequest {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assigned_to: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TicketMessage {
    pub id: uuid::Uuid,
    pub ticket_id: uuid::Uuid,
    pub sender_type: String,
    pub message: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AddMessageRequest {
    pub message: String,
    pub sender_type: Option<String>, // "agent" or "contact", defaults to "agent"
}

#[derive(Debug, Deserialize)]
pub struct TicketListQuery {
    pub status: Option<String>,
    pub priority: Option<String>,
    pub contact_id: Option<uuid::Uuid>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}
