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
    /// Free-text employer name — the column the product actually fills (CSV import, inbound
    /// capture, cross-app tag sync) and the one the Contacts tab renders. It was missing from
    /// this struct, so the `SELECT *` on every contact query silently dropped the column
    /// before serialization and the Company cell showed an em dash for every row of every
    /// tenant, including rows whose `contacts.company` is set.
    pub company: Option<String>,
    /// Machine-readable link to a `companies` row of the SAME tenant — a real reference since
    /// migration 087 (`contacts_company_id_fkey`, ON DELETE SET NULL) and refused at write time
    /// unless the company is the caller's (t_47698f73). Deliberately NOT a display source: the
    /// free-text `company` above is the employer the product renders (decision on t_fbb30c16).
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
    /// `contacts.is_active` is NULLABLE in the live schema (`\d contacts`: `boolean` default true,
    /// no NOT NULL) while this field used to be a plain `bool` (t_97b46a98). sqlx then failed the
    /// decode of the whole row — "unexpected null" — and every route that reads a contact by
    /// `SELECT *` answered 500 "Database error" for a row whose `is_active` is NULL: measured on the
    /// deployed binary for `GET /api/contacts/{id}`, for the `POST /api/contacts` dedup read and for
    /// the `PATCH` `RETURNING *` (2026-09-23). `list`/`search` only looked immune because both filter
    /// `WHERE is_active = true`. Decoded as what the column is: NULL is data, not an error.
    pub is_active: Option<bool>,
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
    /// Optional link to a company of the caller's tenant. A uuid that does not name a company of
    /// this tenant is answered with 404 and is NOT stored (t_47698f73) — before that gate any
    /// uuid was accepted and kept forever as a reference to nothing.
    pub company_id: Option<Uuid>,
    /// Free-text employer — the column the product renders and every non-CRUD writer fills.
    /// It is the source of truth for a contact's company (decision on t_fbb30c16); `company_id`
    /// is the machine-readable link, validated against `companies` and never rendered.
    pub company: Option<String>,
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
    /// Same gate as `CreateContactRequest::company_id`: NULL keeps the stored link, a uuid that
    /// names no company of this tenant is a 404 (t_47698f73).
    pub company_id: Option<Uuid>,
    /// The employer a contact shows: this text column wins over `company_id` for display
    /// (decision on t_fbb30c16). `None` means "not mentioned, keep the stored value"; a blank
    /// string means "clear it" and is normalised to NULL on the way in.
    pub company: Option<String>,
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
