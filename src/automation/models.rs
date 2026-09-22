use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The real column on `automation_rules` is **`is_active`** (nullable, default `true`) — there is
/// no `is_enabled` column; see `src/events/dispatcher.rs`, which already filtered on `is_active`.
/// `is_active` is nullable, so it is projected as `COALESCE(is_active, true)`: NULL means enabled
/// (that is what the column default encodes), and the API therefore always hands back a real
/// boolean instead of failing to decode a NULL. The engine's filters use `is_active IS NOT FALSE`
/// for the same reason.
pub const RULE_COLUMNS: &str = "id, tenant_id, name, description, trigger_type, trigger_config, \
                                action_type, action_config, COALESCE(is_active, true) AS is_active, \
                                created_at, updated_at";

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AutomationRule {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub trigger_config: serde_json::Value,
    pub action_type: String,
    pub action_config: serde_json::Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRuleRequest {
    pub name: String,
    pub description: Option<String>,
    pub trigger_type: String,
    pub trigger_config: serde_json::Value,
    pub action_type: String,
    pub action_config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub trigger_type: Option<String>,
    pub trigger_config: Option<serde_json::Value>,
    pub action_type: Option<String>,
    pub action_config: Option<serde_json::Value>,
    /// `is_active` is the column's real name; the legacy `is_enabled` spelling is still accepted
    /// so an existing caller does not break. `None` = "leave as is" (see the COALESCE in the UPDATE).
    #[serde(alias = "is_enabled")]
    pub is_active: Option<bool>,
}
