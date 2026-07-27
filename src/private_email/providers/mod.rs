//! Email provider abstraction — Mailgun, SMTP, SES, Postmark.
//!
//! Each provider implements the `EmailProvider` trait. The send/receive layer
//! calls the trait, never a specific provider. Adding a new provider means
//! implementing the trait — no changes needed in the routing or handler code.
//!
//! ## Inbound model
//! - **Push (webhook)**: Provider POSTs to our webhook endpoint. Used by Mailgun,
//!   SES (SNS), Postmark. The `accept_inbound` method validates the webhook payload.
//! - **None**: No inbound configured for this domain. Send-only.

pub mod mailgun;
pub mod smtp;

use async_trait::async_trait;
use uuid::Uuid;

/// Result of a send attempt.
#[derive(Debug)]
pub struct SendResult {
    pub success: bool,
    pub provider_message_id: Option<String>,
    pub error: Option<String>,
}

/// Parsed inbound email from any provider's webhook.
#[derive(Debug, Clone)]
pub struct InboundEmail {
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body_plain: String,
    pub body_html: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub provider_message_id: Option<String>,
}

/// Configuration for a domain's email provider, loaded from the database.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub tenant_id: Uuid,
    pub domain_id: Uuid,
    pub domain: String,
    pub provider_type: String, // "mailgun", "smtp", "ses", "postmark"
    pub encrypted_api_key: Option<String>,
    pub region: Option<String>,
    // SMTP-specific
    pub smtp_host: Option<String>,
    pub smtp_port: Option<i32>,
    pub smtp_username: Option<String>,
    pub encrypted_smtp_password: Option<String>,
    pub smtp_tls: bool,
    // Inbound
    pub inbound_mode: String,
    pub encrypted_webhook_key: Option<String>,
}

/// The unified email provider trait. All providers implement this.
#[async_trait]
#[async_trait::async_trait]
pub trait EmailProvider: Send + Sync {
    /// Human-readable name for logging/debugging.
    fn name(&self) -> &'static str;

    /// Send an outbound email. Returns result with optional provider message ID.
    async fn send(
        &self,
        config: &ProviderConfig,
        from: &str,
        to: &str,
        subject: &str,
        body_html: &str,
        in_reply_to: Option<&str>,
    ) -> SendResult;

    /// Validate and parse an inbound webhook payload into a normalized InboundEmail.
    /// Returns None if signature validation fails or the payload isn't a valid inbound message.
    fn accept_inbound(&self, config: &ProviderConfig, body: &[u8]) -> Option<InboundEmail>;
}

/// Build the correct provider from a config.
pub fn provider_for(config: &ProviderConfig) -> Box<dyn EmailProvider> {
    match config.provider_type.as_str() {
        "smtp" => Box::new(smtp::SmtpProvider),
        _ => Box::new(mailgun::MailgunProvider),
    }
}
