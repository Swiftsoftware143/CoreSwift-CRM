//! Industry Dashboard data models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// User industry dashboard — database row from `user_industry_dashboards`.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct UserIndustryDashboard {
    pub id: Uuid,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub industry_slug: String,
    pub dashboard_name: String,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request body for setting/activating an industry dashboard.
#[derive(Debug, Deserialize)]
pub struct SetIndustryRequest {
    pub industry_slug: String,
    pub dashboard_name: Option<String>,
}

/// Available industry option (returned by GET /api/industries/available).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct IndustryOption {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub sort_order: Option<i32>,
}
