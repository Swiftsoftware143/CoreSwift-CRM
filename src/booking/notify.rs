//! What happens around a booking that is not the booking itself.
//!
//! Three jobs, all of them non-fatal by design: telling the customer, telling the team, and tagging
//! the contact. A booking is never failed because an email could not be sent or a tag could not be
//! written — the money and the slot come first, the notification is a courtesy that retries next
//! time. Everything here logs its own failure rather than returning it.
//!
//! Called after the database has committed: the confirmation goes out only for a booking that
//! actually exists.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// One booking's readable form, shared by the mail and the tags.
struct Line {
    resource_name: String,
    starts_at: DateTime<Utc>,
    price_cents: i64,
}

struct Context {
    tenant_id: Uuid,
    calendar_id: Uuid,
    calendar_name: String,
    timezone: String,
    customer_name: String,
    customer_email: String,
    total_cents: i64,
    manage_token: String,
    lines: Vec<Line>,
}

async fn load(pool: &PgPool, order_id: Uuid) -> Option<Context> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, String, String, String, String, i64, String)>(
        "SELECT o.tenant_id, o.calendar_id, c.name, c.timezone,
                o.customer_name, o.customer_email, o.total_cents, o.manage_token
           FROM booking_orders o
           JOIN booking_calendars c ON c.id = o.calendar_id
          WHERE o.id = $1",
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await
    .ok()??;

    let lines: Vec<Line> = sqlx::query_as::<_, (String, DateTime<Utc>, i64)>(
        "SELECT r.name, b.starts_at, b.price_cents
           FROM booking_bookings b
           JOIN booking_resources r ON r.id = b.resource_id
          WHERE b.order_id = $1 AND b.status <> 'cancelled'
          ORDER BY b.starts_at",
    )
    .bind(order_id)
    .fetch_all(pool)
    .await
    .ok()?
    .into_iter()
    .map(|(resource_name, starts_at, price_cents)| Line {
        resource_name,
        starts_at,
        price_cents,
    })
    .collect();

    Some(Context {
        tenant_id: row.0,
        calendar_id: row.1,
        calendar_name: row.2,
        timezone: row.3,
        customer_name: row.4,
        customer_email: row.5,
        total_cents: row.6,
        manage_token: row.7,
        lines,
    })
}

fn money(cents: i64) -> String {
    format!("${}.{:02}", cents / 100, (cents % 100).abs())
}

