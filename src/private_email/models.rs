use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PrivateEmailDomain {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub domain: String,
    pub mailgun_api_key: String,
    pub mailgun_region: String,
    pub catch_all_enabled: bool,
    pub verified: bool,
    pub label: Option<String>,
    pub api_key_id: Option<Uuid>,
    pub provider_type: String,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_username: Option<String>,
    pub smtp_password_encrypted: Option<String>,
    pub smtp_tls: bool,
    pub inbound_mode: String,
    pub webhook_signing_key_encrypted: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PrivateEmailBox {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub domain_id: Uuid,
    pub user_id: Option<Uuid>,
    pub local_part: String,
    pub email_address: String,
    pub mailgun_mailbox_id: Option<String>,
    pub forwarding_enabled: bool,
    pub signature: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// Request/response types

#[derive(Debug, Deserialize)]
pub struct AddDomainRequest {
    pub domain: String,
    pub label: Option<String>,
    #[serde(default)]
    pub mailgun_api_key: Option<String>,
    pub api_key_id: Option<Uuid>,
    #[serde(default = "default_region")]
    pub mailgun_region: String,
    // Provider type selection
    #[serde(default = "default_provider_type")]
    pub provider_type: String,
    // SMTP provider config (when provider_type = "smtp")
    pub smtp_host: Option<String>,
    #[serde(default)]
    pub smtp_port: Option<i32>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
}

fn default_region() -> String { "us".into() }
fn default_provider_type() -> String { "mailgun".into() }
fn default_true() -> bool { true }

#[derive(Debug, Deserialize)]
pub struct ProvisionMailboxRequest {
    pub domain_id: Uuid,
    pub local_part: String,
    pub user_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct SendEmailRequest {
    pub from_address: String,
    pub to: String,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDomainRequest {
    pub catch_all_enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMailboxRequest {
    pub signature: Option<String>,
    pub forwarding_enabled: Option<bool>,
}

// Admin override types

/// Request to set tenant email limit overrides (agency_admin only).
#[derive(Debug, Deserialize)]
pub struct SetTenantEmailLimitsRequest {
    pub max_domains: Option<i32>,
    pub max_mailboxes: Option<i32>,
    pub max_aliases_per_mailbox: Option<i32>,
}

/// Tenant email limits row from DB.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TenantEmailLimits {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub max_domains: Option<i32>,
    pub max_mailboxes: Option<i32>,
    pub max_aliases_per_mailbox: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Combined view: plan defaults + any tenant override.
#[derive(Debug, Clone, Serialize)]
pub struct TenantEmailLimitsResponse {
    pub tenant_id: Uuid,
    pub plan_defaults: PrivateEmailPlanFeatures,
    pub overrides: Option<TenantEmailLimits>,
    pub effective_max_domains: i32,
    pub effective_max_mailboxes: i32,
    pub effective_max_aliases_per_mailbox: i32,
}

// Plan feature limits

#[derive(Debug, Clone, Serialize, Deserialize)]
#[derive(Default)]
pub struct PrivateEmailPlanFeatures {
    #[serde(default)]
    pub private_email: bool,
    #[serde(default)]
    pub max_domains: i32,
    #[serde(default)]
    pub max_mailboxes: i32,
    #[serde(default)]
    pub max_aliases_per_mailbox: i32,
    #[serde(default)]
    pub catch_all_enabled: bool,
}
