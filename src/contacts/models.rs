//! Contact models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Contact model — full profile with JSONB metadata.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Contact {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
    pub company_id: Option<Uuid>,
    pub gender: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateContactRequest {
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: String,
    pub last_name: String,
    pub title: Option<String>,
    pub company_id: Option<Uuid>,
    pub gender: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateContactRequest {
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub title: Option<String>,
    pub company_id: Option<Uuid>,
    pub gender: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}
