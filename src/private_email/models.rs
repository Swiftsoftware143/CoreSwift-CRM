use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

// ── Data Retention Types ──

/// Response returned when querying a tenant's retention settings.
#[derive(Debug, Clone, Serialize)]
pub struct RetentionResponse {
    pub tenant_id: Uuid,
    pub retention_days: i32,
    pub last_purged_at: Option<DateTime<Utc>>,
}

/// Request body for setting a tenant's email retention period.
/// Minimum: 30 days, Maximum: 3650 days (10 years).
#[derive(Debug, Deserialize)]
pub struct SetRetentionRequest {
    #[serde(default = "default_retention")]
    pub retention_days: i32,
}

fn default_retention() -> i32 {
    365
}

/// Request body for triggering a manual purge.
#[derive(Debug, Deserialize)]
pub struct PurgeTriggerRequest {
    /// If provided, limits the purge to a single tenant.
    /// If omitted (agency_admin only), purges all tenants.
    pub tenant_id: Option<Uuid>,
}

/// Summary of what the purge task accomplished.
#[derive(Debug, Clone, Serialize)]
pub struct PurgeSummaryResponse {
    pub tenants_checked: i64,
    pub tenants_purged: i64,
    pub messages_deleted: i64,
    pub events_deleted: i64,
    pub errors: Vec<String>,
}

impl Default for PurgeSummaryResponse {
    fn default() -> Self {
        Self::new()
    }
}

impl PurgeSummaryResponse {
    pub fn new() -> Self {
        Self {
            tenants_checked: 0,
            tenants_purged: 0,
            messages_deleted: 0,
            events_deleted: 0,
            errors: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PrivateEmailDomain {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub domain: String,
    /// Credential columns: NEVER serialized (t_72f0bc83). `list_domains`/`update_domain` answer with
    /// `serde_json::to_value(&domain)`, so before this these fields shipped the tenant's stored
    /// ciphertext — and, on a legacy unsealed row, its PLAINTEXT — to the browser, while no client
    /// reads them (0 references in either served shell). The status of a credential is now reported
    /// by `credential_status`, which is computed by opening the value, never by echoing it.
    #[serde(skip_serializing)]
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
    #[serde(skip_serializing)]
    pub smtp_password_encrypted: Option<String>,
    pub smtp_tls: bool,
    pub inbound_mode: String,
    #[serde(skip_serializing)]
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

fn default_region() -> String {
    "us".into()
}
fn default_provider_type() -> String {
    "mailgun".into()
}
fn default_true() -> bool {
    true
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
    /// Re-enter a domain's own Mailgun credential (t_72f0bc83). A stored key this deployment cannot
    /// open made the domain permanently unusable: the send path fails closed with "…cannot be read
    /// — re-add the domain", and there was no way to re-add one without deleting it (and every
    /// mailbox on it). Sealed on write; empty/omitted leaves the stored value alone.
    #[serde(default)]
    pub mailgun_api_key: Option<String>,
    /// Re-enter a domain's SMTP password (`provider_type = "smtp"`), same contract as above.
    #[serde(default)]
    pub smtp_password: Option<String>,
    /// Point the domain at a DIFFERENT saved key (from `GET /private-email/keys`).
    #[serde(default)]
    pub api_key_id: Option<Uuid>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
