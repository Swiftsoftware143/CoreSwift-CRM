use axum::{
    body::Bytes,
    extract::{FromRequest, Multipart, Path, Request, State},
    http::header::CONTENT_TYPE,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::attachments;
use super::feature_gate;
use super::messages_handler::{self, InboundMessage};
use super::providers::{self, InboundEmail};
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

/// Everything one authenticated delivery needs to become a stored message.
///
/// The two receivers (Mailgun's signed form post, SendGrid's Inbound Parse multipart post) differ
/// ONLY in how a delivery is authenticated and parsed. Everything after that — resolving the
/// mailbox, storing the message, the catch-all rule, the ticket decision, the event, the storage
/// meter and the auto-reply — must be the same code for both, or one receiver slowly grows rules
/// the other does not have. This struct is that seam.
struct Delivery {
    tenant_id: Uuid,
    mailbox_id: Option<Uuid>,
    recipient: String,
    inbound: InboundEmail,
    is_spam: bool,
    has_attachments: bool,
    /// Bytes carried by the delivery's file parts. They are counted toward the account's storage
    /// figure (the meter is what pauses new attachments at 100%).
    attachment_bytes: usize,
    /// The FILE parts themselves (kanban t_f919924f), written against the message row so the
    /// reader can draw an inline logo and offer a real download.
    files: Vec<attachments::InboundFile>,
    provider: &'static str,
    from_display_name: Option<String>,
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
///
/// IT ALSO HONOURS THE RECEIVING CHOICE (kanban t_04db9228): a domain whose receiving provider is
/// SendGrid is NOT receivable through this route, even if its outbound mail goes through Mailgun.
/// Both receivers used to be implicit in `provider_type`, so a customer's two choices could not
/// differ; now each route answers only for the domains that chose it.
pub async fn inbound_webhook(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<Json<serde_json::Value>> {
    // ── Step 1: Extract sender + recipient ─────
    // The delivery arrives in ONE OF TWO SHAPES (kanban t_237e3005): a plain message as
    // `application/x-www-form-urlencoded`, and ANY message carrying an attachment — including the
    // workspace's own embedded logo — as `multipart/form-data` with binary parts. The body used to
    // be extracted as a `String`, so axum answered the multipart shape with
    // `400 Request body didn't contain valid UTF-8` BEFORE this handler ran: every attached email,
    // and every branded message this app sends to one of its own inboxes, was destroyed at the door
    // while the sender saw "Sent". Reading the request by CONTENT TYPE is what makes "incoming mail
    // is never refused" true for attached mail as well as plain mail.
    let delivery = read_delivery(request).await?;

    if delivery.sender.is_empty() || delivery.recipient.is_empty() {
        return Ok(Json(
            json!({"received": false, "error": "could not determine sender or recipient"}),
        ));
    }
    let recipient_email = delivery.recipient.clone();

    // ── Step 2: who is this mail FOR? ───────────────────────────────────────────────────────────
    let Some((tenant_id, domain_id, mailbox_id)) =
        route_recipient(&state, &recipient_email).await?
    else {
        return Ok(Json(
            json!({"received": false, "error": "no matching mailbox"}),
        ));
    };

    // Load provider config to validate webhook signature
    let provider_config = load_provider_config(&state.db, domain_id, tenant_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {}", e)))?;

    // ── Step 2b: does THIS receiver answer for this domain? (kanban t_04db9228) ────────────────
    // Receiving is its own choice. A domain that receives through SendGrid must not accept a
    // Mailgun post (it has no Mailgun signing key, and Mailgun is not its inbound path), and a
    // sending-only domain has no receiver at all. Fail closed with a sentence that names the
    // right door, instead of a generic "could not authenticate" that reads like a bug.
    match provider_config.receiver() {
        Some("mailgun") => {}
        Some("sendgrid") => {
            tracing::warn!(
                recipient = %recipient_email,
                "mailgun inbound refused: this domain receives through SendGrid"
            );
            return Err(AppError::UnauthorizedReason(
                "this domain does not receive mail through Mailgun — its receiving provider is \
                 SendGrid, so a real delivery goes to its SendGrid Inbound Parse address"
                    .into(),
            ));
        }
        _ => {
            tracing::warn!(
                recipient = %recipient_email,
                "mailgun inbound refused: this domain has no receiving provider configured"
            );
            return Err(AppError::UnauthorizedReason(
                "this domain has no inbound receiver configured, so this delivery cannot be \
                 authenticated"
                    .into(),
            ));
        }
    }

    // Validate webhook — pass raw body to provider for signature check
    let provider = providers::inbound_provider_for(&provider_config);
    let signing_key_configured = provider_config
        .encrypted_webhook_key
        .as_deref()
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let inbound = provider.accept_inbound(&provider_config, &delivery.auth_body);

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

    let is_spam = delivery
        .mailgun
        .as_ref()
        .map(|p| p.is_spam())
        .unwrap_or_else(|| {
            // Providers that do not speak Mailgun's fields still get the header check.
            let raw = String::from_utf8_lossy(&delivery.auth_body);
            raw.contains("X-Mailgun-Sflag") && raw.contains("Yes")
        });

    ingest(
        &state,
        Delivery {
            tenant_id,
            mailbox_id,
            recipient: recipient_email,
            inbound,
            is_spam,
            // The field Mailgun posts AND the parts the parser actually counted: a delivery that
            // carries a binary part is an attached delivery even when `attachment-count` is missing.
            has_attachments: delivery
                .mailgun
                .as_ref()
                .map(|p| p.has_attachments())
                .unwrap_or(false)
                || delivery.attachment_parts > 0,
            attachment_bytes: delivery.attachment_bytes,
            files: delivery.files,
            provider: "mailgun",
            from_display_name: delivery
                .mailgun
                .as_ref()
                .map(|p| p.sender.as_str())
                .and_then(display_name)
                .map(str::to_string),
        },
    )
    .await
}

/// The SendGrid Inbound Parse receiver, mounted by `main.rs` at `/api/v1/webhooks/sendgrid`.
///
/// A NAMED router rather than an inline `Router::new()` at the mount because both route censuses
/// (`scripts/route-census.py` and the `NESTS` table in `auth::route_policy`) follow
/// `.nest(prefix, module::fn())`. An inline router is invisible to them: its leaf was read as a
/// ROOT-level mount (`/inbound/:token`) and the real path never reached the allowlist check
/// (kanban t_f75da95f). Moving the identical routes into a function changes no behaviour — the
/// 25 MB `DefaultBodyLimit` still sits OUTSIDE the body-read deadline for the reason spelled out
/// at the mount in `main.rs`.
pub fn sendgrid_router(deadline: crate::body_deadline::BodyReadDeadline) -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/inbound/:token",
            axum::routing::post(sendgrid_inbound).layer(axum::middleware::from_fn_with_state(
                deadline,
                crate::body_deadline::body_read_deadline_middleware,
            )),
        )
        .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024))
}

