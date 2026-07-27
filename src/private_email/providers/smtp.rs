//! SMTP email provider — send via standard SMTP with TLS.
//! No inbound webhook support (SMTP is send-only via this provider).
//! Inbound for SMTP domains would use IMAP polling (not yet implemented).

use async_trait::async_trait;
use lettre::{
    transport::smtp::authentication::Credentials,
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
};
use crate::private_email::providers::{EmailProvider, InboundEmail, ProviderConfig, SendResult};
use crate::private_email::encryption;

pub struct SmtpProvider;

#[async_trait]
impl EmailProvider for SmtpProvider {
    fn name(&self) -> &'static str {
        "smtp"
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
        let host = match &config.smtp_host {
            Some(h) => h.clone(),
            None => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some("SMTP host not configured".into()),
            },
        };

        let port = config.smtp_port.unwrap_or(587) as u16;
        let username = match &config.smtp_username {
            Some(u) => u.clone(),
            None => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some("SMTP username not configured".into()),
            },
        };

        let password = match &config.encrypted_smtp_password {
            Some(p) => match encryption::decrypt_api_key(config.tenant_id, p) {
                Ok(pw) => pw,
                Err(e) => return SendResult {
                    success: false,
                    provider_message_id: None,
                    error: Some(format!("Failed to decrypt SMTP password: {}", e)),
                },
            },
            None => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some("SMTP password not configured".into()),
            },
        };

        // Build the email
        let mut msg_builder = Message::builder()
            .from(from.parse().unwrap_or_else(|_| format!("{} <{}>", from.split('@').next().unwrap_or("user"), from).parse().unwrap()))
            .to(to.parse().unwrap_or_else(|_| format!("{} <{}>", to.split('@').next().unwrap_or("user"), to).parse().unwrap()))
            .subject(subject.to_string());

        if let Some(ref reply_to) = in_reply_to {
            msg_builder = msg_builder.in_reply_to(reply_to.to_string());
        }

        let msg = match msg_builder
            .header(lettre::message::header::ContentType::TEXT_HTML)
            .body(body_html.to_string())
        {
            Ok(m) => m,
            Err(e) => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some(format!("Failed to build email: {}", e)),
            },
        };

        // Build transport
        let creds = Credentials::new(username, password);
        let transport = if config.smtp_tls {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
        };

        let transport = match transport {
            Ok(t) => t.port(port).credentials(creds).build(),
            Err(e) => return SendResult {
                success: false,
                provider_message_id: None,
                error: Some(format!("Failed to build SMTP transport: {}", e)),
            },
        };

        match transport.send(msg).await {
            Ok(response) => {
                let msg_id = response.message().collect::<Vec<_>>().join(" ");
                SendResult {
                    success: true,
                    provider_message_id: if msg_id.is_empty() { None } else { Some(msg_id) },
                    error: None,
                }
            }
            Err(e) => SendResult {
                success: false,
                provider_message_id: None,
                error: Some(format!("SMTP send failed: {}", e)),
            },
        }
    }

    fn accept_inbound(&self, _config: &ProviderConfig, _body: &[u8]) -> Option<InboundEmail> {
        // SMTP provider doesn't support webhook inbound.
        // Inbound would come via IMAP polling (future feature).
        None
    }
}