/// The items rendered as plain lines, in the calendar's own time zone where it can be resolved.
fn items_text(ctx: &Context) -> String {
    if ctx.lines.is_empty() {
        return "(no items)".to_string();
    }
    let tz: Option<chrono_tz::Tz> = ctx.timezone.parse().ok();
    ctx.lines
        .iter()
        .map(|l| match tz {
            Some(tz) => format!(
                "- {} — {} — {}",
                l.resource_name,
                l.starts_at.with_timezone(&tz).format("%a %d %b %Y, %H:%M"),
                money(l.price_cents)
            ),
            None => format!(
                "- {} — {} — {}",
                l.resource_name,
                l.starts_at.format("%a %d %b %Y, %H:%M UTC"),
                money(l.price_cents)
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn first_item(ctx: &Context) -> String {
    ctx.lines
        .first()
        .map(|l| l.resource_name.clone())
        .unwrap_or_else(|| ctx.calendar_name.clone())
}

fn manage_url(ctx: &Context) -> String {
    format!(
        "https://booking.coreswiftcrm.com/book/manage/{}",
        ctx.manage_token
    )
}

fn vars(ctx: &Context) -> serde_json::Value {
    serde_json::json!({
        "customer_name": ctx.customer_name,
        "customer_email": ctx.customer_email,
        "calendar_name": ctx.calendar_name,
        "items": items_text(ctx),
        "first_item": first_item(ctx),
        "total": money(ctx.total_cents),
        "manage_url": manage_url(ctx),
    })
}

/// A booking was confirmed (or made for free): tell the customer, and tell the team.
pub async fn on_confirmed(pool: &PgPool, order_id: Uuid) {
    let Some(ctx) = load(pool, order_id).await else {
        tracing::warn!(order = %order_id, "booking notify: order not found, nothing sent");
        return;
    };

    let send = sqlx::query_scalar::<_, bool>(
        "SELECT send_confirmation FROM booking_calendars WHERE id = $1",
    )
    .bind(ctx.calendar_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(true);

    if send {
        if let Err(e) = crate::email::send_template_email(
            pool,
            ctx.tenant_id,
            &ctx.customer_email,
            "booking_confirmed",
            &vars(&ctx),
        )
        .await
        {
            tracing::warn!(order = %order_id, error = %e, "booking notify: customer confirmation failed");
        }
    }

    let team: Vec<String> = sqlx::query_scalar(
        "SELECT email FROM booking_notify_emails WHERE calendar_id = $1 AND is_active",
    )
    .bind(ctx.calendar_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    for address in team {
        if let Err(e) = crate::email::send_template_email(
            pool,
            ctx.tenant_id,
            &address,
            "booking_team_heads_up",
            &vars(&ctx),
        )
        .await
        {
            tracing::warn!(order = %order_id, to = %address, error = %e,
                "booking notify: team heads-up failed");
        }
    }

    apply_tags(pool, &ctx, "confirm").await;
}

/// A booking was cancelled: run the cancel-stage tag rules (add "Cancelled", or take the confirmed
/// tag back off — the owner chooses which question the tag answers).
pub async fn on_cancelled(pool: &PgPool, order_id: Uuid) {
    if let Some(ctx) = load(pool, order_id).await {
        apply_tags(pool, &ctx, "cancel").await;
    }
}

/// Find the contact for this customer, creating a minimal one if the CRM has never seen them, then
/// apply the calendar's tag rules for the stage.
async fn apply_tags(pool: &PgPool, ctx: &Context, stage: &str) {
    let rules: Vec<(String, String)> = sqlx::query_as(
        "SELECT tag_name, mode FROM booking_tag_rules
          WHERE calendar_id = $1 AND stage = $2 AND is_active",
    )
    .bind(ctx.calendar_id)
    .bind(stage)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if rules.is_empty() {
        return;
    }

    let contact_id = match find_or_create_contact(pool, ctx).await {
        Some(id) => id,
        None => return,
    };

    for (tag_name, mode) in rules {
        let tag_id = match ensure_tag(pool, ctx.tenant_id, &tag_name).await {
            Some(id) => id,
            None => continue,
        };
        if mode == "remove" {
            let _ = sqlx::query(
                "DELETE FROM tag_assignments
                  WHERE tenant_id = $1 AND tag_id = $2 AND entity_type = 'contact' AND entity_id = $3",
            )
            .bind(ctx.tenant_id)
            .bind(tag_id)
            .bind(contact_id)
            .execute(pool)
            .await;
        } else {
            // ON CONFLICT keeps a replayed webhook from assigning the same tag twice
            let _ = sqlx::query(
                "INSERT INTO tag_assignments (id, tenant_id, tag_id, entity_type, entity_id)
                 VALUES (gen_random_uuid(), $1, $2, 'contact', $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(ctx.tenant_id)
            .bind(tag_id)
            .bind(contact_id)
            .execute(pool)
            .await;
        }
    }
}

async fn find_or_create_contact(pool: &PgPool, ctx: &Context) -> Option<Uuid> {
    if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM contacts WHERE tenant_id = $1 AND lower(email) = lower($2) LIMIT 1",
    )
    .bind(ctx.tenant_id)
    .bind(&ctx.customer_email)
    .fetch_optional(pool)
    .await
    {
        return Some(id);
    }

    let (first, last) = split_name(&ctx.customer_name);
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, source, metadata)
         VALUES (gen_random_uuid(), $1, $2, $3, $4, 'booking', jsonb_build_object('source_app','coreswift','from_booking',true))
         ON CONFLICT DO NOTHING
         RETURNING id",
    )
    .bind(ctx.tenant_id)
    .bind(first)
    .bind(last)
    .bind(&ctx.customer_email)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

fn split_name(full: &str) -> (String, Option<String>) {
    let mut parts = full.trim().splitn(2, ' ');
    let first = parts.next().unwrap_or("").to_string();
    let last = parts
        .next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    (first, last)
}

/// Tags are matched case-insensitively, so "Webinar" and "webinar" can never fork into two tags.
async fn ensure_tag(pool: &PgPool, tenant_id: Uuid, name: &str) -> Option<Uuid> {
    let clean = name.trim();
    if clean.is_empty() {
        return None;
    }
    if let Ok(Some(id)) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM tags WHERE tenant_id = $1 AND lower(name) = lower($2) LIMIT 1",
    )
    .bind(tenant_id)
    .bind(clean)
    .fetch_optional(pool)
    .await
    {
        return Some(id);
    }
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO tags (id, tenant_id, name, color) VALUES (gen_random_uuid(), $1, $2, '#6366f1')
         ON CONFLICT DO NOTHING RETURNING id",
    )
    .bind(tenant_id)
    .bind(clean)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

// ============================================================================================
// Reminders
// --------------------------------------------------------------------------------------------
// A booking's reminder schedule is per calendar (`booking_reminders`: an offset in minutes before the
// start, and who hears it). A tick runs at a fixed interval, finds what is due, sends it, and records
// the send — so a restart, an overlapping tick or a second instance can never send the same reminder
// twice. It is deliberately NOT a queue: the schedule is data, and the send table is the ledger.
// ============================================================================================

/// How often the reminder tick runs. Five minutes is the smallest useful granularity for "a day
/// before" and it keeps the query cheap.
pub const REMINDER_TICK: std::time::Duration = std::time::Duration::from_secs(300);

pub async fn run_reminders(pool: &PgPool) -> usize {
    let due: Vec<(Uuid, Uuid, i32, String)> = sqlx::query_as(
        "SELECT b.id, o.tenant_id, rem.offset_minutes, rem.audience
           FROM booking_bookings b
           JOIN booking_orders o    ON o.id = b.order_id
           JOIN booking_calendars c ON c.id = o.calendar_id
           JOIN booking_reminders rem ON rem.calendar_id = c.id AND rem.is_active
          WHERE b.status <> 'cancelled'
            AND o.status IN ('pending', 'confirmed')
            AND b.starts_at > now()
            AND b.starts_at - (rem.offset_minutes * interval '1 minute') <= now()
            AND NOT EXISTS (
                SELECT 1 FROM booking_reminder_sends s
                 WHERE s.booking_id = b.id
                   AND s.offset_minutes = rem.offset_minutes
                   AND s.audience = rem.audience)
          ORDER BY b.starts_at
          LIMIT 200",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    let mut sent = 0usize;
    for (booking_id, tenant_id, offset, audience) in due {
        let Some(order_id) =
            sqlx::query_scalar::<_, Uuid>("SELECT order_id FROM booking_bookings WHERE id = $1")
                .bind(booking_id)
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
        else {
            continue;
        };
        let Some(ctx) = load(pool, order_id).await else {
            continue;
        };

        let recipients: Vec<String> = if audience == "team" {
            sqlx::query_scalar(
                "SELECT email FROM booking_notify_emails WHERE calendar_id = $1 AND is_active",
            )
            .bind(ctx.calendar_id)
            .fetch_all(pool)
            .await
            .unwrap_or_default()
        } else {
            vec![ctx.customer_email.clone()]
        };

        let mut all_ok = true;
        for address in recipients {
            if let Err(e) = crate::email::send_template_email(
                pool,
                tenant_id,
                &address,
                "booking_reminder",
                &vars(&ctx),
            )
            .await
            {
                all_ok = false;
                tracing::warn!(booking = %booking_id, to = %address, error = %e,
                    "booking reminder: send failed");
            }
        }

        // record only when the send was accepted, so a failure is retried on the next tick
        if all_ok {
            let _ = sqlx::query(
                "INSERT INTO booking_reminder_sends (id, booking_id, offset_minutes, audience)
                 VALUES (gen_random_uuid(), $1, $2, $3)
                 ON CONFLICT DO NOTHING",
            )
            .bind(booking_id)
            .bind(offset)
            .bind(&audience)
            .execute(pool)
            .await;
            sent += 1;
        }
    }
    if sent > 0 {
        tracing::info!(count = sent, "booking reminders sent");
    }
    sent
}
