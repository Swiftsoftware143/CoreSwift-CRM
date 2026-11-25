//! One conversation, one ticket.
//!
//! The inbox used to open a ticket for every stored inbound message. That is why this database held
//! seventeen tickets with no messages on them: a DMARC report is mail, so it became a ticket, and a
//! customer who replied three times produced three tickets a person then had to read and merge by hand.
//!
//! This module is the single door inbound mail goes through. It answers one question — is this a new
//! conversation or the next message on an existing one — and it answers it in a fixed order:
//!
//!   1. the mail headers, against the Message-Ids already stored on ticket messages
//!   2. the ticket token in the subject, which is whatever the customer did not delete
//!   3. nothing matched, so it is new
//!
//! Everything runs inside the caller's transaction, so a ticket, its first message and the link back to
//! the stored mail commit together or not at all.

use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;

use crate::errors::AppError;
use crate::helpdesk::junk;
use crate::private_email::providers::InboundEmail;

/// How long a solved ticket can still absorb a reply before the reply becomes its own ticket.
///
/// Inside the window the ticket is reopened, because a customer answering a fortnight-old thread means
/// "this is not finished". Outside it, the old thread is history and a fresh ticket is the honest
/// record — the alternative is a ticket that reopens for years and never appears in reporting again.
const REOPEN_WINDOW_DAYS: i64 = 7;

/// The token a customer sees in the subject, e.g. `[CS-1042]`.
///
/// Derived from the number rather than stored, so the two can never disagree. The outbound mail path
/// will stamp it on every reply; until that lands it is exercised by the tests only.
#[allow(dead_code)]
pub fn subject_token(number: i32) -> String {
    format!("[CS-{number}]")
}

/// Pull the number out of a subject line, if the customer left the token in.
///
/// Accepts `[CS-1042]` and the looser `[#1042]`, because a customer forwarding a thread may retype it.
pub fn number_from_subject(subject: &str) -> Option<i32> {
    let bytes = subject.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end) = subject[i..].find(']') {
                let inner = subject[i + 1..i + end].trim();
                let digits = inner
                    .strip_prefix("CS-")
                    .or_else(|| inner.strip_prefix("cs-"))
                    .or_else(|| inner.strip_prefix('#'))
                    .unwrap_or(inner);
                if let Ok(n) = digits.parse::<i32>() {
                    return Some(n);
                }
                i += end + 1;
                continue;
            }
        }
        i += 1;
    }
    None
}

/// What happened to an inbound message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngestOutcome {
    /// A new ticket was opened for this conversation.
    Created { ticket_id: Uuid, number: i32 },
    /// The message joined an existing ticket (and reopened it if it had been solved).
    Appended { ticket_id: Uuid, number: i32 },
    /// A machine wrote it. It is stored as mail, but it is not work for a person.
    IgnoredNoise,
}

impl IngestOutcome {
    pub fn ticket_id(&self) -> Option<Uuid> {
        match self {
            IngestOutcome::Created { ticket_id, .. }
            | IngestOutcome::Appended { ticket_id, .. } => Some(*ticket_id),
            IngestOutcome::IgnoredNoise => None,
        }
    }
}

