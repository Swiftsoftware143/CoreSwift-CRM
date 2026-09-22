//! Communication delivery providers — Mailgun, SMTP.com, Telnyx.
//!
//! Each provider implements a `deliver` function called by the dispatcher.
//!
//! ## Which transport an email actually goes out on (t_193e2259)
//!
//! Delivery is LAYERED, in this order:
//!   1. **Tenant BYOK** — `tenants.settings->'communications'` holds BOTH a `mailgun_domain` and a
//!      `mailgun_api_key`. The tenant's own domain and From identity are used.
//!   2. **Platform transport** — the app's own `EMAIL_API_URL` / `EMAIL_API_KEY` / `EMAIL_FROM`
//!      (verified Mailgun domain). Used by every workspace that has not configured its own
//!      provider, which before this change was *every* workspace that had ever queued a mail
//!      (322 of 322): the platform settings existed in the environment but no code path read them,
//!      so the only outcome was a permanently `failed` row reading "Mailgun domain not configured".
//!   3. **Nothing** → a PERMANENT failure naming what is missing. Never a silent queue-and-forget.
//!
//! On the platform transport the `From` comes from `EMAIL_FROM` (it must be an address on the
//! DKIM-signing domain) and the tenant's own `from_email` becomes `h:Reply-To`, so replies still
//! reach the workspace without spoofing a domain the platform cannot sign for.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// Where a delivery attempt's transport came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportSource {
    /// The workspace's own provider credentials (BYOK).
    Tenant,
    /// The platform's own mail settings (`EMAIL_*`).
    Platform,
    /// Neither is configured — a delivery attempt cannot even be made.
    None,
}

impl TransportSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TransportSource::Tenant => "tenant",
            TransportSource::Platform => "platform",
            TransportSource::None => "none",
        }
    }
}

/// The platform's own mail transport, read from the environment.
#[derive(Debug, Clone)]
pub struct PlatformMail {
    /// Full Mailgun "send message" endpoint, e.g. https://api.mailgun.net/v3/<domain>/messages
    pub url: String,
    pub api_key: String,
    /// Must be an address on the domain `url` sends through, or receivers will DMARC-fail it.
    pub from: String,
}

/// Result of ONE delivery attempt.
///
/// `permanent` is the difference between "retrying cannot help" (missing/bad credentials, a
/// domain the provider does not know, a malformed request) and "this failed, try again later"
/// (network error, 429, 5xx). Before t_193e2259 both looked identical in the database:
/// every row stayed `failed` with `retry_count = 0` for ever.
#[derive(Debug, Clone)]
pub struct DeliveryOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub permanent: bool,
    pub provider: Option<String>,
    /// The provider's own id for the accepted message (Mailgun: the `<id@domain>` it returns).
    pub provider_message_id: Option<String>,
}

impl DeliveryOutcome {
    pub fn sent(provider: &str, provider_message_id: Option<String>) -> Self {
        Self {
            ok: true,
            error: None,
            permanent: false,
            provider: Some(provider.to_string()),
            provider_message_id,
        }
    }

    /// A failure that retrying cannot fix.
    pub fn permanent(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            permanent: true,
            provider: None,
            provider_message_id: None,
        }
    }

    /// A failure worth another attempt.
    pub fn transient(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(msg.into()),
            permanent: false,
            provider: None,
            provider_message_id: None,
        }
    }
}

/// The error text every un-deliverable email used to produce, kept verbatim so historical rows
/// and the new permanent failure are recognisably the same class.
pub const NOT_CONFIGURED_PREFIX: &str = "Email is not configured for this workspace";

