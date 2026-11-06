use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Ticket {
    pub id: uuid::Uuid,
    /// `None` for a message filed from the public contact page (2026-11-06): that visitor is not
    /// signed in and belongs to no workspace, so the row carries no tenant and the address to answer
    /// travels in `contact_email`. Every ticket filed from inside a workspace still names one.
    pub tenant_id: Option<uuid::Uuid>,
    pub subject: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub assigned_to: Option<uuid::Uuid>,
    pub contact_id: Option<uuid::Uuid>,
    pub source: String,
    pub contact_email: Option<String>,
    pub contact_name: Option<String>,
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
    pub source: Option<String>,
    pub contact_email: Option<String>,
    pub contact_name: Option<String>,
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

/// What the served contact page posts (`POST /api/public/support-request`, 2026-11-06).
///
/// No credential and no workspace: the visitor is not signed in, so the address to answer travels in
/// the body and lands in `tickets.contact_email`.
#[derive(Debug, Deserialize)]
pub struct PublicSupportRequest {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    pub subject: String,
    /// The visitor's words. The served page sends `body`; `message` is accepted as an alias so a
    /// hand-written client is not refused for naming the same field differently.
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContactFormRequest {
    pub tenant_id: uuid::Uuid,
    pub subject: String,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublicTicketRequest {
    pub subject: String,
    pub message: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub priority: Option<String>,
}