/// Webhook handler for SendGrid Inbound Parse.
/// POST /api/v1/webhooks/sendgrid/inbound/:token
///
/// THE CREDENTIAL IS THE URL. SendGrid Inbound Parse signs nothing — there is no HMAC, no shared
/// secret and no signature header, so unlike Mailgun there is nothing to verify against a stored
/// key. Each domain therefore carries its own unguessable `inbound_token` (32 hex characters) and
/// the token in the path IS the authentication: the domain is resolved BY IT through a unique
/// index, so a post to an unknown token finds no domain and writes nothing. This is the same shape
/// the app already uses for `POST /api/webhook/{token}/{action}`, where the token identifies the
/// tenant and the handler answers 401 for an unknown one (kanban t_04db9228).
///
/// WHAT IT REFUSES, and why each refusal is a refusal rather than a 200:
///   * an unknown/empty token — there is no domain, so there is nothing this delivery can belong to;
///   * a domain whose RECEIVING provider is not SendGrid — the customer may well send through
///     SendGrid over SMTP, but receiving is a separate choice and this receiver answers only for
///     the domains that chose it;
///   * a delivery whose recipient belongs to a DIFFERENT domain — a valid token must not be a key
///     into another workspace's inbox, so the routed domain has to be the token's own domain.
pub async fn sendgrid_inbound(
    State(state): State<AppState>,
    Path(token): Path<String>,
    mut multipart: Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    let token = token.trim().to_string();
    if token.len() < 16 {
        tracing::warn!("sendgrid inbound refused: the token in the path is empty or too short");
        return Err(AppError::UnauthorizedReason(
            "inbound webhook refused: the delivery could not be authenticated".into(),
        ));
    }

    // The token resolves the domain. A unique index backs this lookup, so it is one indexed read,
    // not a scan over every tenant's rows.
    let found: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        r#"SELECT id, tenant_id, domain FROM private_email_domains
            WHERE inbound_token = $1 LIMIT 1"#,
    )
    .bind(&token)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let Some((domain_id, tenant_id, domain)) = found else {
        tracing::warn!("sendgrid inbound refused: no domain carries this token");
        return Err(AppError::UnauthorizedReason(
            "inbound webhook refused: the delivery could not be authenticated".into(),
        ));
    };

    let provider_config = load_provider_config(&state.db, domain_id, tenant_id)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to load provider config: {}", e)))?;

    if provider_config.receiver() != Some("sendgrid") {
        tracing::warn!(
            domain = %domain,
            "sendgrid inbound refused: this domain's receiving provider is not SendGrid"
        );
        return Err(AppError::UnauthorizedReason(format!(
            "{} does not receive mail through SendGrid — its receiving provider is {}",
            domain, provider_config.inbound_provider
        )));
    }

    // ── The multipart body ─────────────────────────────────────────────────────────────────────
    // Text fields are kept; a file part is COUNTED and its bytes are dropped (the inbox renders the
    // message, and buffering an attachment we never store is how a receiver runs out of memory).
    let mut fields: Vec<(String, String)> = Vec::new();
    let mut files: Vec<attachments::InboundFile> = Vec::new();
    let mut attachment_parts: u32 = 0;
    let mut attachment_bytes: usize = 0;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    domain = %domain,
                    error = %e,
                    "sendgrid inbound refused: the multipart body could not be read"
                );
                return Err(AppError::BadRequest(
                    "this receiver expects SendGrid's Inbound Parse multipart post".into(),
                ));
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        let declared_name = field.file_name().unwrap_or_default().to_string();
        let declared_type = field.content_type().map(str::to_string);
        let content_id = field.headers().get("content-id").and_then(|v| {
            v.to_str()
                .ok()
                .map(|c| c.trim().trim_matches(['<', '>']).to_string())
                .filter(|c| !c.is_empty())
        });
        if field.file_name().is_some() || is_numbered_attachment_part(&name) {
            // The bytes are MEASURED (they count toward the account's storage figure) and now KEPT
            // (kanban t_f919924f, same rule as the Mailgun receiver).
            attachment_parts += 1;
            if let Ok(data) = field.bytes().await {
                attachment_bytes += data.len();
                let label = if declared_name.is_empty() {
                    name.as_str()
                } else {
                    declared_name.as_str()
                };
                files.push(attachments::InboundFile {
                    filename: attachments::safe_filename(label, attachment_parts as usize),
                    content_type: declared_type
                        .unwrap_or_else(|| "application/octet-stream".into()),
                    is_inline: content_id.is_some(),
                    content_id,
                    bytes: data.to_vec(),
                });
            }
            continue;
        }
        match field.text().await {
            Ok(value) => {
                if !name.is_empty() {
                    fields.push((name, value));
                }
            }
            Err(e) => {
                tracing::warn!(
                    domain = %domain,
                    field = %name,
                    error = %e,
                    "sendgrid inbound: one text field could not be read; the rest of the delivery stands"
                );
            }
        }
    }

    let payload = providers::sendgrid::SendGridInbound::new(fields, attachment_parts);
    let Some(inbound) = payload.accept(&provider_config) else {
        return Ok(Json(json!({
            "received": false,
            "error": "could not determine sender or recipient",
        })));
    };

    // ── Who is this mail FOR? ──────────────────────────────────────────────────────────────────
    let Some((routed_tenant, routed_domain, mailbox_id)) =
        route_recipient(&state, &inbound.to).await?
    else {
        return Ok(Json(
            json!({"received": false, "error": "no matching mailbox"}),
        ));
    };
    // A valid token must not be a key into ANOTHER workspace's inbox: the recipient has to belong to
    // the very domain the token names.
    if routed_domain != domain_id {
        tracing::warn!(
            domain = %domain,
            recipient = %inbound.to,
            "sendgrid inbound refused: the recipient does not belong to this token's domain"
        );
        return Ok(Json(json!({
            "received": false,
            "error": "no matching mailbox",
        })));
    }

    ingest(
        &state,
        Delivery {
            tenant_id: routed_tenant,
            mailbox_id,
            recipient: inbound.to.clone(),
            from_display_name: display_name(&inbound.from).map(str::to_string),
            is_spam: payload.is_spam(),
            has_attachments: payload.has_attachments() || !files.is_empty(),
            attachment_bytes,
            files,
            inbound,
            provider: "sendgrid",
        },
    )
    .await
}