/// A non-empty environment variable, trimmed.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The platform's own mail transport. `None` when it is incomplete — and the missing variables
/// are named at ERROR level (names only, never values) so an operator sees a misconfiguration
/// instead of a pile of failed rows.
pub fn platform_mail() -> Option<PlatformMail> {
    let url = env_nonempty("EMAIL_API_URL");
    let key = env_nonempty("EMAIL_API_KEY");
    let from = env_nonempty("EMAIL_FROM");
    match (url, key, from) {
        (Some(url), Some(api_key), Some(from)) => Some(PlatformMail { url, api_key, from }),
        (url, key, from) => {
            tracing::error!(
                email_api_url = url.is_some(),
                email_api_key = key.is_some(),
                email_from = from.is_some(),
                "{}: the platform mail transport is incomplete (EMAIL_API_URL / EMAIL_API_KEY / EMAIL_FROM)",
                NOT_CONFIGURED_PREFIX
            );
            None
        }
    }
}

/// The platform From identity, for the surfaces that need to show it (never the key).
pub fn platform_from() -> Option<String> {
    env_nonempty("EMAIL_FROM")
}

/// Can a delivery attempt be made at all, and on which transport? Used by the UI/API surface so
/// "email is not configured" is visible *before* somebody wonders why nothing arrived.
pub fn email_transport_status(tenant_has_byok: bool, tenant_domain: Option<&str>) -> Value {
    let (source, domain, from) = if tenant_has_byok {
        (
            TransportSource::Tenant,
            tenant_domain.map(|d| d.to_string()),
            None,
        )
    } else {
        match platform_mail() {
            Some(p) => {
                let domain = p
                    .url
                    .split("/v3/")
                    .nth(1)
                    .and_then(|rest| rest.split('/').next())
                    .map(|d| d.to_string());
                (TransportSource::Platform, domain, Some(p.from))
            }
            None => (TransportSource::None, None, None),
        }
    };
    json!({
        "source": source.as_str(),
        "configured": source != TransportSource::None,
        "domain": domain,
        "from": from,
        "note": match source {
            TransportSource::Tenant => "using this workspace's own provider credentials",
            TransportSource::Platform => "using the platform mail transport (set a provider domain and key to send from your own domain)",
            TransportSource::None => "no email transport is configured for this workspace",
        }
    })
}

/// Is an HTTP status from a provider worth retrying? False = permanent.
pub fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429) || (500..=599).contains(&status)
}

/// Delay before attempt number `attempt` (1-based count of attempts made so far), in minutes:
/// 2, 4, 8, 16, 32, 60 capped. A transient failure must back off, not hammer the provider.
pub fn backoff_minutes(attempt: i32) -> i64 {
    let shift = attempt.clamp(1, 6) as u32;
    (1i64 << shift).min(60)
}