/// File one inbound message: onto its ticket if this is a reply, as a new ticket if it is a question.
pub async fn ingest_inbound(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    inbound: &InboundEmail,
    // The row the mail was stored in, when the caller has one. Recorded on the ticket message so the files the
    // customer sent can be found from the ticket, instead of only from the inbox.
    stored_message: Option<Uuid>,
) -> Result<IngestOutcome, AppError> {
    let own = own_addresses(tx, tenant_id).await?;

    if junk::is_noise(
        &inbound.subject,
        &inbound.from,
        &inbound.body_plain,
        inbound.is_auto_generated,
        &own,
    ) {
        tracing::info!(
            tenant = %tenant_id,
            from = %inbound.from,
            subject = %inbound.subject,
            "inbound mail is machine-generated; stored as mail, no ticket"
        );
        return Ok(IngestOutcome::IgnoredNoise);
    }

    let contact_name = display_name_for(&inbound.from);
    let contact_id = find_contact(tx, tenant_id, &inbound.from).await?;

    // ── 1. the headers ─────────────────────────────────────────────────────────────────────────
    let mut matched: Option<(Uuid, i32, String, Option<chrono::DateTime<chrono::Utc>>)> = None;
    if let Some(parent) = inbound
        .in_reply_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        matched = sqlx::query_as(
            r#"SELECT t.id, t.number, t.status, t.solved_at
                 FROM ticket_messages m
                 JOIN tickets t ON t.id = m.ticket_id
                WHERE m.email_message_id = $1
                  AND t.tenant_id = $2
                ORDER BY m.created_at DESC
                LIMIT 1"#,
        )
        .bind(parent)
        .bind(tenant_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(AppError::Database)?;
    }

    // ── 2. the subject token ───────────────────────────────────────────────────────────────────
    if matched.is_none() {
        if let Some(number) = number_from_subject(&inbound.subject) {
            matched = sqlx::query_as(
                r#"SELECT id, number, status, solved_at
                     FROM tickets
                    WHERE tenant_id = $1 AND number = $2"#,
            )
            .bind(tenant_id)
            .bind(number)
            .fetch_optional(&mut **tx)
            .await
            .map_err(AppError::Database)?;
        }
    }

    // ── 3. append, or open a new conversation ─────────────────────────────────────────────────
    if let Some((ticket_id, number, status, solved_at)) = matched {
        let stale = solved_at
            .map(|at| (chrono::Utc::now() - at).num_days() >= REOPEN_WINDOW_DAYS)
            .unwrap_or(false);
        let closed = status == "solved" || status == "closed";

        if !(closed && stale) {
            insert_message(
                tx,
                ticket_id,
                "contact",
                "reply",
                inbound,
                contact_id,
                stored_message,
            )
            .await?;

            if closed {
                // A reply inside the window means the conversation is not finished: reopen it and
                // clear the solve time so the resolution clock starts from the new answer.
                sqlx::query(
                    r#"UPDATE tickets
                          SET status = 'open', solved_at = NULL, updated_at = NOW()
                        WHERE id = $1"#,
                )
                .bind(ticket_id)
                .execute(&mut **tx)
                .await
                .map_err(AppError::Database)?;
                tracing::info!(ticket = %ticket_id, "customer replied to a solved ticket; reopened");
            } else {
                sqlx::query("UPDATE tickets SET status = CASE WHEN status = 'new' THEN 'open' ELSE status END, updated_at = NOW() WHERE id = $1")
                    .bind(ticket_id)
                    .execute(&mut **tx)
                    .await
                    .map_err(AppError::Database)?;
            }

            return Ok(IngestOutcome::Appended { ticket_id, number });
        }
        // Older than the window: fall through and open a fresh ticket for the new question.
    }

    let subject = if inbound.subject.trim().is_empty() {
        "(no subject)".to_string()
    } else {
        inbound.subject.trim().to_string()
    };

    // `number` is assigned by the trigger in migration 162, in this same transaction, from the
    // account's own counter — no read-then-write race, and no creation path can forget it.
    let (ticket_id, number): (Uuid, i32) = sqlx::query_as(
        r#"INSERT INTO tickets
             (tenant_id, subject, description, status, priority, source,
              contact_id, contact_email, contact_name, ai_state)
           VALUES ($1, $2, $3, 'new', 'medium', 'email', $4, $5, $6, 'none')
           RETURNING id, number"#,
    )
    .bind(tenant_id)
    .bind(&subject)
    .bind(&inbound.body_plain)
    .bind(contact_id)
    .bind(&inbound.from)
    .bind(&contact_name)
    .fetch_one(&mut **tx)
    .await
    .map_err(AppError::Database)?;

    insert_message(
        tx,
        ticket_id,
        "contact",
        "reply",
        inbound,
        contact_id,
        stored_message,
    )
    .await?;

    // How they sounded, recorded in the same transaction that filed the message.
    crate::helpdesk::sentiment::apply_tx(tx, ticket_id, &inbound.body_plain).await;

    tracing::info!(ticket = %ticket_id, number, tenant = %tenant_id, "opened a ticket from inbound mail");
    Ok(IngestOutcome::Created { ticket_id, number })
}

