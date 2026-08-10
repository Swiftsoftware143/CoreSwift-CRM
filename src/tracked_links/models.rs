use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TrackedLink {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tag_id: Uuid,
    pub slug: String,
    pub target_url: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackedLinkWithClicks {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub tag_id: Uuid,
    pub slug: String,
    pub target_url: String,
    pub created_at: DateTime<Utc>,
    pub click_count: i64,
    pub tag_name: Option<String>,
    pub tag_color: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTrackedLinkRequest {
    pub tag_id: Uuid,
    pub target_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateTrackedLinkResponse {
    pub id: Uuid,
    pub tag_id: Uuid,
    pub slug: String,
    pub target_url: String,
    pub tracking_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LinkClick {
    pub id: Uuid,
    pub tracked_link_id: Uuid,
    pub contact_id: Uuid,
    pub tenant_id: Uuid,
    pub clicked_at: DateTime<Utc>,
}