/// The tenant's `settings->'communications'`, or `{}`.
async fn tenant_comms(db: &PgPool, tenant_id: Uuid) -> Value {
    let settings: Option<Value> =
        sqlx::query_scalar("SELECT settings->'communications' FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_optional(db)
            .await
            .unwrap_or(None)
            .flatten();
    settings.unwrap_or(json!({}))
}

/// Configuration loaded for a single delivery attempt.
#[derive(Debug, Clone)]
pub struct DeliveryConfig {
    pub tenant_id: Uuid,
    pub msg_id: Uuid,
    pub channel: String,
    pub to: String,
    pub subject: Option<String>,
    pub body: String,
    pub email_provider: String,
    pub sms_provider: String,
    /// The transport this attempt will use (tenant BYOK / platform / none).
    pub transport: TransportSource,
    /// Full send-message endpoint for the resolved transport.
    pub mailgun_url: Option<String>,
    /// `h:Reply-To`, set when the From had to come from the platform rather than the tenant.
    pub reply_to: Option<String>,
    pub mailgun_domain: Option<String>,
    pub mailgun_api_key: Option<String>,
    pub telnyx_api_key: Option<String>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_username: Option<String>,
    pub smtp_password: Option<String>,
    pub from_email: Option<String>,
    pub from_name: Option<String>,
    pub whatsapp_phone_number_id: Option<String>,
    pub whatsapp_api_token: Option<String>,
}

/// Bound anything that came back from a provider before it goes into an error message or a log
/// line: the message must stay readable and must never carry a whole response body.
fn snippet(s: &str) -> String {
    let s = s.trim().replace(['\n', '\r'], " ");
    s.chars().take(200).collect()
}

/// Attempt delivery via the configured provider chain.
pub async fn deliver(cfg: &DeliveryConfig) -> DeliveryOutcome {
    match cfg.channel.as_str() {
        "email" => deliver_email(cfg).await,
        "sms" => deliver_sms(cfg).await,
        "whatsapp" => deliver_whatsapp(cfg).await,
        other => DeliveryOutcome::permanent(format!("Unknown channel: {}", other)),
    }
}

async fn deliver_email(cfg: &DeliveryConfig) -> DeliveryOutcome {
    match cfg.email_provider.as_str() {
        "mailgun" => deliver_via_mailgun(cfg).await,
        "smtp" => deliver_via_smtp(cfg).await,
        other => DeliveryOutcome::permanent(format!("Unknown email provider: {}", other)),
    }
}

/// Send email through the transport resolved in `load_delivery_config` — the workspace's own
/// Mailgun domain/key, or the platform's. The URL is resolved once, at config time, so this
/// function cannot disagree with the transport the caller reported.
async fn deliver_via_mailgun(cfg: &DeliveryConfig) -> DeliveryOutcome {
    let url = match &cfg.mailgun_url {
        Some(u) => u,
        None => {
            return DeliveryOutcome::permanent(format!(
                "{NOT_CONFIGURED_PREFIX}: no Mailgun domain/key on this workspace and no platform EMAIL_API_URL/EMAIL_API_KEY. Set a provider domain and key in Provider settings."
            ))
        }
    };
    let api_key = match &cfg.mailgun_api_key {
        Some(k) => k,
        None => {
            return DeliveryOutcome::permanent(format!(
                "{NOT_CONFIGURED_PREFIX}: no Mailgun API key ({})",
                cfg.transport.as_str()
            ))
        }
    };
    let domain = cfg
        .mailgun_domain
        .clone()
        .unwrap_or_else(|| url.split("/v3/").nth(1).unwrap_or("").to_string());

    // On the platform transport `from_email` is already EMAIL_FROM (set in load_delivery_config);
    // the tenant's own identity rides along as Reply-To so replies still reach the workspace.
    let from = cfg.from_email.as_deref().unwrap_or("noreply@crm-swift.com");

    let mut params = std::collections::HashMap::new();
    params.insert("from", from);
    params.insert("to", cfg.to.as_str());
    params.insert("subject", cfg.subject.as_deref().unwrap_or("No subject"));
    params.insert("text", cfg.body.as_str());
    if let Some(reply_to) = cfg.reply_to.as_deref() {
        params.insert("h:Reply-To", reply_to);
    }

    let client = reqwest::Client::new();
    match client
        .post(url)
        .basic_auth("api", Some(api_key))
        .form(&params)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            if (200..300).contains(&status) {
                // Mailgun answers with the id it assigned: {"id":"<...@domain>","message":"Queued..."}
                let provider_message_id = serde_json::from_str::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()));
                tracing::info!(
                    msg = %cfg.msg_id, transport = cfg.transport.as_str(), domain = %domain,
                    provider_message_id = provider_message_id.as_deref().unwrap_or("(none returned)"),
                    "Mailgun accepted the message"
                );
                DeliveryOutcome::sent("mailgun", provider_message_id)
            } else {
                let err = format!(
                    "Mailgun returned {} for {}: {}",
                    status,
                    domain,
                    snippet(&body)
                );
                if retryable_status(status) {
                    tracing::warn!(msg = %cfg.msg_id, status, "Mailgun delivery failed (transient)");
                    DeliveryOutcome::transient(err)
                } else {
                    tracing::error!(msg = %cfg.msg_id, status, "Mailgun rejected the message (permanent)");
                    DeliveryOutcome::permanent(err)
                }
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "Mailgun request failed (transient)");
            DeliveryOutcome::transient(format!("Mailgun error: {}", e))
        }
    }
}

