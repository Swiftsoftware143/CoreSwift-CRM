use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::providers;
use super::send_handler::load_provider_config;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct MailgunInbound {
    #[serde(default)]
    pub sender: String,
    #[serde(default)]
    pub recipient: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    #[serde(alias = "body-plain")]
    pub body_plain: String,
    #[serde(default)]
    #[serde(alias = "body-html")]
    pub body_html: String,
    #[serde(default)]
    #[serde(alias = "Message-Id")]
    pub message_id: String,
    #[serde(default)]
    #[serde(alias = "In-Reply-To")]
    pub in_reply_to: String,
    #[serde(default)]
    #[serde(alias = "stripped-text")]
    pub stripped_text: String,
    #[serde(default)]
    pub timestamp: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub signature: String,
}

/// Webhook handler for inbound Mailgun emails.
/// POST /api/v1/webhooks/mailgun/inbound
/// This endpoint is unauthenticated (Mailgun calls it).
pub async fn inbound_webhook(
    State(state): State<AppState>,
    body: String,
) -> ApiResult<Json<serde_json::Value>> {
    // ── Step 1: Extract sender + recipient (multi-provider) ─────
    // Try Mailgun form-urlencoded first, then JSON (SES SNS, Postmark)
    let (sender_email, recipient_email, mailgun_payload) = try_parse_inbound(&body);

    if sender_email.is_empty() || recipient_email.is_empty() {
        return Ok(Json(
            json!({"received": false, "error": "could not determine sender or recipient"}),
        ));
    }

    // Find which mailbox this is for (by recipient email)
    let mailbox = sqlx::query_as::<_, (Uuid, Uuid, Option<Uuid>)>(
        r#"
        SELECT id, tenant_id, user_id
        FROM private_email_boxes
        WHERE email_address = $1 AND status = 'active'
        LIMIT 1
        "#,
    )
    .bind(&recipient_email)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (mailbox_id, tenant_id, _assigned_user_id) = match mailbox {
        Some(m) => m,
        None => {
            // No matching mailbox — try catch-all domain routing
            let domain_part = recipient_email.split('@').nth(1).unwrap_or("");
            let catch_all = sqlx::query_as::<_, (Uuid,)>(
                r#"
                SELECT id FROM private_email_domains
                WHERE domain = $1 AND catch_all_enabled = true
                LIMIT 1
                "#,
            )
            .bind(domain_part)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;

            match catch_all {
                Some((_domain_id,)) => {
                    return Ok(Json(json!({
                        "received": true,
                        "routed": "catch_all",
                        "note": "No specific mailbox found, routed via catch-all"
                    })));
                }
                None => {
                    return Ok(Json(
                        json!({"received": false, "error": "no matching mailbox"}),
                    ));
                }
            }
        }
    };

    // Get domain_id for the mailbox to validate webhook signature
    let domain_row =
        sqlx::query_as::<_, (Uuid,)>("SELECT domain_id FROM private_email_boxes WHERE id = $1")
            .bind(mailbox_id)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?
            .ok_or_else(|| AppError::NotFound("Mailbox domain not found".into()))?;

    let domain_id = domain_row.0;

    // Load provider config to validate webhook signature
    let provider_config = load_provider_config(&state.db, domain_id, tenant_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {}", e)))?;

    // Validate webhook — pass raw body to provider for signature check
    let provider = providers::provider_for(&provider_config);
    let signing_key_configured = provider_config
        .encrypted_webhook_key
        .as_deref()
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let inbound = provider.accept_inbound(&provider_config, body.as_bytes());

    let inbound = match inbound {
        Some(i) => i,
        None => {
            // A signing key IS configured for this domain, so `None` (the provider's "cannot
            // validate" answer) means the payload did not authenticate — a missing/mismatched
            // signature, or a key this deployment cannot open. This endpoint is unauthenticated and
            // used to PROCESS the email anyway, which made the signature check decorative and let
            // anyone POST an inbound mail that creates contacts/tickets (t_45772522). No live domain
            // has a signing key configured (0 rows), so nothing that used to be accepted is refused
            // by this branch — it is armed for the first tenant that sets one.
            if signing_key_configured {
                tracing::warn!(
                    recipient = %recipient_email,
                    "inbound webhook rejected: the configured signing key did not validate"
                );
                return Ok(Json(json!({
                    "received": false,
                    "error": "invalid signature"
                })));
            }
            // No verification configured for this domain — reconstruct from the raw parse, as before.
            let (body_text, subject, body_html, msg_id, in_reply_to) =
                if let Some(ref p) = mailgun_payload {
                    let text = if !p.stripped_text.is_empty() {
                        &p.stripped_text
                    } else if !p.body_plain.is_empty() {
                        &p.body_plain
                    } else {
                        &p.body_html
                    };
                    let mid = if p.message_id.is_empty() {
                        None
                    } else {
                        Some(p.message_id.clone())
                    };
                    let irt = if p.in_reply_to.is_empty() {
                        None
                    } else {
                        Some(p.in_reply_to.clone())
                    };
                    (
                        text.to_string(),
                        p.subject.clone(),
                        if p.body_html.is_empty() {
                            None
                        } else {
                            Some(p.body_html.clone())
                        },
                        mid.clone(),
                        irt,
                    )
                } else {
                    ("".to_string(), "".to_string(), None, None, None)
                };
            super::providers::InboundEmail {
                from: sender_email.clone(),
                to: recipient_email.clone(),
                subject,
                body_plain: body_text,
                body_html,
                message_id: msg_id.clone(),
                in_reply_to,
                provider_message_id: msg_id,
            }
        }
    };

    // Find or create contact by sender email
    let contact_id = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&inbound.from)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    {
        Some((id,)) => id,
        None => {
            // Auto-create contact from inbound email.
            //
            // Two defects lived here and both made EVERY inbound email from an unknown sender a 500
            // (the ticket never got created, which is why no ticket in this database had
            // source='email'):
            //   * `contacts` has no `name` column — the row is first_name/last_name;
            //   * `ON CONFLICT (tenant_id, email)` cannot resolve against the REAL index, which is
            //     partial (`idx_contacts_tenant_email ... WHERE email IS NOT NULL`), so it needs the
            //     same predicate in the conflict target.
            // `last_name` is NOT NULL without a default, so the empty string is deliberate.
            // The id comes back via RETURNING: when two first-time sends from the same sender
            // overlap, the loser's ON CONFLICT fires and its row is never inserted, so handing the
            // minted uuid to the caller wrote an entity_id that exists in no contact row into
            // `events` (proven live: 10 of 36 concurrent sends).
            let local = inbound.from.split('@').next().unwrap_or(&inbound.from);
            let mut parts = local.split_whitespace();
            let first_name = parts.next().unwrap_or(local).to_string();
            let last_name = parts.collect::<Vec<_>>().join(" ");
            let new_id = Uuid::new_v4();
            sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO contacts (id, tenant_id, email, first_name, last_name, source, created_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, 'inbound_email', NOW(), NOW())
                ON CONFLICT (tenant_id, email) WHERE email IS NOT NULL
                DO UPDATE SET updated_at = NOW()
                RETURNING id
                "#,
            )
            .bind(new_id)
            .bind(tenant_id)
            .bind(&inbound.from)
            .bind(&first_name)
            .bind(&last_name)
            .fetch_one(&state.db)
            .await
            .map_err(AppError::Database)?
        }
    };

    // ── Support Ticket Routing ─────────────────────────────────
    // If the receiving mailbox is the tenant's designated support box,
    // auto-create a ticket from this email.
    let support_box_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT (settings->>'support_email_box_id')::uuid FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    if let Some(support_id) = support_box_id {
        if support_id == mailbox_id {
            let contact_name = inbound.from.split('@').next().unwrap_or(&inbound.from);
            let _ = sqlx::query_scalar::<_, Uuid>(
                r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
                   VALUES ($1, $2, $3, 'open', 'medium', 'email', $4, $5)
                   RETURNING id"#
            )
            .bind(tenant_id)
            .bind(&inbound.subject)
            .bind(&inbound.body_plain)
            .bind(&inbound.from)
            .bind(contact_name)
            .fetch_one(&state.db)
            .await;
        }
    }

    // Create event for inbound email.
    //
    // `events.title` is NOT NULL and the insert below used to omit it, so the whole webhook
    // answered 500 *after* the ticket had already been created — the caller (Mailgun / any
    // provider) sees a failure and RETRIES, which is how one email becomes several tickets. The
    // event is a side effect of a request that has already succeeded, so a failure here is
    // recorded and not escalated: the ticket is the contract, the event is bookkeeping.
    let event_payload = serde_json::json!({
        "from": inbound.from,
        "to": inbound.to,
        "subject": inbound.subject,
        "body_preview": &inbound.body_plain[..inbound.body_plain.len().min(500)],
        "message_id": inbound.message_id,
        "in_reply_to": inbound.in_reply_to,
        "provider_message_id": inbound.provider_message_id,
        "provider": provider.name(),
    });
    let event_title = if inbound.subject.trim().is_empty() {
        "(no subject)".to_string()
    } else {
        inbound.subject.clone()
    };
    if let Err(e) = sqlx::query(
        r#"
        INSERT INTO events (id, tenant_id, title, source, event_type, entity_type, entity_id, payload, created_at)
        VALUES (gen_random_uuid(), $1, $2, 'private_email', 'email_received', 'contact', $3, $4, NOW())
        "#,
    )
    .bind(tenant_id)
    .bind(&event_title)
    .bind(contact_id)
    .bind(&event_payload)
    .execute(&state.db)
    .await
    {
        tracing::warn!(error = %e, "inbound email processed, but recording the event failed");
    }

    // Fire auto-reply rules
    super::auto_reply_handler::maybe_fire_auto_reply(
        &state.db,
        tenant_id,
        "always",
        "",
        &inbound.from,
    )
    .await;

    Ok(Json(json!({
        "received": true,
        "provider": provider.name(),
        "from": inbound.from,
        "to": inbound.to,
        "subject": inbound.subject,
    })))
}