/// Store the mail itself on the ticket's timeline, with the ids threading will need next time.
async fn insert_message(
    tx: &mut Transaction<'_, Postgres>,
    ticket_id: Uuid,
    sender_type: &str,
    kind: &str,
    inbound: &InboundEmail,
    author_id: Option<Uuid>,
    stored_message: Option<Uuid>,
) -> Result<(), AppError> {
    let body = if inbound.body_plain.trim().is_empty() {
        inbound.body_html.clone().unwrap_or_default()
    } else {
        inbound.body_plain.clone()
    };
    let references = inbound
        .in_reply_to
        .as_ref()
        .map(|p| serde_json::json!([p]))
        .unwrap_or(serde_json::Value::Null);

    sqlx::query(
        r#"INSERT INTO ticket_messages
             (ticket_id, sender_type, kind, message, body_text, body_html,
              email_message_id, in_reply_to, email_references, author_id, email_message_row_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
    )
    .bind(ticket_id)
    .bind(sender_type)
    .bind(kind)
    .bind(&body)
    .bind(&inbound.body_plain)
    .bind(&inbound.body_html)
    .bind(&inbound.message_id)
    .bind(&inbound.in_reply_to)
    .bind(&references)
    .bind(author_id)
    .bind(stored_message)
    .execute(&mut **tx)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

/// The addresses this account sends support mail from — mail arriving from one of these is a loop.
pub(crate) async fn own_addresses(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
) -> Result<Vec<String>, AppError> {
    let mut out: Vec<String> = sqlx::query_scalar::<_, Option<String>>(
        "SELECT support_email FROM helpdesk_sites WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(AppError::Database)?
    .into_iter()
    .flatten()
    .collect();

    out.extend(
        sqlx::query_scalar::<_, String>(
            "SELECT email_fwd FROM ticket_inboxes WHERE tenant_id = $1 AND email_fwd IS NOT NULL",
        )
        .bind(tenant_id)
        .fetch_all(&mut **tx)
        .await
        .map_err(AppError::Database)?,
    );
    Ok(out)
}

/// Link the mail to a CRM contact when one already exists for that address. Never creates one: a support
/// address is not consent to be added to a marketing list, and creating contacts here would fire the
/// portfolio broadcast from a spam email.
async fn find_contact(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    email: &str,
) -> Result<Option<Uuid>, AppError> {
    if email.trim().is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND lower(email) = lower($2) ORDER BY created_at LIMIT 1",
    )
    .bind(tenant_id)
    .bind(email)
    .fetch_optional(&mut **tx)
    .await
    .map_err(AppError::Database)
}

/// `david.giraudy@acme.com` -> `David Giraudy` when the local part looks like a name.
fn display_name_for(from: &str) -> String {
    let local = from.split('@').next().unwrap_or("").trim();
    if local.is_empty() {
        return "Customer".to_string();
    }
    let cleaned = local.replace(['.', '_', '-'], " ");
    let mut out = String::with_capacity(cleaned.len());
    for word in cleaned.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.extend(first.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    if out.trim().is_empty() {
        local.to_string()
    } else {
        out
    }
}

/// Kept separate so a future non-transactional caller (a test, a repair) can await it directly.
#[allow(dead_code)]
async fn status_of(conn: &mut PgConnection, ticket_id: Uuid) -> Result<Option<String>, AppError> {
    sqlx::query_scalar::<_, String>("SELECT status FROM tickets WHERE id = $1")
        .bind(ticket_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(AppError::Database)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_round_trips() {
        assert_eq!(
            number_from_subject(&format!("Re: {} my invoice", subject_token(1042))),
            Some(1042)
        );
        assert_eq!(number_from_subject("Re: my invoice"), None);
        assert_eq!(number_from_subject("[#77] help"), Some(77));
        // a bracketed word that is not a number must not be mistaken for a ticket
        assert_eq!(number_from_subject("[BUG] it broke"), None);
    }

    #[test]
    fn names_are_readable() {
        assert_eq!(display_name_for("david.giraudy@acme.com"), "David Giraudy");
        assert_eq!(display_name_for("info@acme.com"), "Info");
        assert_eq!(display_name_for("@broken"), "Customer");
    }
}