/// Send email via SMTP.com REST API
async fn deliver_via_smtp(cfg: &DeliveryConfig) -> DeliveryOutcome {
    let api_key = match &cfg.smtp_password {
        Some(k) => k,
        None => return DeliveryOutcome::permanent("SMTP.com API key not configured"),
    };

    let from = cfg.from_email.as_deref().unwrap_or("noreply@crm-swift.com");
    let url = "https://api.smtp.com/v4/messages";

    let payload = serde_json::json!({
        "from": { "email": from, "name": cfg.from_name.as_deref().unwrap_or("CRM Swift") },
        "to": [{ "email": &cfg.to }],
        "subject": cfg.subject.as_deref().unwrap_or("No subject"),
        "textbody": &cfg.body,
    });

    let client = reqwest::Client::new();
    match client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                tracing::info!(msg = %cfg.msg_id, "SMTP.com delivery successful");
                DeliveryOutcome::sent("smtp.com", None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "SMTP.com delivery failed");
                let err = format!("SMTP.com returned {}: {}", status, snippet(&body));
                if retryable_status(status) {
                    DeliveryOutcome::transient(err)
                } else {
                    DeliveryOutcome::permanent(err)
                }
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "SMTP.com request failed");
            DeliveryOutcome::transient(format!("SMTP.com error: {}", e))
        }
    }
}

/// Send WhatsApp via Meta/WhatsApp Business Cloud API
async fn deliver_whatsapp(cfg: &DeliveryConfig) -> DeliveryOutcome {
    let phone_number_id = match &cfg.whatsapp_phone_number_id {
        Some(id) => id,
        None => return DeliveryOutcome::permanent("WhatsApp phone number ID not configured"),
    };
    let api_token = match &cfg.whatsapp_api_token {
        Some(t) => t,
        None => return DeliveryOutcome::permanent("WhatsApp API token not configured"),
    };

    let url = format!(
        "https://graph.facebook.com/v21.0/{}/messages",
        phone_number_id
    );
    let payload = serde_json::json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": &cfg.to,
        "type": "text",
        "text": {
            "preview_url": false,
            "body": &cfg.body
        }
    });

    let client = reqwest::Client::new();
    match client
        .post(&url)
        .header("Authorization", format!("Bearer {}", api_token))
        .header("Content-Type", "application/json")
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                tracing::info!(msg = %cfg.msg_id, "WhatsApp delivery successful");
                DeliveryOutcome::sent("whatsapp", None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "WhatsApp delivery failed");
                let err = format!("WhatsApp returned {}: {}", status, snippet(&body));
                if retryable_status(status) {
                    DeliveryOutcome::transient(err)
                } else {
                    DeliveryOutcome::permanent(err)
                }
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "WhatsApp request failed");
            DeliveryOutcome::transient(format!("WhatsApp error: {}", e))
        }
    }
}

/// Send SMS via Telnyx REST API
async fn deliver_sms(cfg: &DeliveryConfig) -> DeliveryOutcome {
    let api_key = match &cfg.telnyx_api_key {
        Some(k) => k,
        None => return DeliveryOutcome::permanent("Telnyx API key not configured"),
    };

    let from = cfg.from_email.as_deref().unwrap_or("+15555555555");
    let url = "https://api.telnyx.com/v2/messages";

    let payload = serde_json::json!({
        "from": from,
        "to": &cfg.to,
        "text": &cfg.body,
    });

    let client = reqwest::Client::new();
    match client
        .post(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if (200..300).contains(&status) {
                tracing::info!(msg = %cfg.msg_id, "Telnyx SMS accepted");
                DeliveryOutcome::sent("telnyx", None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "Telnyx delivery failed");
                let err = format!("Telnyx returned {}: {}", status, snippet(&body));
                if retryable_status(status) {
                    DeliveryOutcome::transient(err)
                } else {
                    DeliveryOutcome::permanent(err)
                }
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "Telnyx request failed");
            DeliveryOutcome::transient(format!("Telnyx error: {}", e))
        }
    }
}

