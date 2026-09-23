use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Company {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub domain: Option<String>,
    pub industry: Option<String>,
    pub size: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub website: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// How many contacts of this same tenant point at this company through
    /// `contacts.company_id` (a real FK since migration 087, `ON DELETE SET NULL`, and only
    /// ever written through the tenant-checked gate added on t_47698f73).
    ///
    /// This is NOT a column of `companies` and never was: the tenant shell's Companies tab
    /// renders `c.contact_count || c.contacts_count || '0'` and neither key was in this struct,
    /// so every row of every tenant showed the literal 0 (t_87036de2). Every query that decodes
    /// this struct computes the value with the identical correlated subquery instead of relying
    /// on a `#[sqlx(default)]`, so a future query that forgets it fails loudly rather than
    /// silently reporting zero.
    pub contact_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateCompanyRequest {
    pub name: String,
    pub domain: Option<String>,
    pub industry: Option<String>,
    pub size: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub website: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCompanyRequest {
    pub name: Option<String>,
    pub domain: Option<String>,
    pub industry: Option<String>,
    pub size: Option<String>,
    pub phone: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub website: Option<String>,
    pub notes: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub is_active: Option<bool>,
}
