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
    pub mailgun_api_key: String,
    #[serde(default = "default_region")]
    pub mailgun_region: String,
}

fn default_region() -> String {
    "us".into()
}

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

// Plan feature limits

#[derive(Debug, Deserialize)]
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
