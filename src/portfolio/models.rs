use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct PortfolioCompany {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: Option<String>,
    pub email: Option<String>,
    pub description: Option<String>,
    pub settings: Option<serde_json::Value>,
    pub is_active: Option<bool>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct IntegrationTarget {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub portfolio_company_id: Option<Uuid>,
    pub user_id: Option<Uuid>,
    pub name: String,
    pub provider: String,
    pub webhook_url: String,
    pub api_key: Option<String>,
    pub events: Option<Vec<String>>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePortfolioRequest {
    pub name: String,
    pub slug: Option<String>,
    pub email: Option<String>,
    pub description: Option<String>,
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePortfolioRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub email: Option<String>,
    pub description: Option<String>,
    pub settings: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTargetRequest {
    pub name: String,
    pub provider: Option<String>,
    pub webhook_url: String,
    pub api_key: Option<String>,
    pub events: Option<Vec<String>>,
}
