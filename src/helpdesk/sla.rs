//! The response clocks, and the tick that makes them speak.
//!
//! A deadline nobody is told about is a deadline nobody meets. Migration 163 put the two promises on
//! every ticket; this runs on a fixed tick and records two facts per promise — that it is about to be
//! missed, and that it has been missed — on the ticket's own timeline, as an internal event, so the agent
//! working the ticket sees it where they are already looking.
//!
//! Two rules govern the queries below:
//!
//!   1. An event fires ONCE per (ticket, clock, kind). The ledger row is claimed with `ON CONFLICT DO
//!      NOTHING`, so a restart, a second instance or two ticks overlapping cannot repeat it.
//!   2. Only work that is still owed fires. A ticket that has been answered cannot breach its
//!      first-response promise afterwards, and a solved one cannot breach its resolution promise.
//!
//! Pause-while-pending and business hours are NOT applied: the clock is wall-clock, as stamped. Both are
//! real requirements and both belong with the calendar the workspace actually keeps, not invented here.

use sqlx::PgPool;
use std::time::Duration;

/// How long before a deadline the warning fires. An hour is enough notice to answer something today.
const APPROACHING_WINDOW: &str = "60 minutes";

/// How often the tick runs. Overridable for a proof that cannot wait five minutes.
pub fn tick_interval() -> Duration {
    let secs = std::env::var("HELPDESK_SLA_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(60);
    Duration::from_secs(secs)
}

/// What one pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SlaPass {
    /// Events fired this pass: warnings and breaches together.
    pub fired: u64,
    /// People actually told this pass: one per escalation rung that reached a mailbox.
    pub escalated: u64,
}

/// Walk the clocks once and record what changed.
///
/// Everything happens in ONE statement: the ledger row and the timeline entry are written together, so
/// there is no state in which a breach is recorded but not shown, or shown but not recorded.
pub async fn run_once(pool: &PgPool) -> SlaPass {
    let fired = sqlx::query_scalar::<_, i64>(
        r#"
        WITH due AS (
            -- the promise to ANSWER
            SELECT t.id AS ticket_id,
                   t.tenant_id,
                   'first_response'::text AS clock,
                   t.first_response_due AS due_at,
                   t.number
              FROM tickets t
             WHERE t.first_responded_at IS NULL
               AND t.first_response_due IS NOT NULL
               AND t.status IN ('new', 'open', 'pending', 'on_hold')
               AND t.first_response_due <= NOW() + $1::interval
            UNION ALL
            -- the promise to RESOLVE
            SELECT t.id,
                   t.tenant_id,
                   'resolution'::text,
                   t.resolution_due,
                   t.number
              FROM tickets t
             WHERE t.solved_at IS NULL
               AND t.resolution_due IS NOT NULL
               AND t.status IN ('new', 'open', 'pending', 'on_hold')
               AND t.resolution_due <= NOW() + $1::interval
        ),
        marked AS (
            INSERT INTO helpdesk_sla_events (tenant_id, ticket_id, clock, kind, due_at)
            SELECT tenant_id,
                   ticket_id,
                   clock,
                   CASE WHEN due_at < NOW() THEN 'breached' ELSE 'approaching' END,
                   due_at
              FROM due
            ON CONFLICT (ticket_id, clock, kind) DO NOTHING
            RETURNING ticket_id, clock, kind, due_at
        )
        INSERT INTO ticket_messages (ticket_id, sender_type, kind, message)
        SELECT m.ticket_id,
               'system',
               'event',
               CASE
                 WHEN m.kind = 'breached' AND m.clock = 'first_response' THEN
                   'SLA breached: the first response was due ' || to_char(m.due_at, 'YYYY-MM-DD HH24:MI') ||
                   ' and nobody has answered yet.'
                 WHEN m.kind = 'breached' THEN
                   'SLA breached: this was due to be resolved by ' || to_char(m.due_at, 'YYYY-MM-DD HH24:MI') ||
                   ' and it is still open.'
                 WHEN m.clock = 'first_response' THEN
                   'SLA due soon: the first response is due ' || to_char(m.due_at, 'YYYY-MM-DD HH24:MI') || '.'
                 ELSE
                   'SLA due soon: this is due to be resolved by ' || to_char(m.due_at, 'YYYY-MM-DD HH24:MI') || '.'
               END
          FROM marked m
        -- `RETURNING 1` types as INT4, and this is fetched as i64 (INT8), so sqlx refused to decode
        -- the row it had just written: the clocks landed, the pass still logged a failure and
        -- reported zero events on every tick. The cast keeps the shield/count shape and makes the
        -- column the type the decoder asks for.
        RETURNING 1::bigint
        "#,
    )
    .bind(APPROACHING_WINDOW)
    .fetch_all(pool)
    .await
    .map(|rows| rows.len() as i64)
    .unwrap_or_else(|e| {
        // A failing clock must be loud: silence here means nobody ever finds out a promise was missed.
        tracing::error!(error = %e, "the SLA pass failed; no clocks were recorded this tick");
        0
    });

    if fired > 0 {
        tracing::info!(events = fired, "SLA clocks updated");
    }

    // THE LADDER. The timeline events above tell whoever is already looking at the ticket; this tells a
    // person who is not. A failing ladder must not hide the clocks that already landed, so it is reported
    // on its own and the pass still returns.
    let escalated = escalate(pool).await;

    SlaPass {
        fired: fired.max(0) as u64,
        escalated,
    }
}