/// The email transport resolved for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEmail {
    pub transport: TransportSource,
    pub url: Option<String>,
    pub api_key: Option<String>,
    pub from: Option<String>,
    /// Set only on the platform transport: the workspace's own identity, so replies reach it
    /// even though the From has to be the platform's signable address.
    pub reply_to: Option<String>,
    pub tenant_domain: Option<String>,
}

/// The layering decision, as a pure function: tenant BYOK when BOTH halves are present, else the
/// platform transport, else nothing. No env, no DB — so the policy itself is unit-testable.
///
/// A workspace holding only one half of a BYOK pair is NOT an error to swallow and NOT a reason to
/// redirect mail to a half-configured provider: it falls through to the platform transport (the
/// caller logs which half is missing), because a password reset must go out either way.
pub fn resolve_email(
    tenant_domain: Option<String>,
    tenant_key: Option<String>,
    tenant_from: Option<String>,
    platform: Option<PlatformMail>,
) -> ResolvedEmail {
    match (tenant_domain, tenant_key) {
        (Some(domain), Some(key)) => ResolvedEmail {
            transport: TransportSource::Tenant,
            url: Some(format!("https://api.mailgun.net/v3/{}/messages", domain)),
            api_key: Some(key),
            from: tenant_from,
            reply_to: None,
            tenant_domain: Some(domain),
        },
        (domain, _key) => match platform {
            Some(p) => ResolvedEmail {
                transport: TransportSource::Platform,
                url: Some(p.url),
                api_key: Some(p.api_key),
                from: Some(p.from),
                reply_to: tenant_from,
                tenant_domain: domain,
            },
            None => ResolvedEmail {
                transport: TransportSource::None,
                url: None,
                api_key: None,
                from: tenant_from,
                reply_to: None,
                tenant_domain: domain,
            },
        },
    }
}

