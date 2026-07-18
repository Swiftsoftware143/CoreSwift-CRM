//! Communication delivery providers — Mailgun, SMTP.com, Telnyx.
//!
//! Each provider implements a `deliver` function called by the dispatcher.
//! Configured per-tenant via `tenants.settings->'communications'`.

use sqlx::PgPool;
use uuid::Uuid;
use serde_json::{json, Value};

/// Configuration loaded from tenant settings for a single delivery attempt.
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

/// Attempt delivery via the configured provider chain.
/// Returns (success: bool, error_message: Option<String>).
pub async fn deliver(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    match cfg.channel.as_str() {
        "email" => deliver_email(cfg).await,
        "sms" => deliver_sms(cfg).await,
        "whatsapp" => deliver_whatsapp(cfg).await,
        _ => (false, Some(format!("Unknown channel: {}", cfg.channel))),
    }
}

async fn deliver_email(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    match cfg.email_provider.as_str() {
        "mailgun" => deliver_via_mailgun(cfg).await,
        "smtp" => deliver_via_smtp(cfg).await,
        other => (false, Some(format!("Unknown email provider: {}", other))),
    }
}

/// Send email via Mailgun REST API
async fn deliver_via_mailgun(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    let domain = match &cfg.mailgun_domain {
        Some(d) => d,
        None => return (false, Some("Mailgun domain not configured".to_string())),
    };
    let api_key = match &cfg.mailgun_api_key {
        Some(k) => k,
        None => return (false, Some("Mailgun API key not configured".to_string())),
    };

    let from = cfg.from_email.as_deref().unwrap_or("noreply@crm-swift.com");
    let url = format!("https://api.mailgun.net/v3/{}/messages", domain);

    let mut params = std::collections::HashMap::new();
    params.insert("from", from);
    params.insert("to", &cfg.to);
    params.insert("subject", cfg.subject.as_deref().unwrap_or("No subject"));
    params.insert("text", &cfg.body);

    let client = reqwest::Client::new();
    match client
        .post(&url)
        .basic_auth("api", Some(api_key))
        .form(&params)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() {
                tracing::info!(msg = %cfg.msg_id, "Mailgun delivery successful");
                (true, None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "Mailgun delivery failed");
                (false, Some(format!("Mailgun returned {}", status)))
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "Mailgun request failed");
            (false, Some(format!("Mailgun error: {}", e)))
        }
    }
}

/// Send email via SMTP.com REST API
async fn deliver_via_smtp(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    let api_key = match &cfg.smtp_password {
        Some(k) => k,
        None => return (false, Some("SMTP.com API key not configured".to_string())),
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
            let status = resp.status();
            if status.is_success() {
                tracing::info!(msg = %cfg.msg_id, "SMTP.com delivery successful");
                (true, None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "SMTP.com delivery failed");
                (false, Some(format!("SMTP.com returned {}", status)))
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "SMTP.com request failed");
            (false, Some(format!("SMTP.com error: {}", e)))
        }
    }
}

/// Send WhatsApp via Meta/WhatsApp Business Cloud API
async fn deliver_whatsapp(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    let phone_number_id = match &cfg.whatsapp_phone_number_id {
        Some(id) => id,
        None => return (false, Some("WhatsApp phone number ID not configured".to_string())),
    };
    let api_token = match &cfg.whatsapp_api_token {
        Some(t) => t,
        None => return (false, Some("WhatsApp API token not configured".to_string())),
    };

    let url = format!("https://graph.facebook.com/v21.0/{}/messages", phone_number_id);
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
            let status = resp.status();
            if status.is_success() || status == 200 || status == 201 {
                tracing::info!(msg = %cfg.msg_id, "WhatsApp delivery successful");
                (true, None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "WhatsApp delivery failed");
                (false, Some(format!("WhatsApp returned {}", status)))
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "WhatsApp request failed");
            (false, Some(format!("WhatsApp error: {}", e)))
        }
    }
}

/// Send SMS via Telnyx REST API
async fn deliver_sms(cfg: &DeliveryConfig) -> (bool, Option<String>) {
    let api_key = match &cfg.telnyx_api_key {
        Some(k) => k,
        None => return (false, Some("Telnyx API key not configured".to_string())),
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
            let status = resp.status();
            if status.is_success() || status == 202 {
                tracing::info!(msg = %cfg.msg_id, "Telnyx SMS accepted");
                (true, None)
            } else {
                let body = resp.text().await.unwrap_or_default();
                tracing::warn!(msg = %cfg.msg_id, status = %status, body = %body, "Telnyx delivery failed");
                (false, Some(format!("Telnyx returned {}", status)))
            }
        }
        Err(e) => {
            tracing::warn!(msg = %cfg.msg_id, error = %e, "Telnyx request failed");
            (false, Some(format!("Telnyx error: {}", e)))
        }
    }
}

/// Load delivery configuration from tenant settings.
pub async fn load_delivery_config(
    db: &PgPool,
    msg_id: Uuid,
    tenant_id: Uuid,
    channel: &str,
    to: &str,
    subject: Option<String>,
    body: &str,
) -> DeliveryConfig {
    let settings: Option<Value> = sqlx::query_scalar(
        "SELECT settings->'communications' FROM tenants WHERE id = $1"
    ).bind(tenant_id).fetch_optional(db).await.unwrap_or(None).flatten();

    let comms = settings.unwrap_or(json!({}));

    DeliveryConfig {
        tenant_id,
        msg_id,
        channel: channel.to_string(),
        to: to.to_string(),
        subject,
        body: body.to_string(),
        email_provider: comms.get("email_provider").and_then(|v| v.as_str()).unwrap_or("mailgun").to_string(),
        sms_provider: comms.get("sms_provider").and_then(|v| v.as_str()).unwrap_or("telnyx").to_string(),
        mailgun_domain: comms.get("mailgun_domain").and_then(|v| v.as_str()).map(|s| s.to_string()),
        mailgun_api_key: comms.get("mailgun_api_key").and_then(|v| v.as_str()).map(|s| s.to_string()),
        telnyx_api_key: comms.get("telnyx_api_key").and_then(|v| v.as_str()).map(|s| s.to_string()),
        whatsapp_phone_number_id: comms.get("whatsapp_phone_number_id").and_then(|v| v.as_str()).map(|s| s.to_string()),
        whatsapp_api_token: comms.get("whatsapp_api_token").and_then(|v| v.as_str()).map(|s| s.to_string()),
        smtp_host: comms.get("smtp_host").and_then(|v| v.as_str()).map(|s| s.to_string()),
        smtp_port: comms.get("smtp_port").and_then(|v| v.as_u64()).map(|p| p as u16),
        smtp_username: comms.get("smtp_username").and_then(|v| v.as_str()).map(|s| s.to_string()),
        smtp_password: comms.get("smtp_password").and_then(|v| v.as_str()).map(|s| s.to_string()),
        from_email: comms.get("from_email").and_then(|v| v.as_str()).map(|s| s.to_string()),
        from_name: comms.get("from_name").and_then(|v| v.as_str()).map(|s| s.to_string()),
    }
}