/// One rung of the ladder, as claimed for delivery.
#[derive(sqlx::FromRow)]
struct Rung {
    id: uuid::Uuid,
    /// NULLABLE on `tickets` — a ticket filed from the public support form carries NO workspace
    /// (`tenant_id IS NULL`, migration 122). Decoded as `Option` because a non-Option field makes the
    /// whole-row decode fail on such a row, which took the WHOLE ladder down for every tenant, not just
    /// that ticket (`ticket_escalations.tenant_id` is NOT NULL, so the claim INSERT failed first —
    /// measured live 2026-11-26 by the deploy gate: "tickets.tenant_id is NULLABLE and holds 1/13 NULLs
    /// today"). The SQL below now also refuses a workspace-less ticket explicitly: there is no account
    /// to escalate to, so the visitor's ticket stays in the operator's queue and the ladder keeps
    /// running for the customers who do have an SLA.
    tenant_id: Option<uuid::Uuid>,
    clock: String,
    rung: String,
    audience: String,
    recipient: Option<String>,
    number: i32,
    subject: String,
    due_at: chrono::DateTime<chrono::Utc>,
}

/// Tell the right person, once per rung.
///
/// Who: a deadline that is CLOSE goes to whoever owns the ticket (falling back to the account's owner when
/// nobody owns it, because an unowned ticket is exactly the one that gets missed); a deadline that has been
/// MISSED goes to the account's owner/admin as well. What: the workspace's own wording, from the template
/// registry, so a customer-facing brand is a customer-facing brand in an escalation too.
async fn escalate(pool: &PgPool) -> u64 {
    let claimed = sqlx::query_as::<_, Rung>(
        r#"
        WITH due AS (
            SELECT t.id AS ticket_id,
                   t.tenant_id,
                   t.number,
                   t.subject,
                   t.assigned_to,
                   'first_response'::text AS clock,
                   t.first_response_due AS due_at
              FROM tickets t
             WHERE t.first_responded_at IS NULL
               AND t.first_response_due IS NOT NULL
               -- A workspace-less ticket (the public support form, migration 122) has no account to
               -- escalate to and `ticket_escalations.tenant_id` is NOT NULL, so including it failed the
               -- claim INSERT and silenced the ladder for EVERY tenant. Measured live 2026-11-26.
               AND t.tenant_id IS NOT NULL
               AND t.status IN ('new', 'open', 'pending', 'on_hold')
               AND t.first_response_due <= NOW() + $1::interval
            UNION ALL
            SELECT t.id,
                   t.tenant_id,
                   t.number,
                   t.subject,
                   t.assigned_to,
                   'resolution'::text,
                   t.resolution_due
              FROM tickets t
             WHERE t.solved_at IS NULL
               AND t.resolution_due IS NOT NULL
               AND t.tenant_id IS NOT NULL
               AND t.status IN ('new', 'open', 'pending', 'on_hold')
               AND t.resolution_due <= NOW() + $1::interval
        ),
        rungs AS (
            SELECT d.ticket_id,
                   d.tenant_id,
                   d.number,
                   d.subject,
                   d.clock,
                   d.due_at,
                   CASE WHEN d.due_at < NOW() THEN 'breached' ELSE 'due_soon' END AS rung,
                   CASE WHEN d.due_at < NOW() THEN 'owner' ELSE 'assignee' END AS audience,
                   -- who to tell: the ticket's owner, or the account's owner/admin when there is none
                   CASE
                     WHEN d.due_at >= NOW() THEN
                       COALESCE(
                         (SELECT u.email FROM users u
                           WHERE u.id = d.assigned_to AND u.email IS NOT NULL LIMIT 1),
                         (SELECT u.email FROM users u
                           WHERE u.tenant_id = d.tenant_id
                             AND u.email IS NOT NULL
                             -- role::text, because `user_role` on this app is
                             -- {agency_admin, client_admin, team_member, user, company_admin, owner, member}:
                             -- comparing the enum to a label that does not exist raises 22P02 and takes the
                             -- whole ladder down with it. Same list the tenant shell uses.
                             AND u.role::text IN ('owner', 'account_owner', 'admin', 'agency_admin',
                                                  'company_admin', 'client_admin')
                           ORDER BY u.created_at LIMIT 1)
                       )
                     ELSE
                       (SELECT u.email FROM users u
                         WHERE u.tenant_id = d.tenant_id
                           AND u.email IS NOT NULL
                           AND u.role::text IN ('owner', 'account_owner', 'admin', 'agency_admin',
                                                'company_admin', 'client_admin')
                         ORDER BY u.created_at LIMIT 1)
                   END AS recipient
              FROM due d
        ),
        claimed AS (
            INSERT INTO ticket_escalations (tenant_id, ticket_id, clock, rung, audience, delivered)
            SELECT tenant_id, ticket_id, clock, rung, audience, false
              FROM rungs
            ON CONFLICT (ticket_id, clock, rung, audience) DO NOTHING
            RETURNING id, tenant_id, ticket_id, clock, rung, audience
        )
        SELECT c.id,
               c.tenant_id,
               c.clock,
               c.rung,
               c.audience,
               r.recipient,
               r.number,
               r.subject,
               r.due_at
          FROM claimed c
          JOIN rungs r ON r.ticket_id = c.ticket_id AND r.clock = c.clock AND r.rung = c.rung
        "#,
    )
    .bind(APPROACHING_WINDOW)
    .fetch_all(pool)
    .await;

    let claimed = match claimed {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "the escalation ladder could not claim its rungs");
            return 0;
        }
    };

    let mut told = 0u64;
    for r in claimed {
        // Belt and braces beside the SQL filter: a rung with no workspace has no account mailbox and no
        // branding to render, so it is recorded as undeliverable rather than decoded into an error.
        let Some(tenant) = r.tenant_id else {
            let _ = sqlx::query(
                "UPDATE ticket_escalations SET delivered = false, detail = $2 WHERE id = $1",
            )
            .bind(r.id)
            .bind("no workspace on this ticket — nothing to escalate to")
            .execute(pool)
            .await;
            continue;
        };
        let Some(to) = r.recipient.clone() else {
            // Nobody to tell is a fact worth keeping: an account with no owner mailbox is an account where
            // every promise is silent, and that is exactly what this table should show.
            let _ = sqlx::query(
                "UPDATE ticket_escalations SET delivered = false, detail = $2 WHERE id = $1",
            )
            .bind(r.id)
            .bind("no reachable mailbox on this account")
            .execute(pool)
            .await;
            continue;
        };

        let template = if r.rung == "breached" {
            "ticket_sla_breached"
        } else {
            "ticket_due_soon"
        };
        let link = format!("https://app.coreswiftcrm.com/tickets/{}", r.id);
        let vars = serde_json::json!({
            "number": format!("CS-{}", r.number),
            "subject": r.subject,
            "clock": if r.clock == "first_response" { "first response" } else { "resolution" },
            "due_at": r.due_at.to_rfc3339(),
            "link": link,
        });

        match crate::email::send_template_email(pool, tenant, &to, template, &vars).await {
            Ok(()) => {
                told += 1;
                let _ = sqlx::query(
                    "UPDATE ticket_escalations SET delivered = true, detail = $2 WHERE id = $1",
                )
                .bind(r.id)
                .bind(format!("{} -> {}", r.audience, to))
                .execute(pool)
                .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, rung = %r.rung, "an escalation message could not be sent");
                let _ = sqlx::query(
                    "UPDATE ticket_escalations SET delivered = false, detail = $2 WHERE id = $1",
                )
                .bind(r.id)
                .bind(format!("send failed: {e}"))
                .execute(pool)
                .await;
            }
        }
    }

    if told > 0 {
        tracing::info!(told, "escalation ladder delivered");
    }
    told
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_tick_has_a_sane_default_and_honours_the_override() {
        std::env::remove_var("HELPDESK_SLA_TICK_SECS");
        assert_eq!(super::tick_interval().as_secs(), 60);
        std::env::set_var("HELPDESK_SLA_TICK_SECS", "5");
        assert_eq!(super::tick_interval().as_secs(), 5);
        std::env::set_var("HELPDESK_SLA_TICK_SECS", "0");
        assert_eq!(
            super::tick_interval().as_secs(),
            60,
            "zero falls back, it does not spin"
        );
        std::env::remove_var("HELPDESK_SLA_TICK_SECS");
    }
}
