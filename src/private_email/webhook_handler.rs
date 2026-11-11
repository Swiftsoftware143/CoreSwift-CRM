use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::feature_gate;
use super::messages_handler::{self, InboundMessage};
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
    /// Mailgun's own spam verdict. A domain is created with `spam_action=tag`, so a message it
    /// considers spam still arrives — carrying this header instead of being dropped. Filing it in
    /// Spam instead of Inbox is the difference between a usable inbox and one nobody can read
    /// (B205), and it must never mean the message is discarded.
    #[serde(default, alias = "X-Mailgun-Sflag")]
    pub sflag: String,
    /// The full header block, when the webhook is configured to send it. Read only to recover the
    /// spam flag on a domain whose webhook does not forward the S-flag as its own field.
    #[serde(default, alias = "message-headers")]
    pub message_headers: String,
    #[serde(default, alias = "attachment-count")]
    pub attachment_count: String,
}

impl MailgunInbound {
    fn is_spam(&self) -> bool {
        if self.sflag.trim().eq_ignore_ascii_case("yes") {
            return true;
        }
        // `message-headers` is a JSON array of [name, value] pairs.
        if let Ok(headers) = serde_json::from_str::<Vec<(String, String)>>(&self.message_headers) {
            return headers.iter().any(|(k, v)| {
                k.eq_ignore_ascii_case("X-Mailgun-Sflag") && v.trim().eq_ignore_ascii_case("yes")
            });
        }
        false
    }

    fn has_attachments(&self) -> bool {
        self.attachment_count.trim().parse::<u32>().unwrap_or(0) > 0
    }
}

