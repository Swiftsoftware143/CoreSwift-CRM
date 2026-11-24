//! Clock-driven trigger kinds — `date.renewal` and `contact.inactive` (kanban t_990c386e).
//!
//! WHY A SCANNER AND NOT AN EVENT: the other trigger kinds react to something a writer just did
//! (a tag assigned, a deal moved, a score changed). "The renewal date is 30 days away" and "nothing
//! has happened with this contact for 30 days" are facts that become true while nobody is writing
//! anything, so something has to look. This pass runs from the existing worker (every 15 minutes)
//! and asks each active rule's own question against `contacts`.
//!
//! NOTHING HERE EXECUTES AN ACTION. Every match goes through `engine::fire_rule(..., force_schedule
//! = true)`, i.e. it lands in `delayed_actions` due at now + the rule's own delay, and the ordinary
//! 60-second pass evaluates it. Two reasons: a rule's delay/condition must apply to a clock trigger
//! exactly as to an event trigger, and the queue row doubles as the LEDGER — `source_rule_id` +
//! `entity_id` is how the next pass knows this contact has already been queued for this rule inside
//! its recheck window. Without that ledger a daily scan would queue the same 400 contacts every day.
//!
//! CONFIG (all optional, defaults in brackets):
//!   date.renewal      field ["renewal_date"] — a key in `contacts.metadata` holding a date
//!                     days_before [30]     — how far ahead of the date to fire
//!                     recheck_days [330]   — do not re-queue the same contact within this window
//!   contact.inactive  days [30]            — quiet for at least this long
//!                     recheck_days [30]    — do not re-queue the same contact within this window

use super::engine::{automations_enabled, fire_rule};
use super::models::{rule_columns, AutomationRule};
use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

/// One pass is capped so a 50,000-contact workspace cannot turn the worker into a stampede; the next
/// pass picks up where this one stopped because queued contacts leave the candidate set.
const SCAN_LIMIT: i64 = 200;

pub async fn run_scan_triggers(db: &PgPool) {
    let rules = match sqlx::query_as::<_, AutomationRule>(concat!(
        "SELECT ",
        rule_columns!(),
        " FROM automation_rules WHERE trigger_type IN ('date.renewal', 'contact.inactive') \
         AND is_active IS NOT FALSE ORDER BY tenant_id"
    ))
    .fetch_all(db)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "Scan triggers: could not load rules");
            return;
        }
    };

    for rule in rules {
        // Same entitlement gate every other trigger path asks. A plan without automations runs none.
        if !automations_enabled(db, rule.tenant_id).await {
            continue;
        }
        match evaluate_rule(db, &rule).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(rule = %rule.id, queued = n, "Scan trigger queued contacts"),
            Err(e) => tracing::warn!(rule = %rule.id, error = %e, "Scan trigger failed"),
        }
    }
}

async fn evaluate_rule(db: &PgPool, rule: &AutomationRule) -> Result<usize, AppError> {
    let cfg = &rule.trigger_config;
    let recheck_days = cfg
        .get("recheck_days")
        .and_then(|v| v.as_i64())
        .filter(|d| *d > 0);

    let contacts: Vec<Uuid> = match rule.trigger_type.as_str() {
        "date.renewal" => {
            let field = cfg
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("renewal_date");
            let days_before = cfg
                .get("days_before")
                .and_then(|v| v.as_i64())
                .unwrap_or(30)
                .clamp(0, 3650);
            let recheck = recheck_days.unwrap_or(330);
            // `CASE` (not `AND`) around the cast: a metadata value that is not a date must be
            // skipped, never raise 22007 for the whole scan.
            sqlx::query_scalar(
                r#"SELECT c.id FROM contacts c
                    WHERE c.tenant_id = $1 AND c.is_active
                      AND (CASE WHEN c.metadata->>$2 ~ '^[0-9]{4}-[0-9]{2}-[0-9]{2}$'
                                THEN (c.metadata->>$2)::date END)
                          BETWEEN current_date AND current_date + make_interval(days => $3)
                      AND NOT EXISTS (
                          SELECT 1 FROM delayed_actions da
                           WHERE da.tenant_id = $1 AND da.source_rule_id = $4
                             AND da.entity_id = c.id
                             AND da.created_at > now() - make_interval(days => $5))
                    ORDER BY c.id LIMIT $6"#,
            )
            .bind(rule.tenant_id)
            .bind(field)
            .bind(days_before as i32)
            .bind(rule.id)
            .bind(recheck as i32)
            .bind(SCAN_LIMIT)
            .fetch_all(db)
            .await?
        }
        "contact.inactive" => {
            let days = cfg
                .get("days")
                .and_then(|v| v.as_i64())
                .unwrap_or(30)
                .clamp(1, 3650);
            let recheck = recheck_days.unwrap_or(days);
            // "No activity" is `last_contacted_at`, and a contact that was NEVER contacted is quiet
            // from its own creation — the win-back case, not an exclusion.
            sqlx::query_scalar(
                r#"SELECT c.id FROM contacts c
                    WHERE c.tenant_id = $1 AND c.is_active
                      AND COALESCE(c.last_contacted_at, c.created_at)
                          < now() - make_interval(days => $2)
                      AND NOT EXISTS (
                          SELECT 1 FROM delayed_actions da
                           WHERE da.tenant_id = $1 AND da.source_rule_id = $3
                             AND da.entity_id = c.id
                             AND da.created_at > now() - make_interval(days => $4))
                    ORDER BY c.id LIMIT $5"#,
            )
            .bind(rule.tenant_id)
            .bind(days as i32)
            .bind(rule.id)
            .bind(recheck as i32)
            .bind(SCAN_LIMIT)
            .fetch_all(db)
            .await?
        }
        other => {
            // Not a scan trigger: the caller filtered, so this is a bug, not a data condition.
            tracing::warn!(trigger = other, "run_scan_triggers saw a non-scan trigger");
            Vec::new()
        }
    };

    let mut queued = 0usize;
    for contact_id in contacts {
        // `force_schedule = true`: the queue row IS the ledger, even at zero delay.
        fire_rule(db, rule, rule.tenant_id, "contact", contact_id, true).await?;
        queued += 1;
    }
    Ok(queued)
}

/// Tell the automation engine that a contact was CREATED, from the door that created it.
///
/// Both labels are dispatched because `create_rule` accepts both spellings and a rule written with
/// the other one would silently never fire. Fire-and-forget: contact creation must never wait on a
/// rule query, and a failure here must not fail the create.
pub fn contact_created(db: PgPool, tenant_id: Uuid, contact_id: Uuid) {
    tokio::spawn(async move {
        for label in ["contact.created", "ContactCreated"] {
            crate::events::dispatcher::dispatch_rule_event(
                &db,
                tenant_id,
                label,
                Some("contact"),
                Some(contact_id),
            )
            .await;
        }
    });
}
