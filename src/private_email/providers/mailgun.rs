//! Mailgun email provider — send via REST API, receive via webhook.

use async_trait::async_trait;
use crate::private_email::providers::{EmailProvider, InboundEmail, ProviderConfig, SendResult};
use crate::private_email::encryption;
use sha2::Sha256;
use hmac::{Hmac, Mac};

type HmacSha256 = Hmac<Sha256>;

pub struct MailgunProvider;

#[async_trait]
impl EmailProvider for MailgunProvider {
    fn name(&self) -> &'static str {
        "mailgun"
    }

    async fn send(
        &self,
        config: &ProviderConfig,
        from: &str,
        to: &str,
        subject: &str,
        body_html: &str,
        in_reply_to: Option<&str>,
    ) -> SendResult {
        let api_key = match &config.encrypted_api_key {
            Some(k) => match encryption::decrypt_api_key(config.tenant_id, k) {
                Ok(key) => key,
                Err(e) => return SendResult {
                    success: false,
                    provider_message_id: None,
                    error: Some(format!("Failed to decrypt API key: {}", e)),
                },
            },
            None => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some("Mailgun API key not configured".into()),
            },
        };

        let base_url = if config.region.as_deref() == Some("eu") {
            "https://api.eu.mailgun.net"
        } else {
            "https://api.mailgun.net"
        };

        let mut form: Vec<(String, String)> = vec![
            ("from".into(), from.to_string()),
            ("to".into(), to.to_string()),
            ("subject".into(), subject.to_string()),
            ("html".into(), body_html.to_string()),
        ];

        if let Some(ref reply_to) = in_reply_to {
            form.push(("h:In-Reply-To".into(), reply_to.to_string()));
        }

        let client = reqwest::Client::new();
        match client
            .post(format!("{}/v3/{}/messages", base_url, config.domain))
            .basic_auth("api", Some(&api_key))
            .form(&form)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    // Try to extract message ID from response
                    let body = resp.text().await.unwrap_or_default();
                    let msg_id = serde_json::from_str::<serde_json::Value>(&body)
                        .ok()
                        .and_then(|v| v.get("id").and_then(|id| id.as_str()).map(|s| s.to_string()));
                    SendResult {
                        success: true,
                        provider_message_id: msg_id,
                        error: None,
                    }
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    SendResult {
                        success: false,
                        provider_message_id: None,
                        error: Some(format!("Mailgun returned error: {}", body)),
                    }
                }
            }
            Err(e) => SendResult {
                success: false,
                provider_message_id: None,
                error: Some(format!("Mailgun request failed: {}", e)),
            },
        }
    }

    fn accept_inbound(&self, config: &ProviderConfig, body: &[u8]) -> Option<InboundEmail> {
        // Parse form-urlencoded Mailgun webhook
        let body_str = std::str::from_utf8(body).ok()?;
        let params: Vec<(String, String)> = url::form_urlencoded::parse(body_str.as_bytes())
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();

        let get = |key: &str| -> Option<String> {
            params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        };

        // Validate Mailgun signature if webhook key is configured
        if let Some(ref encrypted_key) = config.encrypted_webhook_key {
            if let Ok(signing_key) = encryption::decrypt_api_key(config.tenant_id, encrypted_key) {
                let token = get("token").unwrap_or_default();
                let timestamp = get("timestamp").unwrap_or_default();
                let signature = get("signature").unwrap_or_default();

                // Validate: HMAC-SHA256(signing_key, timestamp + token) == signature
                let mut mac = HmacSha256::new_from_slice(signing_key.as_bytes()).ok()?;
                mac.update(format!("{}{}", timestamp, token).as_bytes());
                let expected = hex::encode(mac.finalize().into_bytes());
                if !constant_time_eq(expected.as_bytes(), signature.as_bytes()) {
                    return None; // Signature mismatch
                }
            }
        }

        let sender = get("sender").unwrap_or_default();
        let recipient = get("recipient").unwrap_or_default();
        let subject = get("subject").unwrap_or_default();
        let body_plain = get("body-plain")
            .or_else(|| get("stripped-text"))
            .unwrap_or_default();
        let body_html = get("body-html");
        let message_id = get("Message-Id");
        let in_reply_to = get("In-Reply-To");

        let from = extract_email_address(&sender);
        let to = extract_email_address(&recipient);

        if from.is_empty() || to.is_empty() {
            return None;
        }

        Some(InboundEmail {
            from,
            to,
            subject,
            body_plain,
            body_html,
            message_id,
            in_reply_to,
            provider_message_id: get("Message-Id"),
        })
    }
}

/// Extract bare email address from "Name <email>" format.
fn extract_email_address(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_lowercase();
        }
    }
    raw.trim().to_lowercase()
}

/// Constant-time comparison for signature validation.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b.iter()).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}