/// Webhook handler for inbound Mailgun emails.
/// POST /api/v1/webhooks/mailgun/inbound
///
/// Mailgun calls this with no session, so the per-domain Mailgun signing key IS the
/// credential: `providers::mailgun::accept_inbound` verifies HMAC-SHA256(signing_key,
/// timestamp + token) whenever `private_email_domains.webhook_signing_key_encrypted` is set,
/// and this handler REFUSES a delivery whose signature does not validate — including a
/// domain that has no signing key configured at all (kanban t_7f134e77). It used to process
/// that case anyway, which made the endpoint an anonymous writer of contacts/tickets/events.
///
/// WHAT IT STORES (B204/B211): every authenticated delivery now becomes a row in
/// `private_email_messages`, tagged with the address it arrived on and flagged with whether it
/// ALSO became a ticket. Before this, a message whose mailbox was not ticked for ticketing was
/// acknowledged and then invisible — accepted by the provider and stored nowhere a human could
/// see it. The ticket decision is now a routing decision layered on top of a stored message,
/// never a condition for storing it.
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

    // ── Step 2: who is this mail FOR? ───────────────────────────────────────────────────────────
    // A created mailbox first; then the domain's catch-all, which accepts mail for addresses
    // nobody created (the MISC bucket, B211). Both paths store the message: a catch-all arrival
    // used to be answered "routed: catch_all" and then dropped, which is the same defect wearing
    // a friendlier sentence.
    let mailbox = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        r#"
        SELECT id, tenant_id, domain_id
        FROM private_email_boxes
        WHERE lower(email_address) = lower($1) AND status = 'active'
        LIMIT 1
        "#,
    )
    .bind(&recipient_email)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (tenant_id, domain_id, mailbox_id) = match mailbox {
        Some((box_id, tenant, domain)) => (tenant, domain, Some(box_id)),
        None => {
            let domain_part = recipient_email
                .rsplit('@')
                .next()
                .unwrap_or("")
                .to_lowercase();
            let catch_all = sqlx::query_as::<_, (Uuid, Uuid)>(
                r#"
                SELECT id, tenant_id FROM private_email_domains
                WHERE lower(domain) = $1 AND catch_all_enabled = true
                LIMIT 1
                "#,
            )
            .bind(&domain_part)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;

            match catch_all {
                // Stored with no mailbox row, which is exactly what makes it MISC.
                Some((domain, tenant)) => {
                    // THE WITHDRAWAL ARM (kanban t_6b54961b). `catch_all_enabled` is the tenant's
                    // stored PREFERENCE, not the entitlement. Reading it here as if it were
                    // permission is why a workspace moved off a plan that sells catch-all (free,
                    // starter, or a past-due downgrade) kept accepting mail for every unmatched
                    // address, while a fresh PATCH on that same workspace was refused.
                    // The plan is asked AT THE MOMENT OF USE through the same helper the PATCH arm
                    // uses, so the two answers cannot disagree, and no plan writer (assign, update
                    // or cancel) has to remember to withdraw anything — a new writer cannot open a
                    // hole. The refusal is byte-identical to a domain with catch-all switched off,
                    // because a sender is not owed our billing state.
                    let plan_sells_catch_all =
                        feature_gate::catch_all_granted(&state.db, tenant).await?;
                    if !feature_gate::catch_all_effective(true, plan_sells_catch_all) {
                        tracing::warn!(
                            domain = %domain_part,
                            tenant = %tenant,
                            "catch-all delivery refused: this workspace's current plan does not \
                             sell catch-all, so its stored switch is not in force"
                        );
                        return Ok(Json(
                            json!({"received": false, "error": "no matching mailbox"}),
                        ));
                    }
                    (tenant, domain, None)
                }
                None => {
                    return Ok(Json(
                        json!({"received": false, "error": "no matching mailbox"}),
                    ));
                }
            }
        }
    };

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
            // `None` is the provider's "cannot validate this delivery" answer, and it has
            // exactly two causes. BOTH are refusals (kanban t_7f134e77):
            //   * a signing key IS configured for this domain and the payload did not authenticate
            //     — a missing or mismatched signature, or a stored key this deployment cannot open;
            //   * this domain has NO signing key configured at all.
            //
            // The signature is the only credential this endpoint has: Mailgun presents no session
            // and cannot carry an internal key, so a delivery that cannot be authenticated is
            // refused instead of processed. The branch that used to sit here reconstructed the
            // message from the raw parse for the second cause, which made an unarmed domain an
            // anonymous writer of contacts/tickets/events — any caller could name a mailbox on it
            // and create rows in that tenant (reproduced live 2026-10-11: 200 + one contact, one
            // ticket, one event). Fail CLOSED.
            if signing_key_configured {
                tracing::warn!(
                    recipient = %recipient_email,
                    "inbound webhook refused: the configured signing key did not validate"
                );
            } else {
                tracing::warn!(
                    recipient = %recipient_email,
                    "inbound webhook refused: no webhook signing key is configured for this domain, so the delivery cannot be authenticated"
                );
            }
            return Err(AppError::UnauthorizedReason(
                "inbound webhook refused: the delivery could not be authenticated".into(),
            ));
        }
    };

    let is_spam = mailgun_payload
        .as_ref()
        .map(|p| p.is_spam())
        .unwrap_or_else(|| {
            // Providers that do not speak Mailgun's fields still get the header check.
            body.contains("X-Mailgun-Sflag") && body.contains("Yes")
        });

    // ── Step 3: store the message ITSELF ────────────────────────────────────────────────────────
    // One row per delivery, whatever the ticket toggle says. Idempotent on the sender's
    // Message-Id, so a provider retry cannot turn one email into two.
    let stored = messages_handler::store_inbound(
        &state.db,
        &InboundMessage {
            tenant_id,
            mailbox_id,
            address: &recipient_email,
            from_address: &inbound.from,
            from_name: mailgun_payload
                .as_ref()
                .map(|p| p.sender.as_str())
                .and_then(display_name),
            to_address: &inbound.to,
            cc_address: None,
            subject: &inbound.subject,
            body_plain: &inbound.body_plain,
            body_html: inbound.body_html.as_deref(),
            message_id: inbound
                .message_id
                .as_deref()
                .filter(|m| !m.trim().is_empty()),
            in_reply_to: inbound
                .in_reply_to
                .as_deref()
                .filter(|m| !m.trim().is_empty()),
            provider: provider.name(),
            provider_message_id: inbound
                .provider_message_id
                .as_deref()
                .filter(|m| !m.trim().is_empty()),
            is_spam,
            has_attachments: mailgun_payload
                .as_ref()
                .map(|p| p.has_attachments())
                .unwrap_or(false),
        },
    )
    .await?;

    let (message_id, message_created) = stored;

    // Spam is filed, not answered: no contact, no ticket, no auto-reply. It is still a stored
    // message the user can see, restore and delete forever.
    if is_spam {
        return Ok(Json(json!({
            "received": true,
            "stored": message_created,
            "message_id": message_id,
            "folder": "spam",
            "provider": provider.name(),
            "to": recipient_email,
        })));
    }

    // ── Step 4: find or create the contact ─────────────────────────────────────────────────────
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

    // ── Step 5: Support Ticket Routing ─────────────────────────────────
    // Mail ALWAYS arrives in the inbox and is tagged with the address it came to (step 3).
    // This decides only which messages ALSO become a ticket automatically, per
    // mailbox (private_email_boxes.creates_ticket, migration 125):
    //   support@ ships ON  - it is the work queue people write to for help
    //   the rest ship OFF  - they land in the inbox and can be converted by hand
    // The user can tick or untick any mailbox. The legacy single designated
    // support box is still honoured so an upgraded account keeps working.
    // A catch-all arrival has no mailbox row, so it never becomes a ticket by itself.
    let creates_ticket: Option<bool> = match mailbox_id {
        Some(box_id) => sqlx::query_scalar(
            "SELECT creates_ticket FROM private_email_boxes WHERE id = $1 AND tenant_id = $2",
        )
        .bind(box_id)
        .bind(tenant_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten(),
        None => None,
    };

    let legacy_support_box: Option<Uuid> = sqlx::query_scalar(
        "SELECT (settings->>'support_email_box_id')::uuid FROM tenants WHERE id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .ok()
    .flatten();

    let should_ticket = creates_ticket.unwrap_or(false)
        || legacy_support_box
            .map(|id| Some(id) == mailbox_id)
            .unwrap_or(false);

    let mut ticket_created: Option<Uuid> = None;
    if should_ticket {
        let contact_name = inbound.from.split('@').next().unwrap_or(&inbound.from);
        match sqlx::query_scalar::<_, Uuid>(
            r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
               VALUES ($1, $2, $3, 'open', 'medium', 'email', $4, $5)
               RETURNING id"#,
        )
        .bind(tenant_id)
        .bind(&inbound.subject)
        .bind(&inbound.body_plain)
        .bind(&inbound.from)
        .bind(contact_name)
        .fetch_one(&state.db)
        .await
        {
            Ok(ticket_id) => {
                ticket_created = Some(ticket_id);
                if let Err(e) =
                    messages_handler::link_ticket(&state.db, tenant_id, message_id, ticket_id).await
                {
                    tracing::warn!(error = %e, "message stored, but linking its ticket failed");
                }
            }
            Err(e) => tracing::warn!(error = %e, "message stored, but creating its ticket failed"),
        }
    }

    // Create event for inbound email.
    //
    // `events.title` is NOT NULL and the insert below used to omit it, so the whole webhook
    // answered 500 *after* the ticket had already been created — the caller (Mailgun / any
    // provider) sees a failure and RETRIES, which is how one email becomes several tickets. The
    // event is a side effect of a request that has already succeeded, so a failure here is
    // recorded and not escalated: the stored message is the contract, the event is bookkeeping.
    // Byte-safe preview: slicing a String at a byte index panics when the 500th byte lands inside
    // a multi-byte character (an accented name in the first paragraph is enough). `chars().take`
    // cannot.
    let preview: String = inbound.body_plain.chars().take(500).collect();
    let event_payload = serde_json::json!({
        "from": inbound.from,
        "to": inbound.to,
        "subject": inbound.subject,
        "body_preview": preview,
        "message_id": inbound.message_id,
        "in_reply_to": inbound.in_reply_to,
        "provider_message_id": inbound.provider_message_id,
        "provider": provider.name(),
        "message_row_id": message_id,
        "ticket_id": ticket_created,
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
        "stored": message_created,
        "message_id": message_id,
        "to": recipient_email,
        "is_ticket": ticket_created.is_some(),
        "ticket_id": ticket_created,
        "provider": provider.name(),
        "from": inbound.from,
        "subject": inbound.subject,
    })))
}

