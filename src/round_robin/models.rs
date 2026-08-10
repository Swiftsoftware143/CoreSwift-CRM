use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RoundRobinTeam {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub strategy: String,
    pub scope_type: String,
    pub scope_id: Option<Uuid>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RoundRobinMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: Uuid,
    pub weight: i32,
    pub max_concurrent_bookings: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct RoundRobinAssignment {
    pub id: Uuid,
    pub team_id: Uuid,
    pub member_id: Uuid,
    pub booking_id: Option<Uuid>,
    pub contact_id: Option<Uuid>,
    pub assigned_at: DateTime<Utc>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    pub description: Option<String>,
    pub strategy: Option<String>,
    pub scope_type: Option<String>,
    pub scope_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    pub user_id: Uuid,
    pub weight: Option<i32>,
    pub max_concurrent_bookings: Option<i32>,
}