/// Which tenant/domain/mailbox does this address belong to?
///
/// A created mailbox first; then the domain's catch-all, which accepts mail for addresses nobody
/// created (the MISC bucket, B211). Both paths store the message: a catch-all arrival used to be
/// answered "routed: catch_all" and then dropped, which is the same defect wearing a friendlier
/// sentence.
async fn route_recipient(
    state: &AppState,
    recipient_email: &str,
) -> Result<Option<(Uuid, Uuid, Option<Uuid>)>, AppError> {
    let mailbox = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        r#"
        SELECT id, tenant_id, domain_id
        FROM private_email_boxes
        WHERE lower(email_address) = lower($1) AND status = 'active'
        LIMIT 1
        "#,
    )
    .bind(recipient_email)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match mailbox {
        Some((box_id, tenant, domain)) => Ok(Some((tenant, domain, Some(box_id)))),
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
                    if feature_gate::catch_all_route(true, plan_sells_catch_all)
                        != feature_gate::CatchAllRoute::Route
                    {
                        tracing::warn!(
                            domain = %domain_part,
                            tenant = %tenant,
                            "catch-all delivery refused: this workspace's current plan does not \
                             sell catch-all, so its stored switch is not in force"
                        );
                        return Ok(None);
                    }
                    Ok(Some((tenant, domain, None)))
                }
                None => Ok(None),
            }
        }
    }
}

/// A machine/system sender is not a person to contact back. These are bounce delivery-status
/// notifications (whose envelope sender is a fresh `bounces-<…>@…` address per failure — hence the
/// unbounded contact growth a naive mint produces), postmaster / mailer-daemon notices and
/// `no-reply` robots. Their message is still stored so the bounce stays visible in the inbox, but
/// no contact row is created for them (kanban t_162b4d57).
fn is_machine_sender(email: &str) -> bool {
    let (local, domain) = match email.rsplit_once('@') {
        Some((l, d)) => (l.trim().to_ascii_lowercase(), d.trim().to_ascii_lowercase()),
        None => (email.trim().to_ascii_lowercase(), String::new()),
    };
    // Brevo/Sendinblue (and other senders) deliver a bounce's DSN from an address on the
    // `…sender-sib.com` Return-Path domain; treating that whole domain as machine is what keeps a
    // single failure from minting one new contact for ever.
    if domain == "sender-sib.com" || domain.ends_with(".sender-sib.com") {
        return true;
    }
    matches!(
        local.as_str(),
        "postmaster"
            | "mailer-daemon"
            | "mailer_daemon"
            | "mailerdaemon"
            | "mailer.daemon"
            | "double-bounce"
            | "daemon"
            | "abuse"
            | "root"
            | "hostmaster"
            | "webmaster"
            | "no-reply"
            | "no_reply"
            | "noreply"
    ) || local == "bounce"
        || local == "bounces"
        || local.starts_with("bounce-")
        || local.starts_with("bounces-")
}