/// `"David Giraudy <david@x.com>"` -> `Some("David Giraudy")`.
fn display_name(raw: &str) -> Option<&str> {
    let start = raw.find('<')?;
    let name = raw[..start].trim().trim_matches('"');
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

fn extract_email(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw[start..].find('>') {
            return raw[start + 1..start + end].trim().to_lowercase();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_email_unwraps_a_display_name() {
        assert_eq!(
            extract_email("David Giraudy <David@Example.COM>"),
            "david@example.com"
        );
        assert_eq!(extract_email("  INFO@x.com "), "info@x.com");
    }

    #[test]
    fn display_name_is_read_from_the_sender_header() {
        assert_eq!(
            display_name("David Giraudy <david@x.com>"),
            Some("David Giraudy")
        );
        assert_eq!(display_name("david@x.com"), None);
        assert_eq!(display_name("<david@x.com>"), None);
    }

    #[test]
    fn spam_is_read_from_mailguns_sflag() {
        let mut p = MailgunInbound {
            sender: String::new(),
            recipient: String::new(),
            subject: String::new(),
            body_plain: String::new(),
            body_html: String::new(),
            message_id: String::new(),
            in_reply_to: String::new(),
            stripped_text: String::new(),
            timestamp: String::new(),
            token: String::new(),
            signature: String::new(),
            sflag: "Yes".into(),
            message_headers: String::new(),
            attachment_count: String::new(),
        };
        assert!(p.is_spam());
        p.sflag = "No".into();
        assert!(!p.is_spam());
        p.message_headers = r#"[["X-Mailgun-Sflag","Yes"]]"#.into();
        assert!(p.is_spam());
    }
}
