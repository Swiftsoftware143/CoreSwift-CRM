//! Plans data models — Plan struct, create/update request types.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Plan model — database row from `plans` table.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub price_monthly: Decimal,
    pub price_yearly: Decimal,
    pub max_contacts: i32,
    pub max_deals: i32,
    pub max_users: i32,
    pub max_storage_mb: i32,
    pub features: serde_json::Value,
    pub payment_link: Option<String>,
    pub payment_provider: Option<String>,
    pub is_active: bool,
    pub sort_order: i32,
    pub max_industries: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for creating a new plan (POST /api/plans).
#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub name: String,
    pub description: Option<String>,
    pub price_monthly: Option<f64>,
    pub price_yearly: Option<f64>,
    pub max_contacts: Option<i32>,
    pub max_deals: Option<i32>,
    pub max_users: Option<i32>,
    pub max_storage_mb: Option<i32>,
    pub features: Option<serde_json::Value>,
    pub payment_link: Option<String>,
    pub payment_provider: Option<String>,
    pub sort_order: Option<i32>,
    pub max_industries: Option<i32>,
}

/// Request body for updating a plan (PATCH /api/plans/:id).
#[derive(Debug, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub price_monthly: Option<f64>,
    pub price_yearly: Option<f64>,
    pub max_contacts: Option<i32>,
    pub max_deals: Option<i32>,
    pub max_users: Option<i32>,
    pub max_storage_mb: Option<i32>,
    pub features: Option<serde_json::Value>,
    pub payment_link: Option<String>,
    pub payment_provider: Option<String>,
    pub is_active: Option<bool>,
    pub sort_order: Option<i32>,
    pub max_industries: Option<i32>,
}

/// Tenant with plan info for listing
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantWithPlan {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
    pub settings: Option<serde_json::Value>,
    pub is_active: bool,
    pub plan_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