/// One authenticated delivery → a stored message, a contact, maybe a ticket, an event.
///
/// Shared by both receivers. The provider name is passed in, never guessed: it is written onto the
/// message row and into the event payload, so "which door did this arrive through" is answerable
/// months later.
async fn ingest(state: &AppState, d: Delivery) -> ApiResult<Json<serde_json::Value>> {
    let Delivery {
        tenant_id,
        mailbox_id,
        recipient,
        inbound,
        is_spam,
        has_attachments,
        attachment_bytes,
        mut files,
        provider,
        from_display_name,
    } = d;
    let provider_name = provider;

    // A part the body draws in place is INLINE even when the delivery published no Content-ID:
    // Mailgun's own logo travels under the body's `cid:` reference, and that reference is the only
    // reliable signal on the inbound leg (kanban t_f919924f).
    for file in files.iter_mut() {
        if !file.is_inline {
            file.is_inline =
                attachments::looks_inline(&file.filename, inbound.body_html.as_deref());
        }
    }

    // ── WHAT CAN BE KEPT? (kanban t_0b3f3165) ──────────────────────────────────────────────────
    // Inbound MAIL is never refused and nothing is deleted. A file, though, must obey the ceilings
    // and the account's own allowance: a part over 10 MB, past 20 files, past 20 MB in one message,
    // or larger than the room the account has left is NOT stored. It leaves a notice row carrying
    // the reason, and only the kept bytes are added to the message's storage figure — so the meter,
    // the banner and the 100% refusal all read what is really stored.
    let mut kept_attachment_bytes = attachment_bytes;
    let mut skipped: Vec<attachments::SkippedFile> = Vec::new();
    if !files.is_empty() {
        let usage = crate::private_email::storage::usage(&state.db, tenant_id).await;
        let plan = attachments::plan_inbound(
            std::mem::take(&mut files),
            usage.used_bytes,
            usage.quota_bytes,
        );
        files = plan.keep;
        kept_attachment_bytes = plan.kept_bytes;
        skipped = plan.skipped;
        for item in &skipped {
            tracing::info!(
                tenant = %tenant_id,
                filename = %item.file.filename,
                bytes = item.file.bytes.len(),
                reason = %item.reason,
                "an arriving file was not stored; the message says why"
            );
        }
    } else if attachment_bytes > 0 {
        // The provider declared file parts but sent no binary body this receiver can read. Counting
        // bytes we do not store would inflate the meter, so the figure is zero and the delivery is
        // logged — the honest counterpart of the old "counted then dropped" defect.
        kept_attachment_bytes = 0;
        tracing::warn!(
            tenant = %tenant_id,
            declared_bytes = attachment_bytes,
            "inbound delivery declared attachments with no binary part; nothing stored or counted"
        );
    }

    // ── Step 3: store the message ITSELF ────────────────────────────────────────────────────────
    // One row per delivery, whatever the ticket toggle says. Idempotent on the sender's
    // Message-Id, so a provider retry cannot turn one email into two.
    let (message_id, message_created) = messages_handler::store_inbound(
        &state.db,
        &InboundMessage {
            tenant_id,
            mailbox_id,
            address: &recipient,
            from_address: &inbound.from,
            from_name: from_display_name.as_deref(),
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
            provider: provider_name,
            provider_message_id: inbound
                .provider_message_id
                .as_deref()
                .filter(|m| !m.trim().is_empty()),
            is_spam,
            has_attachments,
            attachment_bytes: kept_attachment_bytes,
            files: &files,
            skipped: &skipped,
        },
    )
    .await?;

    // ── STORAGE (B206, David 2026-11-11) ────────────────────────────────────────────────────────
    // A stored message is the only thing that moves the storage meter, so this is where a rung can
    // be crossed. The evaluation CLAIMS each of 50 / 75 / 99 once per month and queues ONE reminder
    // to the account owner; it never refuses the delivery and never deletes anything. It runs after
    // the row is committed and off the request path, so a slow mail queue can never delay or fail an
    // inbound delivery — the provider sees the same 200 it always did.
    if message_created {
        let db = state.db.clone();
        tokio::spawn(async move {
            let rungs = crate::private_email::storage::evaluate_and_notify(&db, tenant_id).await;
            if !rungs.is_empty() {
                tracing::info!(tenant = %tenant_id, rungs = ?rungs, "storage reminder queued after an inbound delivery");
            }
        });
    }

    // Spam is filed, not answered: no contact, no ticket, no auto-reply. It is still a stored
    // message the user can see, restore and delete forever.
    if is_spam {
        return Ok(Json(json!({
            "received": true,
            "stored": message_created,
            "message_id": message_id,
            "folder": "spam",
            "provider": provider_name,
            "to": recipient,
        })));
    }

    // ── Step 4: find or create the contact ─────────────────────────────────────────────────────
    // A machine/system sender (a bounce DSN, postmaster, a no-reply robot) is not a person to
    // contact back, and a bounce's envelope sender is a fresh address per failure — so no contact
    // row is minted for it (kanban t_162b4d57). The message is already stored above; only the
    // contact (and the contact-keyed event below) is skipped.
    let contact_id: Option<Uuid> = if is_machine_sender(&inbound.from) {
        None
    } else {
        Some(
            match sqlx::query_as::<_, (Uuid,)>(
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
            },
        )
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

    // A provider RETRY must not mint a second ticket (kanban t_314db8c1).
    //
    // The message arm above already dedupes on the sender's Message-Id, so a retry stores nothing
    // new — but this arm used to run on `should_ticket` alone, so three correctly-signed deliveries
    // of ONE email produced 1 message row and 3 tickets in the work queue (live on 2026-11-11).
    // Mailgun retries whenever it does not see a 2xx, so that is a customer email landing in the
    // queue several times. `private_email_messages.ticket_id` is the record of "this email already
    // has a ticket": it is read and written under a row lock so two deliveries in flight at once
    // cannot both see NULL and both insert. The ticket that already exists is reused and reported.
    // Both ticketing arms are untouched: `private_email_boxes.creates_ticket` OR the legacy
    // `tenants.settings->>'support_email_box_id'` still decide `should_ticket`.
    let mut ticket_created: Option<Uuid> = None;
    if should_ticket {
        match open_ticket_for_message(&state.db, tenant_id, message_id, &inbound).await {
            Ok(id) => ticket_created = id,
            Err(e) => tracing::warn!(error = %e, "message stored, but creating its ticket failed"),
        }
    }

    // Create event for inbound email — once per EMAIL, not once per DELIVERY (kanban t_314db8c1).
    //
    // Everything in this block is a side effect of a genuinely new message: the `email_received`
    // event and the auto-reply rules. A provider retry re-delivers the same Message-Id, which
    // `store_inbound` dedupes (and the ticket arm above now reuses), so this block keys off
    // `message_created`. Without that, one customer email writes the same event several times and,
    // worse, sends the CUSTOMER an auto-reply for every retry.
    //
    // `events.title` is NOT NULL and the insert below used to omit it, so the whole webhook
    // answered 500 *after* the ticket had already been created — the caller (Mailgun / any
    // provider) sees a failure and RETRIES, which is how one email becomes several tickets. The
    // event is a side effect of a request that has already succeeded, so a failure here is
    // recorded and not escalated: the stored message is the contract, the event is bookkeeping.
    // Byte-safe preview: slicing a String at a byte index panics when the 500th byte lands inside
    // a multi-byte character (an accented name in the first paragraph is enough). `chars().take`
    // cannot.
    if message_created {
        let preview: String = inbound.body_plain.chars().take(500).collect();
        let event_payload = serde_json::json!({
            "from": inbound.from,
            "to": inbound.to,
            "subject": inbound.subject,
            "body_preview": preview,
            "message_id": inbound.message_id,
            "in_reply_to": inbound.in_reply_to,
            "provider_message_id": inbound.provider_message_id,
            "provider": provider_name,
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
    }

    Ok(Json(json!({
        "received": true,
        "stored": message_created,
        "message_id": message_id,
        "to": recipient,
        "is_ticket": ticket_created.is_some(),
        "provider": provider_name,
        "ticket_id": ticket_created,
        "from": inbound.from,
        "subject": inbound.subject,
    })))
}

/// Open ONE ticket for a stored inbound message, however many times the provider delivers it.
///
/// Returns the message's ticket id either way: the ticket that already existed on a retry, or the
/// one opened on this delivery. The read and the write happen in one transaction with the message
/// row locked (`FOR UPDATE`), so two simultaneous deliveries of the same Message-Id serialise on
/// that row: the first opens the ticket and links it, the second sees a non-NULL `ticket_id` and
/// reuses it. Checking without the lock would leave exactly the race that a provider's retry storm
/// produces — both callers read NULL, both insert, and the duplicate comes back.
///
/// A delivery with no stored row at all (`message_id` nil: no Message-Id header, nothing matched)
/// still opens its ticket — that is the pre-fix behaviour, there is simply nothing to link it to.
async fn open_ticket_for_message(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    message_id: Uuid,
    inbound: &InboundEmail,
) -> Result<Option<Uuid>, AppError> {
    let contact_name = inbound
        .from
        .split('@')
        .next()
        .unwrap_or(&inbound.from)
        .to_string();

    let mut tx = db.begin().await.map_err(AppError::Database)?;

    let stored_ticket: Option<Option<Uuid>> = if message_id == Uuid::nil() {
        None
    } else {
        sqlx::query_scalar(
            "SELECT ticket_id FROM private_email_messages
              WHERE id = $1 AND tenant_id = $2
                FOR UPDATE",
        )
        .bind(message_id)
        .bind(tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(AppError::Database)?
    };

    if let Some(Some(ticket_id)) = stored_ticket {
        // A retry of an email that already opened a ticket. Nothing to write, nothing to link.
        tx.rollback().await.map_err(AppError::Database)?;
        tracing::info!(
            tenant = %tenant_id,
            message = %message_id,
            ticket = %ticket_id,
            "provider re-delivered an email that already has a ticket; reusing it"
        );
        return Ok(Some(ticket_id));
    }

    let ticket_id: Uuid = sqlx::query_scalar(
        r#"INSERT INTO tickets (tenant_id, subject, description, status, priority, source, contact_email, contact_name)
           VALUES ($1, $2, $3, 'open', 'medium', 'email', $4, $5)
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(&inbound.subject)
    .bind(&inbound.body_plain)
    .bind(&inbound.from)
    .bind(contact_name)
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    if message_id != Uuid::nil() {
        // Same statement as `messages_handler::link_ticket`, inside this transaction so the ticket,
        // the link and the flag commit together or not at all.
        sqlx::query(
            "UPDATE private_email_messages
                SET ticket_id = $3, is_ticket = true, updated_at = NOW()
              WHERE id = $1 AND tenant_id = $2",
        )
        .bind(message_id)
        .bind(tenant_id)
        .bind(ticket_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;
    }

    tx.commit().await.map_err(AppError::Database)?;
    Ok(Some(ticket_id))
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

/// Is this multipart field name one of Mailgun's file parts (`attachment-1`, `attachment-2`, …)?
///
/// The digits are required: `attachment-count` is the TEXT field that says how many files there
/// are, and treating it as a file would add a phantom attachment to every delivery.
fn is_numbered_attachment_part(name: &str) -> bool {
    name.strip_prefix("attachment-")
        .map(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

fn extract_email(raw: &str) -> String {
    if let Some(start) = raw.find('<') {
        if let Some(end) = raw[start..].find('>') {
            return raw[start + 1..start + end].trim().to_lowercase();
        }
    }
    raw.trim().to_lowercase()
}

/// One inbound delivery, parsed from whichever shape it arrived in.
///
/// WHY THIS EXISTS (kanban t_237e3005): Mailgun posts a plain message as
/// `application/x-www-form-urlencoded`, but a message that CARRIES ANY BINARY MIME PART — an
/// invoice, a signed agreement, or this app's own embedded signature logo — is posted as
/// `multipart/form-data`. The handler used to take `body: String`, so axum refused the multipart
/// shape with `400 Request body didn't contain valid UTF-8` before any of our code ran: the mail
/// was lost at the door, the sender was told "Sent", and nothing was logged.
struct ParsedDelivery {
    sender: String,
    recipient: String,
    /// The bytes the provider's signature verifier reads. For urlencoded/JSON this is the raw body
    /// unchanged; for multipart it is a canonical urlencoded form rebuilt from the TEXT fields only.
    /// Mailgun's HMAC covers `timestamp + token`, and both are fields in every shape, so the binary
    /// parts never have to be UTF-8 for the delivery to authenticate.
    auth_body: Vec<u8>,
    /// Mailgun's own field set (spam verdict, attachment count, display name). `None` for the JSON
    /// shapes other providers use, exactly as before.
    mailgun: Option<MailgunInbound>,
    /// File parts the parser actually counted, and the bytes they carried.
    attachment_parts: u32,
    attachment_bytes: usize,
    /// The parts themselves, carried through to the message row (kanban t_f919924f).
    files: Vec<attachments::InboundFile>,
}

/// Read the request into a [`ParsedDelivery`], choosing the parser by CONTENT TYPE.
async fn read_delivery(request: Request) -> Result<ParsedDelivery, AppError> {
    let is_multipart = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.trim_start()
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        })
        .unwrap_or(false);

    if is_multipart {
        let (fields, files, attachment_bytes) = multipart_text_fields(request).await?;
        // Re-encode the TEXT fields as the urlencoded form that `accept_inbound` already speaks, so
        // signature validation and field parsing stay in ONE place (`providers::mailgun`) instead of
        // growing a second, weaker copy for the multipart shape.
        let mut serializer = url::form_urlencoded::Serializer::new(String::new());
        for (name, value) in &fields {
            serializer.append_pair(name, value);
        }
        let auth_body = serializer.finish().into_bytes();
        let text = String::from_utf8_lossy(&auth_body).to_string();
        let mailgun = serde_urlencoded::from_str::<MailgunInbound>(&text).ok();
        let (sender, recipient) = match &mailgun {
            Some(p) => (extract_email(&p.sender), extract_email(&p.recipient)),
            None => {
                let (sender, recipient, _) = try_parse_inbound(&text);
                (sender, recipient)
            }
        };
        return Ok(ParsedDelivery {
            sender,
            recipient,
            auth_body,
            mailgun,
            attachment_parts: files.len() as u32,
            attachment_bytes,
            files,
        });
    }

    // Everything else keeps the shape it has always had. Lossy UTF-8 rather than a refusal: a body
    // that will not decode cannot authenticate, and it is answered with the same `received: false`
    // as any other unreadable delivery instead of a 400 that a provider retries for hours.
    let bytes = Bytes::from_request(request, &())
        .await
        .map_err(|rejection| {
            tracing::warn!(
                status = %rejection.status(),
                "inbound webhook refused: the delivery body could not be read"
            );
            AppError::BadRequest(format!(
                "the delivery body could not be read: {}",
                rejection.body_text()
            ))
        })?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    let (sender, recipient, mailgun) = try_parse_inbound(&text);
    Ok(ParsedDelivery {
        sender,
        recipient,
        auth_body: bytes.to_vec(),
        mailgun,
        attachment_parts: 0,
        attachment_bytes: 0,
        files: Vec::new(),
    })
}

/// Walk a `multipart/form-data` delivery: keep the TEXT fields and KEEP the file parts (kanban
/// t_f919924f — they used to be counted and dropped). A field that cannot be read is logged and
/// skipped — the rest of the delivery stands, which is how the SendGrid receiver behaves too.
async fn multipart_text_fields(
    request: Request,
) -> Result<(Vec<(String, String)>, Vec<attachments::InboundFile>, usize), AppError> {
    // `()` is the extractor state: neither extractor reads it (both take their body limit from the
    // request's own `DefaultBodyLimit` extension), and naming the empty state is what lets this
    // parser be unit-tested without an `AppState`.
    let mut multipart = Multipart::from_request(request, &())
        .await
        .map_err(|rejection| {
            tracing::warn!(
                status = %rejection.status(),
                "inbound webhook refused: the multipart delivery could not be parsed"
            );
            AppError::BadRequest(format!(
                "this receiver expects Mailgun's multipart delivery: {}",
                rejection.body_text()
            ))
        })?;

    let mut fields: Vec<(String, String)> = Vec::new();
    let mut files: Vec<attachments::InboundFile> = Vec::new();
    let mut attachment_parts: u32 = 0;
    let mut attachment_bytes: usize = 0;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "inbound webhook refused: the multipart delivery could not be read"
                );
                return Err(AppError::BadRequest(format!(
                    "the multipart delivery could not be read: {}",
                    e.body_text()
                )));
            }
        };
        let name = field.name().unwrap_or_default().to_string();
        // Mailgun names file parts `attachment-1`, `attachment-2`, … and also fills them with a
        // server-side filename; either signal means "this part is a file, not a field". The DIGITS
        // matter: `attachment-count` is a TEXT field and counting it as a file would report one
        // attachment too many on every delivery (caught by the unit test below).
        let is_file = field.file_name().is_some() || is_numbered_attachment_part(&name);
        let declared_name = field.file_name().unwrap_or_default().to_string();
        let declared_type = field.content_type().map(str::to_string);
        let content_id = field.headers().get("content-id").and_then(|v| {
            v.to_str()
                .ok()
                .map(|c| c.trim().trim_matches(['<', '>']).to_string())
                .filter(|c| !c.is_empty())
        });
        match field.bytes().await {
            Ok(data) => {
                if is_file {
                    attachment_parts += 1;
                    attachment_bytes += data.len();
                    let label = if declared_name.is_empty() {
                        name.as_str()
                    } else {
                        declared_name.as_str()
                    };
                    files.push(attachments::InboundFile {
                        filename: attachments::safe_filename(label, attachment_parts as usize),
                        content_type: declared_type
                            .unwrap_or_else(|| "application/octet-stream".into()),
                        is_inline: content_id.is_some(),
                        content_id,
                        bytes: data.to_vec(),
                    });
                } else if !name.is_empty() {
                    fields.push((name, String::from_utf8_lossy(&data).into_owned()));
                }
            }
            Err(e) => {
                tracing::warn!(
                    field = %name,
                    error = %e,
                    "inbound webhook: one multipart field could not be read; the rest of the delivery stands"
                );
            }
        }
    }
    Ok((fields, files, attachment_bytes))
}

/// Attempt to parse the inbound webhook body from the JSON shapes other providers use.
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
    fn machine_senders_are_not_minted_as_contacts() {
        // A bounce DSN's envelope sender, a postmaster notice and a no-reply robot are machine
        // senders — skip the contact mint, or every bounce grows the contact list for ever.
        assert!(is_machine_sender(
            "bounces-180158782-3752652062@ha.d.sender-sib.com"
        ));
        assert!(is_machine_sender("postmaster@zaarhub.com"));
        assert!(is_machine_sender("MAILER-DAEMON@example.com"));
        assert!(is_machine_sender("no-reply@swiftsoftware.net"));
        assert!(is_machine_sender("No-Reply@x.com"));
        assert!(is_machine_sender("noreply@x.com"));
        assert!(is_machine_sender("bounce@example.com"));
        assert!(is_machine_sender("bounces@example.com"));
        assert!(is_machine_sender("double-bounce@example.com"));

        // A real person still mints a contact — a domain that merely LOOKS machine-like is not enough.
        assert!(!is_machine_sender("david@example.com"));
        assert!(!is_machine_sender("someone@swiftsoftware.net"));
        assert!(!is_machine_sender("hello@bouncycastle.com"));
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

    // ── The shape that used to be destroyed at the door (kanban t_237e3005) ─────────────────────

    fn multipart_field(out: &mut Vec<u8>, boundary: &str, name: &str, value: &str) {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }

    fn multipart_file(out: &mut Vec<u8>, boundary: &str, name: &str, filename: &str, bytes: &[u8]) {
        out.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; \
                 filename=\"{filename}\"\r\nContent-Type: image/png\r\n\r\n"
            )
            .as_bytes(),
        );
        out.extend_from_slice(bytes);
        out.extend_from_slice(b"\r\n");
    }

    /// A genuine Mailgun multipart delivery: the text fields it posts plus one BINARY part whose
    /// bytes are not valid UTF-8. Before the fix this request never reached the handler at all —
    /// axum refused the `String` extractor with 400 and the mail was lost.
    fn mailgun_multipart_body(boundary: &str, png: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        multipart_field(&mut out, boundary, "sender", "David <david@example.com>");
        multipart_field(
            &mut out,
            boundary,
            "recipient",
            "Box <box@mail.example.com>",
        );
        multipart_field(&mut out, boundary, "subject", "Invoice attached");
        multipart_field(&mut out, boundary, "body-plain", "hello with a logo");
        multipart_field(&mut out, boundary, "body-html", "<p>hello with a logo</p>");
        multipart_field(
            &mut out,
            boundary,
            "message-headers",
            r#"[["Message-Id","<m1@x>"]]"#,
        );
        multipart_field(&mut out, boundary, "attachment-count", "1");
        multipart_field(&mut out, boundary, "timestamp", "1700000000");
        multipart_field(&mut out, boundary, "token", "abc123");
        multipart_field(&mut out, boundary, "signature", "deadbeef");
        multipart_file(&mut out, boundary, "attachment-1", "logo.png", png);
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        out
    }

    fn delivery_request(content_type: &str, body: Vec<u8>) -> Request {
        Request::builder()
            .method("POST")
            .header(CONTENT_TYPE, content_type)
            .body(axum::body::Body::from(body))
            .expect("a hand-built request")
    }

    #[tokio::test]
    async fn a_multipart_delivery_with_a_binary_part_parses_instead_of_being_refused() {
        let boundary = "cs-probe-boundary";
        let png: Vec<u8> = vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0xff, 0xfe, 0x00, 0x7f,
        ];
        let request = delivery_request(
            &format!("multipart/form-data; boundary={boundary}"),
            mailgun_multipart_body(boundary, &png),
        );

        let parsed = read_delivery(request)
            .await
            .expect("a multipart delivery must be parsed, not refused");

        assert_eq!(parsed.sender, "david@example.com");
        assert_eq!(parsed.recipient, "box@mail.example.com");
        assert_eq!(parsed.attachment_parts, 1);
        assert_eq!(parsed.attachment_bytes, png.len());
        let payload = parsed.mailgun.as_ref().expect("Mailgun's own fields");
        // The signature covers timestamp+token, so both must survive the re-encoding untouched.
        assert_eq!(payload.timestamp, "1700000000");
        assert_eq!(payload.token, "abc123");
        assert_eq!(payload.signature, "deadbeef");
        assert_eq!(payload.body_plain, "hello with a logo");
        assert_eq!(payload.subject, "Invoice attached");
        assert!(payload.has_attachments());
    }

    #[tokio::test]
    async fn a_multipart_delivery_with_no_file_still_parses_its_fields() {
        let boundary = "cs-plain-boundary";
        let mut body = Vec::new();
        multipart_field(&mut body, boundary, "sender", "david@example.com");
        multipart_field(&mut body, boundary, "recipient", "box@mail.example.com");
        multipart_field(&mut body, boundary, "body-plain", "no attachment here");
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());

        let parsed = read_delivery(delivery_request(
            &format!("multipart/form-data; boundary={boundary}"),
            body,
        ))
        .await
        .expect("multipart without a file must parse");
        assert_eq!(parsed.attachment_parts, 0);
        assert_eq!(parsed.attachment_bytes, 0);
        assert_eq!(
            parsed.mailgun.as_ref().map(|p| p.body_plain.as_str()),
            Some("no attachment here")
        );
    }

    #[tokio::test]
    async fn a_urlencoded_delivery_is_handed_to_the_verifier_byte_for_byte() {
        // The plain shape must be UNCHANGED: the provider's verifier reads the caller's own bytes,
        // not a re-encoding of them.
        let body = "sender=david%40example.com&recipient=box%40mail.example.com&subject=Hi\
                    &body-plain=hello+there&timestamp=1&token=t&signature=s";
        let parsed = read_delivery(delivery_request(
            "application/x-www-form-urlencoded",
            body.as_bytes().to_vec(),
        ))
        .await
        .expect("the form shape must keep working");

        assert_eq!(parsed.sender, "david@example.com");
        assert_eq!(parsed.recipient, "box@mail.example.com");
        assert_eq!(parsed.auth_body, body.as_bytes());
        assert_eq!(parsed.attachment_parts, 0);
        assert_eq!(parsed.attachment_bytes, 0);
        assert_eq!(
            parsed.mailgun.as_ref().map(|p| p.body_plain.as_str()),
            Some("hello there")
        );
    }
}
