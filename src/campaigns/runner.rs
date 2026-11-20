//! The campaign runner — what actually SENDS a campaign step.
//!
//! Until this existed, campaigns were configuration with no engine: rows in `email_campaigns`, steps,
//! triggers and enrolments, and nothing anywhere that processed a due enrolment. Two consequences, and
//! the second is the reason this file matters: nothing ever sent a campaign, and the per-campaign
//! opt-out that the unsubscribe page records could never be acted on, because no code sent a campaign
//! message to act on.
//!
//! THE RULES, all of which are read from the same places the rest of the app reads them:
//!
//!  1. **A due enrolment sends its next step and advances.** `current_step` counts the steps already
//!     sent, so the step to send is `step_order = current_step + 1`. No such step means the campaign
//!     is finished, and that is what the row becomes.
//!  2. **A campaign opt-out is honoured before anything else.** If the address asked to stop THIS
//!     campaign, the enrolment becomes `unsubscribed`, the row is stamped, and no message is built at
//!     all. This is checked here rather than in the transport because it is a fact about the campaign,
//!     not about mail in general.
//!  3. **The account-wide gate still applies on top.** Class suppression, the daily/hourly allowance,
//!     the reputation watchdog, the footer and the one-click headers all live in `providers::deliver`,
//!     so a campaign cannot bypass a block the customer made on the unsubscribe page.
//!  4. **Campaigns leave on the account's OWN automation lane** (`mg.<domain>`), never the main domain
//!     that carries human replies, and never the platform identity. An account with no verified domain
//!     gets a recorded, permanent refusal instead of a silent fallback — the same rule the automation
//!     lane already enforces.
//!
//! Nothing here retries a failed send: an enrolment that failed stays put with the failure on the
//! outbound row, because re-sending a campaign step into a hard bounce is exactly how a domain dies.

use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::mail_compliance;

/// How many enrolments one tick will process. A cap keeps a backlog from pinning a worker thread and
/// keeps each tick's write set small; the next tick picks up where this one stopped.
const BATCH: i64 = 25;

struct DueEnrolment {
    enrolment_id: Uuid,
    campaign_id: Uuid,
    tenant_id: Uuid,
    campaign_name: String,
    entity_type: String,
    entity_id: Uuid,
    current_step: i32,
}

/// What one tick did — returned so the caller can log a real number rather than a guess.
pub struct TickReport {
    pub processed: usize,
    pub sent: usize,
    pub opted_out: usize,
    pub completed: usize,
    pub refused: usize,
}

/// Merge a campaign step's body for one recipient. Deliberately tiny: three fields, and anything else
/// is left exactly as written so a stray `{{...}}` is visible rather than silently emptied.
fn render(template: &str, first: &str, last: &str, email: &str, campaign: &str) -> String {
    template
        .replace("{{first_name}}", first)
        .replace("{{last_name}}", last)
        .replace("{{email}}", email)
        .replace("{{campaign}}", campaign)
}

