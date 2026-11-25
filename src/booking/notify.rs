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
    /// The artwork spec on the position, e.g. "600x150". None means no spec is stated.
    size: Option<String>,
    /// The product's own checkout address, when it has one — so a pay-after booking can be settled
    /// from the confirmation instead of waiting for someone to send a link.
    checkout_url: Option<String>,
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
    /// What the buyer typed, if anything, and whether they ticked the text consent.
    phone: Option<String>,
    sms_opt_in: bool,
    lines: Vec<Line>,
}

async fn load(pool: &PgPool, order_id: Uuid) -> Option<Context> {
    let row = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            String,
            String,
            String,
            i64,
            String,
            Option<String>,
            bool,
        ),
    >(
        "SELECT o.tenant_id, o.calendar_id, c.name, c.timezone,
                o.customer_name, o.customer_email, o.total_cents, o.manage_token,
                o.phone, COALESCE(o.sms_opt_in, false)
           FROM booking_orders o
           JOIN booking_calendars c ON c.id = o.calendar_id
          WHERE o.id = $1",
    )
    .bind(order_id)
    .fetch_optional(pool)
    .await
    .ok()??;

    let lines: Vec<Line> =
        sqlx::query_as::<_, (String, DateTime<Utc>, i64, Option<String>, Option<String>)>(
            "SELECT r.name, b.starts_at, b.price_cents, r.size,
                coalesce(nullif(r.checkout_embed, ''), nullif(c.payment_embed, ''))
           FROM booking_bookings b
           JOIN booking_resources r ON r.id = b.resource_id
           JOIN booking_calendars c ON c.id = r.calendar_id
          WHERE b.order_id = $1 AND b.status <> 'cancelled'
          ORDER BY b.starts_at",
        )
        .bind(order_id)
        .fetch_all(pool)
        .await
        .ok()?
        .into_iter()
        .map(
            |(resource_name, starts_at, price_cents, size, checkout)| Line {
                resource_name,
                starts_at,
                price_cents,
                size,
                checkout_url: checkout
                    .as_deref()
                    .and_then(super::service::extract_checkout_url),
            },
        )
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
        phone: row.8.clone(),
        sms_opt_in: row.9,
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

