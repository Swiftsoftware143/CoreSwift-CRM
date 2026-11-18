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
    SlaPass {
        fired: fired.max(0) as u64,
    }
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