fn extract_email(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw.find('>') {
            return raw[start + 1..end].trim().to_lowercase();
        }
    }
    raw.trim().to_lowercase()
}

/// Attempt to parse the inbound webhook body from multiple provider formats.
/// Returns (sender, recipient, optional_mailgun_payload).
fn try_parse_inbound(body: &str) -> (String, String, Option<MailgunInbound>) {
    // Attempt 1: Mailgun form-urlencoded
    if let Ok(payload) = serde_urlencoded::from_str::<MailgunInbound>(body) {
        let sender = extract_email(&payload.sender);
        let recipient = extract_email(&payload.recipient);
        if !sender.is_empty() && !recipient.is_empty() {
            return (sender, recipient, Some(payload));
        }
    }
    // Attempt 2: JSON body (SES SNS, Postmark, custom providers)
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        let recipient = json
            .get("recipient")
            .or_else(|| json.get("to"))
            .or_else(|| json.get("mail").and_then(|m| m.get("recipient")))
            .and_then(|v| v.as_str())
            .map(extract_email)
            .unwrap_or_default();
        let sender = json
            .get("sender")
            .or_else(|| json.get("from"))
            .or_else(|| json.get("mail").and_then(|m| m.get("sender")))
            .and_then(|v| v.as_str())
            .map(extract_email)
            .unwrap_or_default();
        if !sender.is_empty() && !recipient.is_empty() {
            return (sender, recipient, None);
        }
    }
    (String::new(), String::new(), None)
}