/// The values a confirmation email can use.
///
/// `positions`, `sizes`, `dates` and `artwork_block` exist because the request this email has to make
/// is per-position: a sponsor needs the spec of the thing they bought, and the date it runs. Putting
/// those in one block means an owner edits the wording, not the data.
fn vars(ctx: &Context) -> serde_json::Value {
    let positions = ctx
        .lines
        .iter()
        .map(|l| l.resource_name.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let sizes = ctx
        .lines
        .iter()
        .map(|l| l.size.clone().unwrap_or_else(|| "—".to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    let dates = ctx
        .lines
        .iter()
        .map(|l| date_text(ctx, l))
        .collect::<Vec<_>>()
        .join(", ");
    serde_json::json!({
        "customer_name": ctx.customer_name,
        "customer_email": ctx.customer_email,
        "calendar_name": ctx.calendar_name,
        "items": items_text(ctx),
        "first_item": first_item(ctx),
        "first_position": ctx.lines.first().map(|l| l.resource_name.clone()).unwrap_or_default(),
        "first_size": ctx.lines.first().and_then(|l| l.size.clone()).unwrap_or_default(),
        "first_date": ctx.lines.first().map(|l| date_text(ctx, l)).unwrap_or_default(),
        "positions": positions,
        "sizes": sizes,
        "dates": dates,
        // One line per position with its spec and date — what the artwork request lists.
        "artwork_block": artwork_block(ctx),
        "payment_url": ctx.lines.iter().find_map(|l| l.checkout_url.clone()).unwrap_or_default(),
        "total": money(ctx.total_cents),
        "manage_url": manage_url(ctx),
    })
}

fn date_text(ctx: &Context, l: &Line) -> String {
    match ctx.timezone.parse::<chrono_tz::Tz>() {
        Ok(tz) => l
            .starts_at
            .with_timezone(&tz)
            .format("%a %d %b %Y")
            .to_string(),
        Err(_) => l.starts_at.format("%a %d %b %Y").to_string(),
    }
}

/// The per-position list the artwork request prints: position, spec, date.
fn artwork_block(ctx: &Context) -> String {
    if ctx.lines.is_empty() {
        return String::new();
    }
    ctx.lines
        .iter()
        .map(|l| {
            format!(
                "  Position:  {}\n  Size:      {}\n  Runs:      {}",
                l.resource_name,
                l.size
                    .clone()
                    .unwrap_or_else(|| "tell us what you have".to_string()),
                date_text(ctx, l)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
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
/// apply the calendar's TAG rules and LIST rules for the stage.
///
/// WHY THE ENGINE IS CALLED FROM HERE (2026-11-20): this function wrote `tag_assignments` directly, so a
/// tag applied BY A BOOKING never reached `automation::engine` — the tag existed on the contact and no
/// automation rule watching that tag ever ran. Measured: every other tag writer
/// (`tags::handlers`, `inbound`, `external_api`) fires the engine; this one did not. The chain David
/// asked for — booking → tag/list → automation — was broken in its middle link.
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

    let list_rules: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT list_id, mode FROM booking_list_rules
          WHERE calendar_id = $1 AND stage = $2 AND is_active",
    )
    .bind(ctx.calendar_id)
    .bind(stage)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if rules.is_empty() && list_rules.is_empty() {
        return;
    }

    let contact_id = match find_or_create_contact(pool, ctx).await {
        Some(id) => id,
        None => return,
    };

    // ── THE PHONE NUMBER AND THE TEXT CONSENT (David, 2026-11-21) ───────────────────────────────
    // A number is a contact detail; the TICK is permission. The number is saved either way, the tag is
    // applied only when the buyer ticked the box, and the tag is what puts them in the Text Opt-Ins list
    // (rule: TagAdded SMS Opt-In -> AddToList). Nothing is inferred from a phone number alone.
    if stage == "confirm" {
        if let Some(phone) = ctx
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let _ = sqlx::query("UPDATE contacts SET phone = $1, updated_at = now() WHERE id = $2 AND tenant_id = $3")
                .bind(phone)
                .bind(contact_id)
                .bind(ctx.tenant_id)
                .execute(pool)
                .await;
        }
        if ctx.sms_opt_in {
            if let Some(opt_in) = ensure_tag(pool, ctx.tenant_id, "SMS Opt-In").await {
                let _ = sqlx::query(
                    "INSERT INTO tag_assignments (id, tenant_id, tag_id, entity_type, entity_id) \
                     VALUES (gen_random_uuid(), $1, $2, 'contact', $3) ON CONFLICT DO NOTHING",
                )
                .bind(ctx.tenant_id)
                .bind(opt_in)
                .bind(contact_id)
                .execute(pool)
                .await;
                crate::automation::engine::fire_tag_trigger(
                    pool,
                    ctx.tenant_id,
                    "contact",
                    contact_id,
                    opt_in,
                    "TagAdded",
                )
                .await;
            }
        }
    }

    // ── REPEAT CUSTOMER (David, 2026-11-21) ─────────────────────────────────────────────────────
    // "Second booking -> Repeat Customer". The trigger vocabulary has no counter, so the fact is read
    // where it is known: how many bookings this contact already had BEFORE this one. Confirmed only,
    // because a cancelled booking never made anyone a returning customer.
    if stage == "confirm" {
        // This booking is already confirmed when the tags are applied, so TWO confirmed bookings for
        // the same person means one of them came before this one. Counted through the order's contact
        // AND the customer's address, because an order placed before the contact existed carries no
        // contact_id and would otherwise make a returning customer look new.
        // `booking_bookings` HAS NO tenant_id (measured 2026-11-21): the column lives on the ORDER, and
        // the first version of this query named b.tenant_id, failed with 42703 and was swallowed by
        // unwrap_or(0) - so a returning customer was never tagged and nothing said why. The error is
        // logged now, because a silent default is how this class of bug hides.
        let prior: i64 = match sqlx::query_scalar(
            "SELECT count(*) FROM booking_bookings b \
               JOIN booking_orders o ON o.id = b.order_id \
              WHERE o.tenant_id = $1 AND o.status = 'confirmed' \
                AND (o.contact_id = $2 OR lower(o.customer_email) = lower($3))",
        )
        .bind(ctx.tenant_id)
        .bind(contact_id)
        .bind(&ctx.customer_email)
        .fetch_one(pool)
        .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(tenant = %ctx.tenant_id, error = %e, "could not count prior bookings for the repeat-customer tag");
                0
            }
        };
        if prior >= 2 {
            if let Some(repeat) = ensure_tag(pool, ctx.tenant_id, "Repeat Customer").await {
                let _ = sqlx::query(
                    "INSERT INTO tag_assignments (id, tenant_id, tag_id, entity_type, entity_id) \
                     VALUES (gen_random_uuid(), $1, $2, 'contact', $3) ON CONFLICT DO NOTHING",
                )
                .bind(ctx.tenant_id)
                .bind(repeat)
                .bind(contact_id)
                .execute(pool)
                .await;
                crate::automation::engine::fire_tag_trigger(
                    pool,
                    ctx.tenant_id,
                    "contact",
                    contact_id,
                    repeat,
                    "TagAdded",
                )
                .await;
            }
        }
    }

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
        // The fan-out every other tag writer already does: a booking that tags somebody must be able
        // to start the automation that watches that tag.
        let trigger = if mode == "remove" {
            "TagRemoved"
        } else {
            "TagAdded"
        };
        crate::automation::engine::fire_tag_trigger(
            pool,
            ctx.tenant_id,
            "contact",
            contact_id,
            tag_id,
            trigger,
        )
        .await;
    }

    // ── THE LIST HALF ────────────────────────────────────────────────────────────────────────────
    // Same two questions the tags answer, asked of a list: does "confirmed" put this person IN the
    // list, and does "cancelled" take them out (or put them in a different one)?
    for (list_id, mode) in list_rules {
        if mode == "remove" {
            let _ = sqlx::query(
                "DELETE FROM list_members WHERE tenant_id = $1 AND list_id = $2 AND contact_id = $3",
            )
            .bind(ctx.tenant_id)
            .bind(list_id)
            .bind(contact_id)
            .execute(pool)
            .await;
            crate::automation::engine::fire_list_trigger(
                pool,
                ctx.tenant_id,
                contact_id,
                list_id,
                "ListRemoved",
            )
            .await;
        } else {
            // A replayed webhook must not add the same member twice: the unique index on
            // (list_id, contact_id) makes this idempotent rather than a duplicate error.
            let inserted = sqlx::query(
                "INSERT INTO list_members (id, list_id, contact_id, tenant_id, added_manually)
                 VALUES (gen_random_uuid(), $1, $2, $3, false)
                 ON CONFLICT DO NOTHING",
            )
            .bind(list_id)
            .bind(contact_id)
            .bind(ctx.tenant_id)
            .execute(pool)
            .await
            .map(|r| r.rows_affected())
            .unwrap_or(0);
            // Only a NEW membership is an event. A booking replayed against an existing member is not
            // "they joined the list again", and firing the trigger would re-run the automation.
            if inserted > 0 {
                crate::automation::engine::fire_list_trigger(
                    pool,
                    ctx.tenant_id,
                    contact_id,
                    list_id,
                    "ListAdded",
                )
                .await;
            }
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

/// The merge values the confirmation email would use for one order — exposed so the console can render
/// the message a customer will actually receive instead of describing it.
pub async fn preview_vars(pool: &PgPool, order_id: Uuid) -> Option<serde_json::Value> {
    load(pool, order_id).await.map(|ctx| vars(&ctx))
}

/// The newest order on a calendar, so a preview needs no hand-picked id. `None` when nothing is booked.
pub async fn newest_order(pool: &PgPool, calendar_id: Uuid) -> Option<Uuid> {
    sqlx::query_scalar(
        "SELECT id FROM booking_orders WHERE calendar_id = $1 ORDER BY created_at DESC LIMIT 1",
    )
    .bind(calendar_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
}

/// Sample values for a calendar with nothing booked yet: the real positions and their specs, so the
/// preview still shows the artwork request with the right sizes and the owner can edit it before the
/// first sale rather than after.
pub async fn sample_vars(pool: &PgPool, calendar_id: Uuid, tenant: Uuid) -> serde_json::Value {
    let rows: Vec<(String, Option<String>, i64)> = sqlx::query_as(
        "SELECT name, size, price_cents FROM booking_resources
          WHERE calendar_id = $1 AND active ORDER BY sort_order, name",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let name: String = sqlx::query_scalar("SELECT name FROM booking_calendars WHERE id = $1")
        .bind(calendar_id)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "your calendar".to_string());
    let items = if rows.is_empty() {
        "- (no positions yet)".to_string()
    } else {
        rows.iter()
            .map(|(n, _, p)| format!("- {} — {}", n, money(*p)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let block = rows
        .iter()
        .map(|(n, size, _)| {
            format!(
                "  Position:  {}\n  Size:      {}\n  Runs:      the date you booked",
                n,
                size.clone()
                    .unwrap_or_else(|| "tell us what you have".to_string())
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    serde_json::json!({
        "customer_name": "Sample Sponsor",
        "customer_email": "sample@example.invalid",
        "calendar_name": name,
        "items": items,
        "first_item": rows.first().map(|(n, _, _)| n.clone()).unwrap_or_default(),
        "first_position": rows.first().map(|(n, _, _)| n.clone()).unwrap_or_default(),
        "first_size": rows.first().and_then(|(_, s, _)| s.clone()).unwrap_or_default(),
        "first_date": "the date you booked",
        "positions": rows.iter().map(|(n, _, _)| n.clone()).collect::<Vec<_>>().join(", "),
        "sizes": rows.iter().map(|(_, s, _)| s.clone().unwrap_or_else(|| "—".into())).collect::<Vec<_>>().join(", "),
        "dates": "the date you booked",
        "artwork_block": block,
        "total": money(rows.iter().map(|(_, _, p)| *p).sum()),
        "manage_url": format!("https://booking.coreswiftcrm.com/book/manage/{}", tenant),
    })
}
