use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
    pub settings: Option<serde_json::Value>,
    /// `tenants.is_active` is NULLABLE in the live schema (boolean, default true, 0 NULLs live) and
    /// this struct is decoded from `SELECT *` / `RETURNING *` on `tenants`, so a plain `bool` here
    /// made a whole-row decode fail (500) the moment a row carried a NULL — the same class as
    /// `Contact.is_active` on t_97b46a98. A NULL is data (unknown), not `false` / `true`.
    /// Every INSERT INTO tenants in the fleet omits this column (so the default applies) and the
    /// only bind is `is_active = COALESCE($n, is_active)`, so no writer can store a NULL today.
    pub is_active: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAccountRequest {
    pub name: String,
    pub slug: String,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAccountRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
    pub is_active: Option<bool>,
}