/// Process every enrolment that is due now. Safe to call every minute, and safe to call twice: the
/// step is advanced in the same statement family as the send, and a second tick sees the new
/// `next_send_at`.
pub async fn run_due(db: &PgPool) -> TickReport {
    let mut report = TickReport {
        processed: 0,
        sent: 0,
        opted_out: 0,
        completed: 0,
        refused: 0,
    };

    let due = sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, String, Uuid, i32)>(
        "SELECT e.id, e.campaign_id, c.tenant_id, c.name, e.entity_type, e.entity_id, e.current_step
           FROM email_campaign_enrollments e
           JOIN email_campaigns c ON c.id = e.campaign_id
          WHERE e.status = 'active'
            AND c.status = 'active'
            AND coalesce(e.next_send_at, now()) <= now()
          ORDER BY coalesce(e.next_send_at, now())
          LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(db)
    .await
    .unwrap_or_default()
    .into_iter()
    .map(|(enrolment_id, campaign_id, tenant_id, campaign_name, entity_type, entity_id, current_step)| {
        DueEnrolment {
            enrolment_id,
            campaign_id,
            tenant_id,
            campaign_name,
            entity_type,
            entity_id,
            current_step,
        }
    })
    .collect::<Vec<_>>();

    for d in due {
        report.processed += 1;

        // ── who is this? A campaign addresses a CONTACT, and only a contact. Anything else is left
        // alone rather than guessed at: an enrolment naming a company or a deal has no mailbox.
        if d.entity_type != "contact" {
            continue;
        }
        let contact = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
            "SELECT first_name, last_name, email FROM contacts WHERE id = $1 AND tenant_id = $2",
        )
        .bind(d.entity_id)
        .bind(d.tenant_id)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
        let Some((first, last, email)) = contact else {
            // The contact was deleted: nothing to send, and the enrolment should not sit forever.
            let _ = sqlx::query(
                "UPDATE email_campaign_enrollments
                    SET status = 'completed', completed_at = now()
                  WHERE id = $1",
            )
            .bind(d.enrolment_id)
            .execute(db)
            .await;
            report.completed += 1;
            continue;
        };
        let Some(email) = email.filter(|e| e.contains('@')) else {
            continue;
        };

        // ── RULE 2: did this address ask to stop THIS campaign? Nothing is built if so.
        if mail_compliance::campaign_opted_out(db, d.tenant_id, &email, d.campaign_id).await {
            let _ = sqlx::query(
                "UPDATE email_campaign_enrollments
                    SET status = 'unsubscribed', completed_at = now()
                  WHERE id = $1",
            )
            .bind(d.enrolment_id)
            .execute(db)
            .await;
            report.opted_out += 1;
            tracing::info!(
                campaign = %d.campaign_name,
                "campaign step skipped: the recipient opted out of this campaign"
            );
            continue;
        }

        // ── which step is due?
        let step = sqlx::query_as::<_, (i32, Option<String>, Option<String>, Option<i32>)>(
            "SELECT step_order, subject, body, delay_days
               FROM email_campaign_steps
              WHERE campaign_id = $1 AND step_order = $2",
        )
        .bind(d.campaign_id)
        .bind(d.current_step + 1)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();

        let Some((step_order, subject, body, delay_days)) = step else {
            let _ = sqlx::query(
                "UPDATE email_campaign_enrollments
                    SET status = 'completed', completed_at = now()
                  WHERE id = $1",
            )
            .bind(d.enrolment_id)
            .execute(db)
            .await;
            report.completed += 1;
            continue;
        };

        let first = first.unwrap_or_default();
        let last = last.unwrap_or_default();
        let subject_text = render(
            &subject.unwrap_or_else(|| d.campaign_name.clone()),
            &first,
            &last,
            &email,
            &d.campaign_name,
        );
        let body_text = render(
            &body.unwrap_or_default(),
            &first,
            &last,
            &email,
            &d.campaign_name,
        );

        // ── RULE 4: the account's own automation lane, or a recorded refusal.
        let automation_domain =
            crate::communications::providers::account_main_domain(db, d.tenant_id)
                .await
                .map(|base| crate::communications::providers::automation_subdomain(&base));

        let msg_id = Uuid::new_v4();
        let _ = sqlx::query(
            "INSERT INTO outbound_messages
                (id, tenant_id, contact_id, channel, to_address, subject, body, status,
                 automation_domain, template_data)
             VALUES ($1, $2, $3, 'email', $4, $5, $6, 'queued', $7, $8)",
        )
        .bind(msg_id)
        .bind(d.tenant_id)
        .bind(d.entity_id)
        .bind(&email)
        .bind(&subject_text)
        .bind(&body_text)
        .bind(automation_domain.as_deref())
        .bind(serde_json::json!({
            "campaign_id": d.campaign_id,
            "campaign_name": d.campaign_name,
            "step_order": step_order,
        }) as Value)
        .execute(db)
        .await;

        let outcome = match automation_domain.as_deref() {
            Some(dom) => {
                let cfg = crate::communications::providers::load_automation_delivery_config(
                    db,
                    msg_id,
                    d.tenant_id,
                    dom,
                    &email,
                    Some(subject_text.clone()),
                    &body_text,
                )
                .await;
                crate::communications::providers::deliver(
                    db,
                    &cfg,
                    mail_compliance::CLASS_BULK,
                )
                .await
            }
            None => crate::communications::providers::DeliveryOutcome::permanent(
                "this account has no verified sending domain, so its automation lane (mg.<domain>) \
                 has no identity to send a campaign from",
            ),
        };
        let _ = crate::communications::providers::record_attempt(db, msg_id, &outcome).await;
        if outcome.ok {
            report.sent += 1;
        } else {
            report.refused += 1;
            tracing::warn!(
                campaign = %d.campaign_name,
                msg = %msg_id,
                error = outcome.error.as_deref().unwrap_or("no reason given"),
                "campaign step did not send"
            );
        }

        // ── advance. A failed step still advances: retrying a campaign into a dead address is how a
        // domain loses its reputation, and the failure is already recorded on the outbound row.
        let next_delay = sqlx::query_scalar::<_, Option<i32>>(
            "SELECT delay_days FROM email_campaign_steps
              WHERE campaign_id = $1 AND step_order = $2",
        )
        .bind(d.campaign_id)
        .bind(step_order + 1)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .flatten();

        match next_delay {
            Some(days) => {
                let _ = sqlx::query(
                    "UPDATE email_campaign_enrollments
                        SET current_step = $2, next_send_at = $3
                      WHERE id = $1",
                )
                .bind(d.enrolment_id)
                .bind(step_order)
                .bind(Utc::now() + Duration::days(days.max(0) as i64))
                .execute(db)
                .await;
            }
            None => {
                let _ = sqlx::query(
                    "UPDATE email_campaign_enrollments
                        SET current_step = $2, status = 'completed', completed_at = now()
                      WHERE id = $1",
                )
                .bind(d.enrolment_id)
                .bind(step_order)
                .execute(db)
                .await;
                report.completed += 1;
            }
        }
        let _ = delay_days; // the CURRENT step's delay is history; the next step's delay schedules it
    }

    report
}