/// Load delivery configuration, resolving WHICH transport this attempt uses (see `resolve_email`).
pub async fn load_delivery_config(
    db: &PgPool,
    msg_id: Uuid,
    tenant_id: Uuid,
    channel: &str,
    to: &str,
    subject: Option<String>,
    body: &str,
) -> DeliveryConfig {
    let comms = tenant_comms(db, tenant_id).await;

    let field = |k: &str| -> Option<String> {
        comms
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    let tenant_domain = field("mailgun_domain");
    let tenant_key = field("mailgun_api_key");
    let tenant_from = field("from_email");

    if tenant_domain.is_some() != tenant_key.is_some() {
        tracing::warn!(
            tenant = %tenant_id,
            mailgun_domain_set = tenant_domain.is_some(),
            mailgun_api_key_set = tenant_key.is_some(),
            "Workspace has half a Mailgun configuration — falling back to the platform mail transport"
        );
    }

    let resolved = resolve_email(tenant_domain, tenant_key, tenant_from, platform_mail());

    DeliveryConfig {
        tenant_id,
        msg_id,
        channel: channel.to_string(),
        to: to.to_string(),
        subject,
        body: body.to_string(),
        email_provider: field("email_provider").unwrap_or_else(|| "mailgun".to_string()),
        sms_provider: field("sms_provider").unwrap_or_else(|| "telnyx".to_string()),
        transport: resolved.transport,
        mailgun_url: resolved.url,
        reply_to: resolved.reply_to,
        mailgun_domain: resolved.tenant_domain,
        mailgun_api_key: resolved.api_key,
        telnyx_api_key: field("telnyx_api_key"),
        whatsapp_phone_number_id: field("whatsapp_phone_number_id"),
        whatsapp_api_token: field("whatsapp_api_token"),
        smtp_host: field("smtp_host"),
        smtp_port: comms
            .get("smtp_port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16),
        smtp_username: field("smtp_username"),
        smtp_password: field("smtp_password"),
        from_email: resolved.from,
        from_name: field("from_name"),
    }
}

/// What recording one attempt did to the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptRecord {
    Sent {
        message_id: Option<String>,
    },
    /// Permanent failure — terminal, no further attempt.
    Failed {
        attempts: i32,
    },
    /// Transient failure — requeued with backoff.
    Retrying {
        attempt: i32,
        max: i32,
        next_at: chrono::DateTime<chrono::Utc>,
    },
    /// Transient failure and out of attempts — terminal.
    GaveUp {
        attempts: i32,
    },
}

/// Persist the result of ONE delivery attempt against `outbound_messages`.
///
/// This is the only place the permanent/transient policy is applied, so the worker poll, the
/// UI "send" path and the automation actions all record the same way:
///   * success → `sent`, `sent_at`, the provider's `message_id` + `provider`, error cleared;
///   * permanent → `failed` on the first attempt, error prefixed `permanent:`;
///   * transient → `queued` again with `scheduled_at` = now + exponential backoff, until
///     `max_retries` attempts have been made, then `failed` with `transient: gave up after N`.
///
/// `retry_count` is incremented on every attempt, so a row can no longer sit at 0 while it is
/// being retried, and "gave up" can no longer look like "never tried".
pub async fn record_attempt(
    db: &PgPool,
    msg_id: Uuid,
    outcome: &DeliveryOutcome,
) -> Result<AttemptRecord, sqlx::Error> {
    if outcome.ok {
        sqlx::query(
            "UPDATE outbound_messages
                SET status = 'sent', sent_at = NOW(), error_message = NULL,
                    message_id = $2, provider = $3, scheduled_at = NULL
              WHERE id = $1",
        )
        .bind(msg_id)
        .bind(outcome.provider_message_id.as_deref())
        .bind(outcome.provider.as_deref())
        .execute(db)
        .await?;
        return Ok(AttemptRecord::Sent {
            message_id: outcome.provider_message_id.clone(),
        });
    }

    let counts: Option<(i32, i32)> = sqlx::query_as(
        "SELECT retry_count, COALESCE(max_retries, 3) FROM outbound_messages WHERE id = $1",
    )
    .bind(msg_id)
    .fetch_optional(db)
    .await?;
    let (retry_count, max_retries) = counts.unwrap_or((0, 3));
    let attempts = retry_count + 1;
    let err = outcome
        .error
        .clone()
        .unwrap_or_else(|| "delivery failed with no error text".to_string());

    if outcome.permanent || attempts >= max_retries {
        let message = if outcome.permanent {
            format!("permanent: {err}")
        } else {
            format!(
                "transient: gave up after {attempts} attempt(s) (max_retries {max_retries}): {err}"
            )
        };
        sqlx::query(
            "UPDATE outbound_messages
                SET status = 'failed', error_message = $2, retry_count = $3, scheduled_at = NULL
              WHERE id = $1",
        )
        .bind(msg_id)
        .bind(&message)
        .bind(attempts)
        .execute(db)
        .await?;
        tracing::error!(msg = %msg_id, error = %message, "outbound message FAILED (no further attempt)");
        return Ok(if outcome.permanent {
            AttemptRecord::Failed { attempts }
        } else {
            AttemptRecord::GaveUp { attempts }
        });
    }

    let next_at = chrono::Utc::now() + chrono::Duration::minutes(backoff_minutes(attempts));
    let message = format!("transient (attempt {attempts}/{max_retries}): {err}");
    sqlx::query(
        "UPDATE outbound_messages
            SET status = 'queued', error_message = $2, retry_count = $3, scheduled_at = $4
          WHERE id = $1",
    )
    .bind(msg_id)
    .bind(&message)
    .bind(attempts)
    .bind(next_at)
    .execute(db)
    .await?;
    tracing::warn!(msg = %msg_id, retry_at = %next_at, error = %message, "outbound message will be retried");
    Ok(AttemptRecord::Retrying {
        attempt: attempts,
        max: max_retries,
        next_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform() -> PlatformMail {
        PlatformMail {
            url: "https://api.mailgun.net/v3/mail.coreswiftcrm.com/messages".to_string(),
            api_key: "key-platform".to_string(),
            from: "CoreSwift CRM <noreply@mail.coreswiftcrm.com>".to_string(),
        }
    }

    #[test]
    fn tenant_byok_with_both_halves_wins_over_the_platform() {
        let r = resolve_email(
            Some("mail.tenant.example".to_string()),
            Some("key-tenant".to_string()),
            Some("hello@tenant.example".to_string()),
            Some(platform()),
        );
        assert_eq!(r.transport, TransportSource::Tenant);
        assert_eq!(
            r.url.as_deref(),
            Some("https://api.mailgun.net/v3/mail.tenant.example/messages")
        );
        assert_eq!(r.api_key.as_deref(), Some("key-tenant"));
        // The tenant's own domain is signable, so its own From is used and nothing is redirected.
        assert_eq!(r.from.as_deref(), Some("hello@tenant.example"));
        assert_eq!(r.reply_to, None);
    }

    #[test]
    fn no_tenant_config_falls_back_to_the_platform_transport() {
        let r = resolve_email(
            None,
            None,
            Some("hello@tenant.example".to_string()),
            Some(platform()),
        );
        assert_eq!(r.transport, TransportSource::Platform);
        assert_eq!(
            r.url.as_deref(),
            Some("https://api.mailgun.net/v3/mail.coreswiftcrm.com/messages")
        );
        // From must be the signable platform address; the workspace keeps the replies.
        assert_eq!(
            r.from.as_deref(),
            Some("CoreSwift CRM <noreply@mail.coreswiftcrm.com>")
        );
        assert_eq!(r.reply_to.as_deref(), Some("hello@tenant.example"));
    }

    #[test]
    fn half_a_byok_pair_is_not_used_and_falls_through() {
        // domain but no key
        let r = resolve_email(
            Some("mail.tenant.example".to_string()),
            None,
            None,
            Some(platform()),
        );
        assert_eq!(r.transport, TransportSource::Platform);
        // key but no domain
        let r = resolve_email(None, Some("key-tenant".to_string()), None, Some(platform()));
        assert_eq!(r.transport, TransportSource::Platform);
    }

    #[test]
    fn nothing_configured_resolves_to_no_transport_at_all() {
        let r = resolve_email(None, None, None, None);
        assert_eq!(r.transport, TransportSource::None);
        assert!(r.url.is_none() && r.api_key.is_none());
        // and the delivery attempt then fails permanently, naming the problem
        let o = DeliveryOutcome::permanent(format!("{NOT_CONFIGURED_PREFIX}: nothing configured"));
        assert!(o.permanent && !o.ok);
        assert!(o.error.unwrap().starts_with(NOT_CONFIGURED_PREFIX));
    }

    #[test]
    fn only_5xx_rate_limits_and_timeouts_are_worth_a_retry() {
        for s in [408u16, 425, 429, 500, 502, 503, 504] {
            assert!(retryable_status(s), "{s} should be retryable");
        }
        for s in [200u16, 201, 400, 401, 402, 403, 404, 405, 415, 422] {
            assert!(!retryable_status(s), "{s} should be permanent");
        }
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_minutes(1), 2);
        assert_eq!(backoff_minutes(2), 4);
        assert_eq!(backoff_minutes(3), 8);
        assert_eq!(backoff_minutes(6), 60);
        assert_eq!(backoff_minutes(9), 60);
        assert_eq!(backoff_minutes(0), 2);
    }

    #[test]
    fn outcomes_carry_their_own_classification() {
        let s = DeliveryOutcome::sent("mailgun", Some("<id@mail.coreswiftcrm.com>".to_string()));
        assert!(s.ok && !s.permanent && s.error.is_none());
        assert_eq!(s.provider.as_deref(), Some("mailgun"));
        let t = DeliveryOutcome::transient("connection reset");
        assert!(!t.ok && !t.permanent);
        let p = DeliveryOutcome::permanent("Mailgun returned 401");
        assert!(!p.ok && p.permanent);
    }

    #[test]
    fn provider_bodies_are_bounded_before_they_reach_a_log_line() {
        let long = format!("line one\nline two {}", "x".repeat(500));
        let s = snippet(&long);
        assert_eq!(s.chars().count(), 200);
        assert!(!s.contains('\n'));
    }
}
