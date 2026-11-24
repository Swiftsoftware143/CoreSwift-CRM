//! Database operations. Everything takes `&mut PgConnection` so it works inside or outside a transaction.

use super::availability::{appointment_windows, day_start, seats_used};
use super::error::{BookingError, Result};
use super::models::*;
use super::pricing::{self, LineItem};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

// A booking occupies capacity if it is confirmed, or held and the hold has not run out. The
// predicate is written out inside each statement rather than interpolated: the release gate refuses
// run-time-built SQL, and a constant spliced into a query is exactly that.
const MAX_RANGE_DAYS: i64 = 92;
const MAX_ITEMS: usize = 100;

pub struct ResolvedItem {
    pub resource: Resource,
    pub slot_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub quantity: i32,
    pub price_cents: i64,
    pub meta: Value,
}

pub fn tz_of(cal: &Calendar) -> Result<Tz> {
    cal.timezone
        .parse::<Tz>()
        .map_err(|_| BookingError::invalid(format!("bad timezone: {}", cal.timezone)))
}

fn day_starts(tz: Tz, from: NaiveDate, to_exclusive: NaiveDate) -> Vec<DateTime<Utc>> {
    from.iter_days()
        .take_while(|d| *d < to_exclusive)
        .filter_map(|d| day_start(tz, d))
        .collect()
}

// ---------- lookups ----------

pub async fn calendar_for_tenant(
    conn: &mut PgConnection,
    id: Uuid,
    tenant: Uuid,
) -> Result<Calendar> {
    sqlx::query_as::<_, Calendar>(
        "SELECT * FROM booking_calendars WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant)
    .fetch_optional(conn)
    .await?
    .ok_or(BookingError::NotFound("calendar"))
}

/// Resolve a calendar from the address a customer opened.
///
/// A calendar's name is unique per WORKSPACE, not platform-wide: "consultations" is a good name and
/// two owners are allowed to want it. So a bare name is only accepted while it is unambiguous, and a
/// name that exists in more than one workspace must arrive workspace-qualified — which the shared
/// address does (`/b/<workspace>/<name>`), while a workspace host or a verified own domain implies
/// the workspace by host.
pub async fn calendar_by_slug(
    conn: &mut PgConnection,
    slug: &str,
    workspace: Option<&str>,
) -> Result<Calendar> {
    let tenant: Option<Uuid> = match workspace.map(str::trim).filter(|w| !w.is_empty()) {
        Some(ws) => Some(
            sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM tenants WHERE (slug = $1 OR lower(name) = lower($1)) AND is_active LIMIT 1",
            )
            .bind(ws)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(BookingError::NotFound("workspace"))?,
        ),
        None => None,
    };

    let rows = sqlx::query_as::<_, Calendar>(
        "SELECT * FROM booking_calendars
          WHERE slug = $1 AND is_active AND ($2::uuid IS NULL OR tenant_id = $2)
          ORDER BY created_at",
    )
    .bind(slug)
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await?;

    let mut rows = rows.into_iter();
    let first = rows.next().ok_or(BookingError::NotFound("calendar"))?;
    if rows.next().is_some() {
        return Err(BookingError::conflict(
            "that calendar name exists in more than one workspace — open it from the workspace's own address, or with the workspace in the link",
        ));
    }
    Ok(first)
}

async fn calendar_by_id(conn: &mut PgConnection, id: Uuid) -> Result<Calendar> {
    sqlx::query_as::<_, Calendar>("SELECT * FROM booking_calendars WHERE id = $1")
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(BookingError::NotFound("calendar"))
}

pub async fn resource_for_tenant(
    conn: &mut PgConnection,
    id: Uuid,
    tenant: Uuid,
) -> Result<(Resource, Calendar)> {
    let res = sqlx::query_as::<_, Resource>("SELECT * FROM booking_resources WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or(BookingError::NotFound("resource"))?;
    let cal = calendar_for_tenant(conn, res.calendar_id, tenant)
        .await
        .map_err(|_| BookingError::NotFound("resource"))?;
    Ok((res, cal))
}

async fn rules_for(conn: &mut PgConnection, resource_id: Uuid) -> Result<Vec<AvailabilityRule>> {
    Ok(sqlx::query_as::<_, AvailabilityRule>(
        "SELECT weekday, start_time, end_time FROM booking_availability_rules WHERE resource_id = $1 ORDER BY weekday, start_time",
    )
    .bind(resource_id)
    .fetch_all(conn)
    .await?)
}

pub async fn list_resources(
    conn: &mut PgConnection,
    calendar_id: Uuid,
    only_active: bool,
) -> Result<Vec<Resource>> {
    Ok(sqlx::query_as::<_, Resource>(
        "SELECT * FROM booking_resources WHERE calendar_id = $1 AND (active OR NOT $2) ORDER BY sort_order, name",
    )
    .bind(calendar_id)
    .bind(only_active)
    .fetch_all(conn)
    .await?)
}

pub async fn list_bundles(
    conn: &mut PgConnection,
    calendar_id: Uuid,
    only_active: bool,
) -> Result<Vec<Bundle>> {
    Ok(sqlx::query_as::<_, Bundle>(
        "SELECT id, calendar_id, name, kind, min_items, percent_off, fixed_price_cents, eligible_resource_ids, required_resource_ids, active
         FROM booking_bundles WHERE calendar_id = $1 AND (active OR NOT $2) ORDER BY created_at",
    )
    .bind(calendar_id)
    .bind(only_active)
    .fetch_all(conn)
    .await?)
}

// ---------- admin: setup ----------

pub async fn create_calendar(
    conn: &mut PgConnection,
    tenant: Uuid,
    c: NewCalendar,
) -> Result<Calendar> {
    let tz = c.timezone.unwrap_or_else(|| "America/New_York".into());
    tz.parse::<Tz>()
        .map_err(|_| BookingError::invalid("bad timezone: {tz}"))?;
    let slug = c.slug.trim().to_lowercase();
    if slug.is_empty()
        || !slug
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        return Err(BookingError::invalid(
            "slug may only contain letters, numbers and dashes",
        ));
    }
    Ok(sqlx::query_as::<_, Calendar>(
        "INSERT INTO booking_calendars (tenant_id, name, slug, kind, timezone, currency, hold_minutes, settings)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
    )
    .bind(tenant)
    .bind(c.name.trim())
    .bind(slug)
    .bind(c.kind.as_str())
    .bind(tz)
    .bind(c.currency.unwrap_or_else(|| "usd".into()).to_lowercase())
    .bind(c.hold_minutes.unwrap_or(30).clamp(1, 24 * 60))
    .bind(c.settings.unwrap_or_else(|| json!({})))
    .fetch_one(conn)
    .await?)
}

/// Update the parts of a calendar an owner is allowed to change: its name, and its look.
///
/// The look lives in `settings.theme` (brand colour, ink, background, font stack) rather than in new
/// columns, because it is presentation and will keep growing — a column per knob is a migration per
/// knob. Every value is length-limited and colour-checked HERE: this string is written into a public
/// page's stylesheet, so it is validated as data, never interpolated blindly.
pub async fn update_calendar(
    pool: &PgPool,
    tenant: Uuid,
    id: Uuid,
    name: Option<String>,
    slug: Option<String>,
    settings: Option<serde_json::Value>,
    reply_to_email: Option<String>,
    send_confirmation: Option<bool>,
) -> Result<Calendar> {
    let current = calendar_for_tenant(&mut *pool.acquire().await?, id, tenant).await?;

    let new_name = match name {
        Some(n) if !n.trim().is_empty() => n.trim().to_string(),
        Some(_) => return Err(BookingError::invalid("the calendar needs a name")),
        None => current.name.clone(),
    };

    let mut merged = current.settings.clone();
    if let Some(incoming) = settings {
        if let (Some(base), Some(extra)) = (merged.as_object_mut(), incoming.as_object()) {
            for (k, v) in extra {
                base.insert(k.clone(), v.clone());
            }
        } else {
            merged = incoming;
        }
    }
    // THE WEB ADDRESS CAN BE CORRECTED (ZaarHub, 2026-11-23): it could only be set at creation, so a
    // wrong address meant rebuilding the calendar. Renaming it is allowed, validated the same way as
    // creation, scoped to the account, and the OLD address stops working — which the console says out
    // loud before saving, because a link already published in an email will 404 afterwards.
    let new_slug = match slug {
        Some(raw) => {
            let cand = raw.trim().to_lowercase();
            if cand.is_empty()
                || cand.len() > 60
                || !cand
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            {
                return Err(BookingError::invalid(
                    "a web address uses letters, numbers and dashes only (up to 60)",
                ));
            }
            if cand != current.slug {
                let taken: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM booking_calendars \
                        WHERE tenant_id = $1 AND slug = $2 AND id <> $3)",
                )
                .bind(tenant)
                .bind(&cand)
                .bind(id)
                .fetch_one(&mut *pool.acquire().await?)
                .await?;
                if taken {
                    return Err(BookingError::conflict(format!(
                        "another calendar on this account already answers on /b/{cand}"
                    )));
                }
            }
            cand
        }
        None => current.slug.clone(),
    };

    // validate the theme if one is present
    if let Some(theme) = merged.get("theme") {
        validate_theme(theme)?;
    }
    // Where a buyer goes when there is money to pay. https only, for the same reason the plan links
    // are: this address is opened by a customer with a card in hand.
    if let Some(v) = merged.get("checkout_url") {
        match v.as_str() {
            Some(u) if u.trim().is_empty() => {}
            Some(u) if u.trim().starts_with("https://") => {}
            _ => {
                return Err(BookingError::invalid(
                    "checkout_url must start with https://",
                ))
            }
        }
    }

    let reply = match reply_to_email {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v.trim().to_string()),
        None => current.reply_to_email.clone(),
    };
    let send = send_confirmation.unwrap_or(current.send_confirmation);

    sqlx::query_as::<_, Calendar>(
        "UPDATE booking_calendars
            SET name = $3, slug = $7, settings = $4, reply_to_email = $5, send_confirmation = $6,
                updated_at = now()
          WHERE id = $1 AND tenant_id = $2
          RETURNING *",
    )
    .bind(id)
    .bind(tenant)
    .bind(new_name)
    .bind(merged)
    .bind(reply)
    .bind(send)
    .bind(new_slug)
    .fetch_optional(&mut *pool.acquire().await?)
    .await?
    .ok_or(BookingError::NotFound("calendar"))
}

/// A colour is `#rgb` or `#rrggbb`; a font is a name from a short list. Nothing else reaches the
/// public page's stylesheet.
fn validate_theme(theme: &serde_json::Value) -> Result<()> {
    let obj = match theme.as_object() {
        Some(o) => o,
        None => return Err(BookingError::invalid("theme must be an object")),
    };
    for key in ["brand", "ink", "bg", "accent"] {
        if let Some(v) = obj.get(key) {
            let Some(s) = v.as_str() else {
                return Err(BookingError::invalid(format!(
                    "theme.{key} must be a colour"
                )));
            };
            let ok = s.trim().starts_with('#')
                && matches!(s.trim().len(), 4 | 7)
                && s.trim()[1..].chars().all(|c| c.is_ascii_hexdigit());
            if !ok {
                return Err(BookingError::invalid(format!(
                    "theme.{key} must look like #6366f1"
                )));
            }
        }
    }
    if let Some(v) = obj.get("font") {
        let Some(s) = v.as_str() else {
            return Err(BookingError::invalid("theme.font must be a font name"));
        };
        const ALLOWED: &[&str] = &[
            "system",
            "inter",
            "rounded",
            "geometric",
            "humanist",
            "serif",
            "mono",
        ];
        if !ALLOWED.contains(&s.trim()) {
            return Err(BookingError::invalid(format!(
                "theme.font must be one of: {}",
                ALLOWED.join(", ")
            )));
        }
    }
    if let Some(v) = obj.get("logo_url") {
        let Some(s) = v.as_str() else {
            return Err(BookingError::invalid("theme.logo_url must be an address"));
        };
        if !s.trim().is_empty() && !s.trim().starts_with("https://") {
            return Err(BookingError::invalid(
                "theme.logo_url must start with https://",
            ));
        }
    }
    Ok(())
}

pub async fn list_calendars(conn: &mut PgConnection, tenant: Uuid) -> Result<Vec<Calendar>> {
    Ok(sqlx::query_as::<_, Calendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 ORDER BY created_at",
    )
    .bind(tenant)
    .fetch_all(conn)
    .await?)
}

pub async fn create_resource(
    conn: &mut PgConnection,
    calendar_id: Uuid,
    r: NewResource,
) -> Result<Resource> {
    Ok(sqlx::query_as::<_, Resource>(
        "INSERT INTO booking_resources
           (calendar_id, name, description, capacity, price_cents, compare_at_price_cents, duration_min,
            exclusive, slot_interval_min, buffer_min, min_notice_min, sort_order, metadata)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13) RETURNING *",
    )
    .bind(calendar_id)
    .bind(r.name.trim())
    .bind(r.description.unwrap_or_default())
    .bind(r.capacity.unwrap_or(1).max(1))
    .bind(r.price_cents.unwrap_or(0).max(0))
    // Display only: stored as given (never below zero). Whether it is SHOWN is the page's rule, so a
    // value at or under the price simply does not render a strike-through.
    .bind(r.compare_at_price_cents.map(|v| v.max(0)))
    .bind(r.duration_min.unwrap_or(30).max(1))
    .bind(r.exclusive.unwrap_or(false))
    .bind(r.slot_interval_min.or(r.duration_min).unwrap_or(30).max(1))
    .bind(r.buffer_min.unwrap_or(0).max(0))
    .bind(r.min_notice_min.unwrap_or(0).max(0))
    .bind(r.sort_order.unwrap_or(0))
    .bind(r.metadata.unwrap_or_else(|| json!({})))
    .fetch_one(conn)
    .await?)
}

/// Correct an existing bookable resource (ZaarHub, 2026-11-23).
///
/// A mis-priced position could be CREATED and never corrected: the console offered only "Slots", so the
/// only way to change a price was to rebuild the calendar. Every field is editable here, including the
/// compare-at price — and an empty compare-at means "remove the was-price", which is why it is written
/// unconditionally rather than merged.
pub async fn update_resource(
    conn: &mut PgConnection,
    tenant: Uuid,
    resource_id: Uuid,
    r: NewResource,
) -> Result<Resource> {
    // A DATED SLOT KEEPS ITS OWN PRICE, SO A CORRECTION HAS TO FOLLOW IT (measured 2026-11-23). The
    // price a buyer is charged on a dated slot is the SLOT's, not the resource's: editing the resource
    // from 25.00 to 40.00 left thirteen open dates still selling at 25.00 — the page showed the new
    // "was" figure beside the old price. The old price is read first, and only slots that were still
    // carrying it are moved, so a date deliberately priced on its own is left alone.
    let previous_price: Option<i64> = sqlx::query_scalar(
        "SELECT r.price_cents FROM booking_resources r JOIN booking_calendars c ON c.id = r.calendar_id \
          WHERE r.id = $1 AND c.tenant_id = $2",
    )
    .bind(resource_id)
    .bind(tenant)
    .fetch_optional(&mut *conn)
    .await?;

    let updated = sqlx::query_as::<_, Resource>(
        "UPDATE booking_resources r SET
             name = $3, description = $4, capacity = $5, price_cents = $6,
             compare_at_price_cents = $7, duration_min = $8,
             exclusive = $9, slot_interval_min = $10, buffer_min = $11,
             min_notice_min = $12, sort_order = $13
           FROM booking_calendars c
          WHERE r.calendar_id = c.id AND r.id = $1 AND c.tenant_id = $2
        RETURNING r.*",
    )
    .bind(resource_id)
    .bind(tenant)
    .bind(r.name.trim())
    .bind(r.description.unwrap_or_default())
    .bind(r.capacity.unwrap_or(1).max(1))
    .bind(r.price_cents.unwrap_or(0).max(0))
    .bind(r.compare_at_price_cents.map(|v| v.max(0)))
    .bind(r.duration_min.unwrap_or(30).max(1))
    .bind(r.exclusive.unwrap_or(false))
    .bind(r.slot_interval_min.or(r.duration_min).unwrap_or(30).max(1))
    .bind(r.buffer_min.unwrap_or(0).max(0))
    .bind(r.min_notice_min.unwrap_or(0).max(0))
    .bind(r.sort_order.unwrap_or(0))
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| BookingError::NotFound("resource"))?;

    if let Some(old_price) = previous_price {
        let new_price = updated.price_cents;
        if old_price != new_price {
            let moved = sqlx::query(
                "UPDATE booking_slots SET price_cents = $2 \
                  WHERE resource_id = $1 AND price_cents = $3 AND status = 'open'",
            )
            .bind(resource_id)
            .bind(new_price)
            .bind(old_price)
            .execute(&mut *conn)
            .await?
            .rows_affected();
            if moved > 0 {
                tracing::info!(resource = %resource_id, slots = moved,
                    "resource price corrected: open slots moved with it");
            }
        }
    }
    Ok(updated)
}

/// Remove a bookable resource, and refuse while a live booking depends on it.
///
/// `booking_bookings.resource_id` has no cascade, so a delete would fail deep in the driver with a
/// database error naming a constraint. The count is asked FIRST and answered as a sentence: the owner
/// is told how many bookings stand in the way, which is the only fact that lets them decide (cancel
/// those bookings, or retire the resource instead of deleting it). Slots and open hours cascade.
pub async fn delete_resource(
    conn: &mut PgConnection,
    tenant: Uuid,
    resource_id: Uuid,
) -> Result<(String, i64)> {
    let owned: Option<(String,)> = sqlx::query_as(
        "SELECT r.name FROM booking_resources r JOIN booking_calendars c ON c.id = r.calendar_id \
          WHERE r.id = $1 AND c.tenant_id = $2",
    )
    .bind(resource_id)
    .bind(tenant)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((name,)) = owned else {
        return Err(BookingError::NotFound("resource"));
    };

    let live: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM booking_bookings b \
           JOIN booking_slots s ON s.id = b.slot_id \
          WHERE s.resource_id = $1 \
            AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now()))",
    )
    .bind(resource_id)
    .fetch_one(&mut *conn)
    .await?;

    if live > 0 {
        return Err(BookingError::conflict(format!(
            "\"{name}\" has {live} live booking{}. Cancel {} first, or keep the resource and stop offering it by setting its places to zero — deleting it now would leave {} without the thing {} paid for.",
            if live == 1 { "" } else { "s" },
            if live == 1 { "that booking" } else { "those bookings" },
            if live == 1 { "a buyer" } else { "buyers" },
            if live == 1 { "they" } else { "they" }
        )));
    }

    let slots = sqlx::query("DELETE FROM booking_slots WHERE resource_id = $1")
        .bind(resource_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    sqlx::query("DELETE FROM booking_resources WHERE id = $1")
        .bind(resource_id)
        .execute(&mut *conn)
        .await?;
    Ok((name, slots as i64))
}

/// Replace the weekly open hours of an appointment resource.
pub async fn set_rules(
    pool: &PgPool,
    resource_id: Uuid,
    rules: Vec<AvailabilityRule>,
) -> Result<Vec<AvailabilityRule>> {
    for r in &rules {
        if !(0..=6).contains(&r.weekday) || r.end_time <= r.start_time {
            return Err(BookingError::invalid(
                "each rule needs weekday 0-6 and end_time after start_time",
            ));
        }
    }
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM booking_availability_rules WHERE resource_id = $1")
        .bind(resource_id)
        .execute(&mut *tx)
        .await?;
    for r in &rules {
        sqlx::query("INSERT INTO booking_availability_rules (resource_id, weekday, start_time, end_time) VALUES ($1,$2,$3,$4)")
            .bind(resource_id)
            .bind(r.weekday)
            .bind(r.start_time)
            .bind(r.end_time)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(rules)
}

/// Bulk-create inventory slots. Existing slots at the same time are left untouched.
pub async fn generate_slots(
    pool: &PgPool,
    res: &Resource,
    cal: &Calendar,
    g: GenerateSlots,
) -> Result<Vec<Slot>> {
    if cal.kind != CalendarKind::Inventory {
        return Err(BookingError::invalid(
            "slots can only be generated on inventory calendars",
        ));
    }
    if g.to < g.from || (g.to - g.from).num_days() > 366 {
        return Err(BookingError::invalid("date range must be 1-366 days"));
    }
    let tz = tz_of(cal)?;
    let duration = Duration::minutes(g.duration_min.unwrap_or(res.duration_min).max(1) as i64);
    let mut created = Vec::new();
    let mut tx = pool.begin().await?;
    for day in g.from.iter_days().take_while(|d| *d <= g.to) {
        let wd = chrono::Datelike::weekday(&day).num_days_from_monday();
        if !g.weekdays.contains(&wd) {
            continue;
        }
        let Some(start) = super::availability::local_to_utc(tz, day.and_time(g.time)) else {
            continue;
        };
        // A ZERO IN THE PRICE BOX IS NOT A PRICE (measured 2026-11-23: "my generate created free
        // slots"). The box was read as "0 means free", so clearing it produced $0.00 dates under a
        // $12.00 position — and because a repeat run skipped existing dates, the mistake was permanent.
        // Zero (and negative) now means "no override", which falls back to the position's own price.
        // A genuinely free date is set afterwards on that one date, deliberately.
        let typed = g
            .weekday_prices
            .get(&wd)
            .copied()
            .or(g.price_cents)
            .filter(|p| *p > 0);
        let price = typed.unwrap_or(res.price_cents).max(0);
        // REGENERATING CORRECTS OPEN DATES (2026-11-23). A wrong price on a generated date used to be
        // permanent: the insert skipped dates that already existed. It now overwrites the price and
        // capacity of an OPEN date that nobody has booked, so "generate again" is the fix for a
        // mis-typed run. A date with a live booking (confirmed, or held and unexpired) is never
        // touched — the buyer agreed to a number.
        // ONE SLOT PER POSITION PER ISSUE DATE (measured live 2026-11-24: "Email — TOP reports
        // open_slots: 80 while every other position reports 41"). The conflict target was the exact
        // (resource_id, starts_at) timestamp, so a run at a DIFFERENT time of day created a second slot
        // on the same date instead of correcting the first: 39 dates ended up on sale twice, and a
        // sponsor could book the same issue date twice on one position.
        //
        // The inventory unit is the DATE, so the date is what a run corrects. A slot already on that
        // local date with nobody booked has its time, price and capacity corrected in place (same id,
        // so any tags or links that reference it survive); a slot with a live booking stops the run for
        // that date rather than sitting beside a duplicate.
        let first_here = super::availability::local_to_utc(tz, day.and_hms_opt(0, 0, 0).unwrap())
            .unwrap_or_else(Utc::now);
        let last_here = first_here + Duration::days(1);
        let existing: Option<(Uuid, bool)> = sqlx::query_as(
            "SELECT s.id,
                    EXISTS (SELECT 1 FROM booking_bookings b
                             WHERE b.slot_id = s.id
                               AND (b.status = 'confirmed'
                                    OR (b.status = 'held' AND b.hold_expires_at > now()))) AS taken
               FROM booking_slots s
              WHERE s.resource_id = $1 AND s.starts_at >= $2 AND s.starts_at < $3
              ORDER BY taken DESC, s.starts_at
              LIMIT 1",
        )
        .bind(res.id)
        .bind(first_here)
        .bind(last_here)
        .fetch_optional(&mut *tx)
        .await?;

        let slot = match existing {
            Some((_, true)) => None, // the date is sold; a run never moves a date under a buyer
            Some((sid, false)) => {
                let updated = sqlx::query_as::<_, Slot>(
                    "UPDATE booking_slots
                        SET starts_at = $2, ends_at = $3, capacity = $4, price_cents = $5,
                            label = $6, status = 'open', closed_reason = NULL
                      WHERE id = $1
                        AND status <> 'cancelled'
                     RETURNING *",
                )
                .bind(sid)
                .bind(start)
                .bind(start + duration)
                .bind(g.capacity.unwrap_or(res.capacity).max(1))
                .bind(price.max(0))
                .bind(g.label.clone().unwrap_or_default())
                .fetch_optional(&mut *tx)
                .await?;
                // A slot that had been closed by an exclusivity block is reopened by a run that
                // deliberately includes its date, because closed_reason is cleared above.
                updated
            }
            None => {
                sqlx::query_as::<_, Slot>(
                    "INSERT INTO booking_slots
                        (resource_id, starts_at, ends_at, capacity, price_cents, label)
                     VALUES ($1,$2,$3,$4,$5,$6)
                     ON CONFLICT (resource_id, starts_at) DO UPDATE
                        SET price_cents = EXCLUDED.price_cents,
                            capacity    = EXCLUDED.capacity,
                            ends_at     = EXCLUDED.ends_at
                     RETURNING *",
                )
                .bind(res.id)
                .bind(start)
                .bind(start + duration)
                .bind(g.capacity.unwrap_or(res.capacity).max(1))
                .bind(price.max(0))
                .bind(g.label.clone().unwrap_or_default())
                .fetch_optional(&mut *tx)
                .await?
            }
        };
        created.extend(slot);
    }
    tx.commit().await?;
    Ok(created)
}

pub async fn update_slot(
    conn: &mut PgConnection,
    tenant: Uuid,
    slot_id: Uuid,
    u: UpdateSlot,
) -> Result<Slot> {
    if let Some(s) = &u.status {
        if s != "open" && s != "closed" {
            return Err(BookingError::invalid("status must be open or closed"));
        }
    }
    sqlx::query_as::<_, Slot>(
        "UPDATE booking_slots s SET
            price_cents = COALESCE($3, s.price_cents),
            capacity    = COALESCE($4, s.capacity),
            label       = COALESCE($5, s.label),
            status      = COALESCE($6, s.status)
         FROM booking_resources r JOIN booking_calendars c ON c.id = r.calendar_id
         WHERE s.id = $1 AND r.id = s.resource_id AND c.tenant_id = $2
         RETURNING s.*",
    )
    .bind(slot_id)
    .bind(tenant)
    .bind(u.price_cents)
    .bind(u.capacity)
    .bind(u.label)
    .bind(u.status)
    .fetch_optional(conn)
    .await?
    .ok_or(BookingError::NotFound("slot"))
}

pub async fn create_bundle(
    conn: &mut PgConnection,
    calendar_id: Uuid,
    b: NewBundle,
) -> Result<Bundle> {
    if b.kind != "percent_off" && b.kind != "fixed_price" {
        return Err(BookingError::invalid(
            "kind must be percent_off or fixed_price",
        ));
    }
    if b.min_items < 1 {
        return Err(BookingError::invalid("min_items must be at least 1"));
    }
    Ok(sqlx::query_as::<_, Bundle>(
        "INSERT INTO booking_bundles (calendar_id, name, kind, min_items, percent_off, fixed_price_cents, eligible_resource_ids, required_resource_ids)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)
         RETURNING id, calendar_id, name, kind, min_items, percent_off, fixed_price_cents, eligible_resource_ids, required_resource_ids, active",
    )
    .bind(calendar_id)
    .bind(b.name.trim())
    .bind(&b.kind)
    .bind(b.min_items)
    .bind(b.percent_off.unwrap_or(0).clamp(0, 100))
    .bind(b.fixed_price_cents.unwrap_or(0).max(0))
    .bind(&b.eligible_resource_ids)
    .bind(&b.required_resource_ids)
    .fetch_one(conn)
    .await?)
}

// ---------- availability ----------

/// How much choice a buyer is shown, and how far ahead they may look.
///
/// From `settings.limits`: `max_options` caps the whole page, `max_per_day` caps one day, and
/// `max_advance_days` stops a visitor booking six months out. All optional, all enforced HERE rather
/// than in the page — a limit that only exists in the browser is not a limit, it is a suggestion.
#[derive(Debug, Default, Clone, Copy)]
pub struct AvailabilityLimits {
    pub max_options: Option<u32>,
    pub max_per_day: Option<u32>,
    pub max_advance_days: Option<i64>,
}

impl AvailabilityLimits {
    fn of(cal: &Calendar) -> Self {
        let l = cal.settings.get("limits");
        let num = |k: &str| l.and_then(|v| v.get(k)).and_then(|v| v.as_u64());
        Self {
            max_options: num("max_options").map(|v| v as u32).filter(|v| *v > 0),
            max_per_day: num("max_per_day").map(|v| v as u32).filter(|v| *v > 0),
            max_advance_days: num("max_advance_days").map(|v| v as i64).filter(|v| *v > 0),
        }
    }
}

/// Cut the list down to what the owner allows a buyer to see: never past their horizon, never more
/// than N a day, never more than N in total. Order is preserved, so the SOONEST positions survive.
fn apply_limits(
    mut out: Vec<Opening>,
    limits: AvailabilityLimits,
    tz: chrono_tz::Tz,
) -> Vec<Opening> {
    if let Some(days) = limits.max_advance_days {
        let horizon = Utc::now() + Duration::days(days);
        out.retain(|o| o.starts_at <= horizon);
    }
    // PER POSITION PER DAY, not per calendar per day (ZaarHub, 2026-11-23). A newsletter sells four
    // positions in the SAME issue: Email TOP, MIDDLE, FOOTER and a sponsored post, all on one date.
    // Counting one tile per DATE hid three of the four — a "1 per day" cap silently made three
    // products unsellable. The cap now counts each position separately, which is what an owner means
    // by "one option a day": one of each thing they sell.
    let mut per_position_day: std::collections::HashMap<(uuid::Uuid, chrono::NaiveDate), u32> =
        std::collections::HashMap::new();
    let mut kept = Vec::with_capacity(out.len());
    // `out` arrives ordered by start time, so the nearest positions are the ones that survive a cap.
    //
    // ONLY BOOKABLE OPENINGS COUNT AGAINST THE CAP (measured 2026-11-23). A taken position stays on an
    // ad grid on purpose — it reads as "Booked" and advertises the demand — but it must not consume a
    // place in the owner's window: with a cap of 5, one sale left the buyer looking at four things they
    // could buy, and the sixth date never appeared. Sold openings are still returned, they simply do
    // not count, so every sale lets one more date into view.
    let mut kept_open = 0u32;
    for o in out {
        let bookable = o.remaining > 0;
        if bookable {
            if let Some(cap) = limits.max_options {
                if kept_open >= cap {
                    // The window is full of bookable openings: this one is beyond it. Taken ones are
                    // still kept, so the page can show them as booked.
                    continue;
                }
            }
            let day = o.starts_at.with_timezone(&tz).date_naive();
            let used = per_position_day.entry((o.resource_id, day)).or_insert(0);
            if let Some(cap) = limits.max_per_day {
                if *used >= cap {
                    continue;
                }
            }
            *used += 1;
            kept_open += 1;
        }
        kept.push(o);
    }
    kept
}

/// The widest window the availability builder accepts, as a date range from today. Callers that want
/// "everything" must use this: asking for 365 days is refused, and a refused range answers an error
/// object that reads like an empty calendar.
pub fn widest_window() -> (NaiveDate, NaiveDate) {
    let today = Utc::now().date_naive();
    (today, today + Duration::days(MAX_RANGE_DAYS))
}

/// What the buyer will actually be shown, in plain words — computed from live inventory.
///
/// Three interlocking numbers with no explanation is how a scarcity setting becomes a mystery. This
/// answers the question an owner actually asks: with these limits and this much open inventory, what
/// does a visitor get? It is computed from the SAME availability builder the public page uses, so a
/// stale sentence is impossible, and the console can print it instead of describing the settings.
#[derive(Debug, serde::Serialize)]
pub struct BuyerView {
    pub open_total: usize,
    pub shown: usize,
    pub horizon_days: Option<i64>,
    pub per_day_per_position: Option<u32>,
    pub unlimited: bool,
    pub sentence: String,
}

pub async fn buyer_view(
    conn: &mut PgConnection,
    cal: &Calendar,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<BuyerView> {
    let limits = AvailabilityLimits::of(cal);
    let all = availability(conn, cal, from, to, false).await?;
    let shown = availability(conn, cal, from, to, true).await?;
    let unlimited = limits.max_options.is_none()
        && limits.max_per_day.is_none()
        && limits.max_advance_days.is_none();
    let shown_bookable = shown.iter().filter(|o| o.remaining > 0).count();
    let mut parts: Vec<String> = Vec::new();
    if limits.max_options.is_some()
        || limits.max_per_day.is_some()
        || limits.max_advance_days.is_some()
    {
        parts.push(format!(
            "Buyer sees {} of {} open position{}",
            shown_bookable,
            all.len(),
            if all.len() == 1 { "" } else { "s" }
        ));
        if let Some(d) = limits.max_advance_days {
            parts.push(format!(
                "within the next {d} day{}",
                if d == 1 { "" } else { "s" }
            ));
        } else {
            parts.push("with no date horizon".to_string());
        }
        if let Some(p) = limits.max_per_day {
            parts.push(format!("at most {p} per date for each position"));
        }
    } else {
        parts.push(format!(
            "Buyer sees all {} open position{}. No limits are set, so nothing is being held back.",
            all.len(),
            if all.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(BuyerView {
        open_total: all.len(),
        shown: shown_bookable,
        horizon_days: limits.max_advance_days,
        per_day_per_position: limits.max_per_day,
        unlimited,
        sentence: format!("{}.", parts.join(" ")),
    })
}

pub async fn availability(
    conn: &mut PgConnection,
    cal: &Calendar,
    from: NaiveDate,
    to: NaiveDate,
    enforce_limits: bool,
) -> Result<Vec<Opening>> {
    if to < from || (to - from).num_days() > MAX_RANGE_DAYS {
        return Err(BookingError::invalid(format!(
            "range must be 0-{MAX_RANGE_DAYS} days"
        )));
    }
    let tz = tz_of(cal)?;
    let to_excl = to
        .succ_opt()
        .ok_or_else(|| BookingError::invalid("bad date"))?;
    let (range_start, range_end) = match (day_start(tz, from), day_start(tz, to_excl)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Err(BookingError::invalid("bad date")),
    };
    let now = Utc::now();
    let mut out = Vec::new();

    match cal.kind {
        CalendarKind::Inventory => {
            let sql = "SELECT s.id, s.resource_id, r.name, s.label, s.starts_at, s.ends_at, s.price_cents,
                        r.compare_at_price_cents,
                        (s.capacity - COALESCE((SELECT SUM(b.quantity) FROM booking_bookings b
                                                WHERE b.slot_id = s.id AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now()))), 0))::BIGINT
                 FROM booking_slots s JOIN booking_resources r ON r.id = s.resource_id
                 WHERE r.calendar_id = $1 AND r.active AND s.status = 'open'
                   AND s.starts_at >= $2 AND s.starts_at < $3 AND s.starts_at > now()
                 ORDER BY s.starts_at, r.sort_order, r.name";
            type Row = (
                Uuid,
                Uuid,
                String,
                String,
                DateTime<Utc>,
                DateTime<Utc>,
                i64,
                Option<i64>,
                i64,
            );
            let rows: Vec<Row> = sqlx::query_as(&sql)
                .bind(cal.id)
                .bind(range_start)
                .bind(range_end)
                .fetch_all(conn)
                .await?;
            for (
                slot_id,
                resource_id,
                resource_name,
                label,
                starts_at,
                ends_at,
                price_cents,
                compare_at,
                remaining,
            ) in rows
            {
                out.push(Opening {
                    resource_id,
                    resource_name,
                    slot_id: Some(slot_id),
                    label,
                    starts_at,
                    ends_at,
                    price_cents,
                    // The rule, in one place: a "was" price only exists when it is HIGHER. Equal or
                    // lower is not a saving, and showing it would be a false claim.
                    compare_at_price_cents: compare_at.filter(|c| *c > price_cents),
                    remaining: remaining.max(0),
                });
            }
        }
        CalendarKind::Appointment => {
            for res in list_resources(&mut *conn, cal.id, true).await? {
                let rules = rules_for(&mut *conn, res.id).await?;
                let sql = "SELECT b.starts_at, b.ends_at, b.quantity FROM booking_bookings b
                     WHERE b.resource_id = $1 AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now())) AND b.starts_at < $3 AND b.ends_at > $2";
                let busy: Vec<(DateTime<Utc>, DateTime<Utc>, i32)> = sqlx::query_as(&sql)
                    .bind(res.id)
                    .bind(range_start - Duration::days(1))
                    .bind(range_end + Duration::days(1))
                    .fetch_all(&mut *conn)
                    .await?;
                let earliest = now + Duration::minutes(res.min_notice_min as i64);
                for day in from.iter_days().take_while(|d| *d <= to) {
                    for (s, e) in appointment_windows(&res, &rules, tz, day) {
                        let remaining =
                            res.capacity as i64 - seats_used(&busy, s, e, res.buffer_min);
                        if s >= earliest && remaining > 0 {
                            out.push(Opening {
                                resource_id: res.id,
                                resource_name: res.name.clone(),
                                slot_id: None,
                                label: String::new(),
                                starts_at: s,
                                ends_at: e,
                                price_cents: res.price_cents,
                                compare_at_price_cents: res
                                    .compare_at_price_cents
                                    .filter(|c| *c > res.price_cents),
                                remaining,
                            });
                        }
                    }
                }
            }
            out.sort_by_key(|o| o.starts_at);
        }
        CalendarKind::DateRange => {
            let days = day_starts(tz, from, to_excl.succ_opt().unwrap_or(to_excl));
            for res in list_resources(&mut *conn, cal.id, true).await? {
                let sql = "SELECT d, COALESCE((SELECT SUM(b.quantity) FROM booking_bookings b
                                         WHERE b.resource_id = $1 AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now()))
                                           AND b.starts_at <= d AND b.ends_at > d), 0)::BIGINT
                     FROM unnest($2::timestamptz[]) AS d ORDER BY d";
                let used: Vec<(DateTime<Utc>, i64)> = sqlx::query_as(&sql)
                    .bind(res.id)
                    .bind(&days[..days.len().saturating_sub(1)])
                    .fetch_all(&mut *conn)
                    .await?;
                for (i, (d, u)) in used.into_iter().enumerate() {
                    let end = days.get(i + 1).copied().unwrap_or(d + Duration::days(1));
                    if end <= now {
                        continue;
                    }
                    out.push(Opening {
                        resource_id: res.id,
                        resource_name: res.name.clone(),
                        slot_id: None,
                        label: String::new(),
                        starts_at: d,
                        ends_at: end,
                        price_cents: res.price_cents,
                        compare_at_price_cents: res
                            .compare_at_price_cents
                            .filter(|c| *c > res.price_cents),
                        remaining: (res.capacity as i64 - u).max(0),
                    });
                }
            }
            out.sort_by_key(|o| o.starts_at);
        }
    }
    if enforce_limits {
        out = apply_limits(out, AvailabilityLimits::of(cal), tz);
    }
    Ok(out)
}

// ---------- quoting and ordering ----------

/// Turn a customer's request into concrete times and a price. Does not check capacity.
async fn resolve_item(
    conn: &mut PgConnection,
    cal: &Calendar,
    tz: Tz,
    input: &OrderItemInput,
) -> Result<ResolvedItem> {
    let resource = sqlx::query_as::<_, Resource>(
        "SELECT * FROM booking_resources WHERE id = $1 AND calendar_id = $2 AND active",
    )
    .bind(input.resource_id)
    .bind(cal.id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(BookingError::NotFound("resource"))?;
    let quantity = input.quantity.unwrap_or(1);
    if !(1..=1000).contains(&quantity) {
        return Err(BookingError::invalid("quantity must be 1-1000"));
    }
    let now = Utc::now();
    let (slot_id, starts_at, ends_at, unit_price) = match cal.kind {
        CalendarKind::Inventory => {
            let slot_id = input
                .slot_id
                .ok_or_else(|| BookingError::invalid("slot_id is required"))?;
            let slot = sqlx::query_as::<_, Slot>(
                "SELECT * FROM booking_slots WHERE id = $1 AND resource_id = $2",
            )
            .bind(slot_id)
            .bind(resource.id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or(BookingError::NotFound("slot"))?;
            if slot.status != "open" || slot.starts_at <= now {
                return Err(BookingError::conflict(format!(
                    "{} on {} is no longer bookable",
                    resource.name,
                    slot.starts_at.with_timezone(&tz).date_naive()
                )));
            }
            (
                Some(slot.id),
                slot.starts_at,
                slot.ends_at,
                slot.price_cents,
            )
        }
        CalendarKind::Appointment => {
            let start = input
                .starts_at
                .ok_or_else(|| BookingError::invalid("starts_at is required"))?;
            if start < now + Duration::minutes(resource.min_notice_min as i64) {
                return Err(BookingError::invalid("that time is too soon to book"));
            }
            let rules = rules_for(&mut *conn, resource.id).await?;
            let day = start.with_timezone(&tz).date_naive();
            let end = appointment_windows(&resource, &rules, tz, day)
                .into_iter()
                .find(|(s, _)| *s == start)
                .map(|(_, e)| e)
                .ok_or_else(|| BookingError::invalid("that time is not offered"))?;
            (None, start, end, resource.price_cents)
        }
        CalendarKind::DateRange => {
            let (Some(from), Some(to)) = (input.start_date, input.end_date) else {
                return Err(BookingError::invalid(
                    "start_date and end_date are required",
                ));
            };
            let days = (to - from).num_days();
            if !(1..=366).contains(&days) {
                return Err(BookingError::invalid(
                    "date range must be 1-366 days (end_date is exclusive)",
                ));
            }
            let (Some(start), Some(end)) = (day_start(tz, from), day_start(tz, to)) else {
                return Err(BookingError::invalid("bad date"));
            };
            if end <= now {
                return Err(BookingError::invalid("those dates have passed"));
            }
            (None, start, end, resource.price_cents * days)
        }
    };
    Ok(ResolvedItem {
        resource,
        slot_id,
        starts_at,
        ends_at,
        quantity,
        price_cents: unit_price * quantity as i64,
        meta: input.meta.clone().unwrap_or_else(|| json!({})),
    })
}

/// Fail with Conflict if adding `quantity` would oversell. Caller must hold the resource row lock.
async fn check_capacity(
    conn: &mut PgConnection,
    kind: CalendarKind,
    tz: Tz,
    resource: &Resource,
    slot_id: Option<Uuid>,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    quantity: i32,
) -> Result<()> {
    let (capacity, used): (i64, i64) = match kind {
        CalendarKind::Inventory => {
            let sql = "SELECT s.capacity::BIGINT, COALESCE((SELECT SUM(b.quantity) FROM booking_bookings b
                                                      WHERE b.slot_id = s.id AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now()))), 0)::BIGINT
                 FROM booking_slots s WHERE s.id = $1 AND s.status = 'open'";
            sqlx::query_as(&sql)
                .bind(slot_id)
                .fetch_optional(&mut *conn)
                .await?
                .ok_or_else(|| BookingError::conflict("slot is closed"))?
        }
        CalendarKind::Appointment => {
            let pad = Duration::minutes(resource.buffer_min.max(0) as i64);
            let sql = "SELECT COALESCE(SUM(b.quantity), 0)::BIGINT FROM booking_bookings b
                 WHERE b.resource_id = $1 AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now())) AND b.starts_at < $3 AND b.ends_at > $2";
            let (used,): (i64,) = sqlx::query_as(&sql)
                .bind(resource.id)
                .bind(starts_at - pad)
                .bind(ends_at + pad)
                .fetch_one(&mut *conn)
                .await?;
            (resource.capacity as i64, used)
        }
        CalendarKind::DateRange => {
            let days = day_starts(
                tz,
                starts_at.with_timezone(&tz).date_naive(),
                ends_at.with_timezone(&tz).date_naive(),
            );
            let sql = "SELECT COALESCE(MAX(x.used), 0)::BIGINT FROM (
                   SELECT (SELECT COALESCE(SUM(b.quantity), 0) FROM booking_bookings b
                           WHERE b.resource_id = $1 AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now())) AND b.starts_at <= d AND b.ends_at > d) AS used
                   FROM unnest($2::timestamptz[]) AS d) x";
            let (used,): (i64,) = sqlx::query_as(&sql)
                .bind(resource.id)
                .bind(&days)
                .fetch_one(&mut *conn)
                .await?;
            (resource.capacity as i64, used)
        }
    };
    if used + quantity as i64 > capacity {
        return Err(BookingError::conflict(format!(
            "{} on {} is sold out",
            resource.name,
            starts_at.with_timezone(&tz).format("%b %-d, %Y")
        )));
    }
    Ok(())
}

async fn lock_resources(conn: &mut PgConnection, mut ids: Vec<Uuid>) -> Result<()> {
    ids.sort();
    ids.dedup();
    sqlx::query("SELECT id FROM booking_resources WHERE id = ANY($1) ORDER BY id FOR UPDATE")
        .bind(&ids)
        .execute(conn)
        .await?;
    Ok(())
}

fn price(items: &[ResolvedItem], bundles: &[Bundle]) -> Quote {
    let lines: Vec<LineItem> = items
        .iter()
        .map(|i| LineItem {
            resource_id: i.resource.id,
            price_cents: i.price_cents,
        })
        .collect();
    pricing::quote(&lines, bundles)
}

fn validate_items(items: &[OrderItemInput]) -> Result<()> {
    if items.is_empty() || items.len() > MAX_ITEMS {
        return Err(BookingError::invalid("an order needs 1-{MAX_ITEMS} items"));
    }
    Ok(())
}

pub async fn quote(
    conn: &mut PgConnection,
    cal: &Calendar,
    items: &[OrderItemInput],
) -> Result<Quote> {
    validate_items(items)?;
    let tz = tz_of(cal)?;
    let mut resolved = Vec::with_capacity(items.len());
    for i in items {
        resolved.push(resolve_item(&mut *conn, cal, tz, i).await?);
    }
    let bundles = list_bundles(conn, cal.id, true).await?;
    Ok(price(&resolved, &bundles))
}

/// Create a pending order and hold its slots for `calendar.hold_minutes`.
/// All-or-nothing: if any item is unavailable nothing is held.
/// DATE EXCLUSIVITY — the same inventory must never be sold twice.
///
/// Four positions are sold per issue (Email TOP / MIDDLE / FOOTER / Sponsored Post) and a "Full
/// Takeover" sells all four in one issue, exclusive. With positions independent, the takeover could be
/// sold while all four individual slots stayed on sale. This is the one rule that closes that, and it
/// works in BOTH directions:
///
///   * booking an exclusive position closes every other position on that DATE;
///   * booking any other position closes the exclusive position on that DATE.
///
/// Two independent positions on the same date (TOP and FOOTER, say) are untouched: they are different
/// products on one issue, which is the normal case.
///
/// Returns the number of slots closed so the caller can report it.
async fn apply_date_exclusivity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_id: Uuid,
    resource_id: Uuid,
    exclusive: bool,
    date: chrono::NaiveDate,
    tz: &str,
) -> Result<usize> {
    // 1) REFUSE A CONFLICT BEFORE WRITING. If this date already carries a live booking on a position
    // that conflicts — either this one is exclusive, or the booked one is exclusive — the sale is
    // stopped with a sentence an operator can act on, rather than sold and then discovered.
    let conflict: Option<String> = sqlx::query_scalar(
        "SELECT r.name
           FROM booking_bookings b
           JOIN booking_resources r ON r.id = b.resource_id
          WHERE r.calendar_id = $1
            AND b.resource_id <> $2
            AND ($3::bool OR r.exclusive)
            AND (b.starts_at AT TIME ZONE $4)::date = $5
            AND (b.status = 'confirmed' OR (b.status = 'held' AND b.hold_expires_at > now()))
          LIMIT 1",
    )
    .bind(calendar_id)
    .bind(resource_id)
    .bind(exclusive)
    .bind(tz)
    .bind(date)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(name) = conflict {
        return Err(BookingError::invalid(format!(
            "That date is already sold as \"{name}\". Pick another date."
        )));
    }

    // 2) CLOSE WHAT CAN NO LONGER BE SOLD: every other position's open, unbooked slot on this date.
    // A slot already carrying a live booking is never touched — a buyer agreed to a number.
    let closed = sqlx::query(
        "UPDATE booking_slots s
            SET status = 'closed',
                closed_reason = 'exclusive:' || (SELECT name FROM booking_resources WHERE id = $2)
           FROM booking_resources r
          WHERE r.id = s.resource_id
            AND r.calendar_id = $1
            AND s.resource_id <> $2
            AND s.status = 'open'
            AND ($3::bool OR r.exclusive)
            AND (s.starts_at AT TIME ZONE $4)::date = $5
            AND NOT EXISTS (
                  SELECT 1 FROM booking_bookings b
                   WHERE b.slot_id = s.id
                     AND (b.status = 'confirmed'
                          OR (b.status = 'held' AND b.hold_expires_at > now())))",
    )
    .bind(calendar_id)
    .bind(resource_id)
    .bind(exclusive)
    .bind(tz)
    .bind(date)
    .execute(&mut **tx)
    .await?
    .rows_affected() as usize;
    Ok(closed)
}

/// REOPEN what an exclusivity block closed, when the booking that caused it goes away.
///
/// A blocked date that stays blocked after a cancellation is lost inventory that nobody can find: the
/// slot is closed and the reason points at a booking that no longer exists. Only slots closed BY this
/// rule (never by hand) come back, and only when nothing else on the date is live.
async fn release_date_exclusivity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    calendar_id: Uuid,
    date: chrono::NaiveDate,
    tz: &str,
) -> Result<u64> {
    let freed = sqlx::query(
        "UPDATE booking_slots s
            SET status = 'open', closed_reason = NULL
           FROM booking_resources r
          WHERE r.id = s.resource_id
            AND r.calendar_id = $1
            AND s.closed_reason LIKE 'exclusive:%'
            AND (s.starts_at AT TIME ZONE $2)::date = $3
            AND NOT EXISTS (
                  SELECT 1 FROM booking_bookings b
                   JOIN booking_resources br ON br.id = b.resource_id
                   WHERE br.calendar_id = $1
                     AND (b.starts_at AT TIME ZONE $2)::date = $3
                     AND (b.status = 'confirmed'
                          OR (b.status = 'held' AND b.hold_expires_at > now())))",
    )
    .bind(calendar_id)
    .bind(tz)
    .bind(date)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    Ok(freed)
}

pub async fn create_order(
    pool: &PgPool,
    cal: &Calendar,
    input: NewOrder,
) -> Result<OrderWithBookings> {
    validate_items(&input.items)?;
    let name = input.customer_name.trim();
    let email = input.customer_email.trim().to_lowercase();
    if name.is_empty() || !email.contains('@') {
        return Err(BookingError::invalid(
            "customer_name and a valid customer_email are required",
        ));
    }
    // MANDATORY ANSWERS ARE ENFORCED HERE (2026-11-23). The form may ask for a company name and mark
    // it required; an order without it is refused with the field's own label, not a generic 400 — and
    // refused on the SERVER, because a browser check is advice, not a rule.
    let answers = input
        .answers
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    let required: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, label FROM booking_form_fields \
          WHERE calendar_id = $1 AND is_active AND required ORDER BY sort_order",
    )
    .bind(cal.id)
    .fetch_all(&mut *pool.acquire().await?)
    .await?;
    for (key, label) in required {
        let filled = answers
            .get(&key)
            .map(|v| match v {
                serde_json::Value::String(s) => !s.trim().is_empty(),
                serde_json::Value::Null => false,
                _ => true,
            })
            .unwrap_or(false);
        if !filled {
            return Err(BookingError::invalid(format!(
                "{label} is required for this booking."
            )));
        }
    }

    let tz = tz_of(cal)?;
    let mut tx = pool.begin().await?;
    lock_resources(&mut tx, input.items.iter().map(|i| i.resource_id).collect()).await?;

    let mut resolved = Vec::with_capacity(input.items.len());
    for i in &input.items {
        resolved.push(resolve_item(&mut tx, cal, tz, i).await?);
    }
    let bundles = list_bundles(&mut tx, cal.id, true).await?;
    let q = price(&resolved, &bundles);
    let hold_until = Utc::now() + Duration::minutes(cal.hold_minutes as i64);

    let order = sqlx::query_as::<_, Order>(
        "INSERT INTO booking_orders
           (tenant_id, calendar_id, contact_id, customer_name, customer_email, subtotal_cents, discount_cents, total_cents, bundle_id, hold_expires_at, notes, phone, sms_opt_in, answers)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING *",
    )
    .bind(cal.tenant_id)
    .bind(cal.id)
    .bind(input.contact_id)
    .bind(name)
    .bind(&email)
    .bind(q.subtotal_cents)
    .bind(q.discount_cents)
    .bind(q.total_cents)
    .bind(q.bundle_id)
    .bind(hold_until)
    .bind(input.notes.clone().unwrap_or_default())
    .bind(input.phone.as_deref().map(str::trim).filter(|p| !p.is_empty()))
    .bind(input.sms_opt_in.unwrap_or(false))
    // The answers, stored beside the order they belong to (validated above, never trusted from the
    // browser alone).
    .bind(&answers)
    .fetch_one(&mut *tx)
    .await?;

    let mut bookings = Vec::with_capacity(resolved.len());
    for item in &resolved {
        // Checked one at a time so two items in the same order cannot oversell a slot together.
        check_capacity(
            &mut tx,
            cal.kind,
            tz,
            &item.resource,
            item.slot_id,
            item.starts_at,
            item.ends_at,
            item.quantity,
        )
        .await?;
        // One date, one exclusivity rule: refuse a date already committed to a conflicting position,
        // then close what this booking has just made unsellable.
        let local_date = item.starts_at.with_timezone(&tz).date_naive();
        apply_date_exclusivity(
            &mut tx,
            cal.id,
            item.resource.id,
            item.resource.exclusive,
            local_date,
            tz.name(),
        )
        .await?;
        let b = sqlx::query_as::<_, Booking>(
            "INSERT INTO booking_bookings (order_id, resource_id, slot_id, starts_at, ends_at, quantity, price_cents, hold_expires_at, meta)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *",
        )
        .bind(order.id)
        .bind(item.resource.id)
        .bind(item.slot_id)
        .bind(item.starts_at)
        .bind(item.ends_at)
        .bind(item.quantity)
        .bind(item.price_cents)
        .bind(hold_until)
        .bind(&item.meta)
        .fetch_one(&mut *tx)
        .await?;
        bookings.push(b);
    }
    tx.commit().await?;
    Ok(OrderWithBookings { order, bookings })
}

async fn bookings_of(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<Booking>> {
    Ok(sqlx::query_as::<_, Booking>(
        "SELECT * FROM booking_bookings WHERE order_id = $1 ORDER BY starts_at, created_at",
    )
    .bind(order_id)
    .fetch_all(conn)
    .await?)
}

/// `tenant = None` is for trusted callers (public order page, payment webhook).
pub async fn get_order(
    conn: &mut PgConnection,
    order_id: Uuid,
    tenant: Option<Uuid>,
) -> Result<OrderWithBookings> {
    let order = sqlx::query_as::<_, Order>(
        "SELECT * FROM booking_orders WHERE id = $1 AND ($2::uuid IS NULL OR tenant_id = $2)",
    )
    .bind(order_id)
    .bind(tenant)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or(BookingError::NotFound("order"))?;
    let bookings = bookings_of(conn, order.id).await?;
    Ok(OrderWithBookings { order, bookings })
}

pub async fn list_orders(conn: &mut PgConnection, calendar_id: Uuid) -> Result<Vec<Order>> {
    Ok(sqlx::query_as::<_, Order>(
        "SELECT * FROM booking_orders WHERE calendar_id = $1 ORDER BY created_at DESC LIMIT 500",
    )
    .bind(calendar_id)
    .fetch_all(conn)
    .await?)
}

/// Call after payment succeeds (or straight away for free bookings). Idempotent.
/// If the hold ran out, the slots are re-checked: still free -> confirmed, otherwise Conflict
/// (the caller should then refund).
pub async fn confirm_order(
    pool: &PgPool,
    order_id: Uuid,
    tenant: Option<Uuid>,
    payment_ref: Option<String>,
) -> Result<OrderWithBookings> {
    let mut tx = pool.begin().await?;
    let order = sqlx::query_as::<_, Order>(
        "SELECT * FROM booking_orders WHERE id = $1 AND ($2::uuid IS NULL OR tenant_id = $2) FOR UPDATE",
    )
    .bind(order_id)
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BookingError::NotFound("order"))?;

    match order.status.as_str() {
        "confirmed" => {
            let bookings = bookings_of(&mut tx, order.id).await?;
            return Ok(OrderWithBookings { order, bookings });
        }
        "cancelled" => return Err(BookingError::conflict("order was cancelled")),
        _ => {}
    }

    if order.hold_expires_at <= Utc::now() {
        let cal = calendar_by_id(&mut tx, order.calendar_id).await?;
        let tz = tz_of(&cal)?;
        let held = bookings_of(&mut tx, order.id).await?;
        lock_resources(&mut tx, held.iter().map(|b| b.resource_id).collect()).await?;
        for b in &held {
            let res =
                sqlx::query_as::<_, Resource>("SELECT * FROM booking_resources WHERE id = $1")
                    .bind(b.resource_id)
                    .fetch_one(&mut *tx)
                    .await?;
            check_capacity(
                &mut tx,
                cal.kind,
                tz,
                &res,
                b.slot_id,
                b.starts_at,
                b.ends_at,
                b.quantity,
            )
            .await
            .map_err(|_| {
                BookingError::conflict("hold expired and the slot was taken by someone else")
            })?;
            sqlx::query("UPDATE booking_bookings SET status = 'confirmed' WHERE id = $1")
                .bind(b.id)
                .execute(&mut *tx)
                .await?;
        }
    }

    sqlx::query(
        "UPDATE booking_bookings SET status = 'confirmed' WHERE order_id = $1 AND status = 'held'",
    )
    .bind(order.id)
    .execute(&mut *tx)
    .await?;
    let order = sqlx::query_as::<_, Order>(
        "UPDATE booking_orders SET status = 'confirmed', payment_ref = COALESCE($2, payment_ref) WHERE id = $1 RETURNING *",
    )
    .bind(order.id)
    .bind(payment_ref)
    .fetch_one(&mut *tx)
    .await?;
    let bookings = bookings_of(&mut tx, order.id).await?;
    tx.commit().await?;
    Ok(OrderWithBookings { order, bookings })
}

/// Cancel an order and free its slots immediately.
pub async fn cancel_order(
    pool: &PgPool,
    order_id: Uuid,
    tenant: Option<Uuid>,
) -> Result<OrderWithBookings> {
    let mut tx = pool.begin().await?;
    let order = sqlx::query_as::<_, Order>(
        "UPDATE booking_orders SET status = 'cancelled' WHERE id = $1 AND ($2::uuid IS NULL OR tenant_id = $2) RETURNING *",
    )
    .bind(order_id)
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BookingError::NotFound("order"))?;
    // A DATE BLOCKED BY A BOOKING THAT NO LONGER EXISTS IS LOST INVENTORY (2026-11-23). Cancelling an
    // order releases the dates its exclusivity rule closed, so a takeover that falls through does not
    // leave the whole issue unsellable.
    let dates: Vec<(chrono::DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT b.starts_at, coalesce(c.timezone, 'UTC') FROM booking_bookings b
           JOIN booking_calendars c ON c.id = $2
          WHERE b.order_id = $1",
    )
    .bind(order.id)
    .bind(order.calendar_id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("UPDATE booking_bookings SET status = 'cancelled' WHERE order_id = $1")
        .bind(order.id)
        .execute(&mut *tx)
        .await?;
    for (starts, tzname) in dates {
        let local = match tzname.parse::<chrono_tz::Tz>() {
            Ok(z) => starts.with_timezone(&z).date_naive(),
            Err(_) => starts.date_naive(),
        };
        let _ = release_date_exclusivity(&mut tx, order.calendar_id, local, &tzname).await;
    }
    let bookings = bookings_of(&mut tx, order.id).await?;
    tx.commit().await?;
    Ok(OrderWithBookings { order, bookings })
}

// ---------- ad creative ----------

/// Advertiser submits copy/artwork for one booking. Stored in `meta.creative`, status "pending".
pub async fn submit_creative(
    conn: &mut PgConnection,
    order_id: Uuid,
    booking_id: Uuid,
    creative: Value,
) -> Result<Booking> {
    sqlx::query_as::<_, Booking>(
        "UPDATE booking_bookings
         SET meta = meta || jsonb_build_object('creative', $3::jsonb, 'creative_status', 'pending')
         WHERE id = $1 AND order_id = $2 AND status <> 'cancelled' RETURNING *",
    )
    .bind(booking_id)
    .bind(order_id)
    .bind(creative)
    .fetch_optional(conn)
    .await?
    .ok_or(BookingError::NotFound("booking"))
}

pub async fn review_creative(
    conn: &mut PgConnection,
    tenant: Uuid,
    booking_id: Uuid,
    r: CreativeReview,
) -> Result<Booking> {
    let status = if r.approved { "approved" } else { "rejected" };
    sqlx::query_as::<_, Booking>(
        "UPDATE booking_bookings b
         SET meta = b.meta || jsonb_build_object('creative_status', $3::text, 'creative_note', $4::text)
         FROM booking_orders o
         WHERE b.id = $1 AND o.id = b.order_id AND o.tenant_id = $2 RETURNING b.*",
    )
    .bind(booking_id)
    .bind(tenant)
    .bind(status)
    .bind(r.note.unwrap_or_default())
    .fetch_optional(conn)
    .await?
    .ok_or(BookingError::NotFound("booking"))
}

// ============================================================================================
// Manage link — the customer reschedules, cancels or rebooks without an account
// --------------------------------------------------------------------------------------------
// The token in the URL IS the authority, so every function here starts by resolving the order from
// that token and never from a caller-supplied id. A booking is moved inside a transaction that first
// frees its own capacity, so the capacity check sees the world as it would be AFTER the move — which
// is the only way "can I have this slot instead?" can be answered honestly.
// ============================================================================================

/// A booking plus the names a customer needs to recognise it.
#[derive(Debug, serde::Serialize)]
pub struct ManageBooking {
    pub id: Uuid,
    pub resource_name: String,
    pub label: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub quantity: i32,
    pub price_cents: i64,
    pub status: String,
    pub creative_status: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ManageView {
    pub order_id: Uuid,
    pub calendar_name: String,
    pub calendar_slug: String,
    pub kind: String,
    pub timezone: String,
    pub currency: String,
    pub customer_name: String,
    pub customer_email: String,
    pub status: String,
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub total_cents: i64,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub hold_expires_at: DateTime<Utc>,
    pub bookings: Vec<ManageBooking>,
}

async fn order_by_token(conn: &mut PgConnection, token: &str) -> Result<Order> {
    sqlx::query_as::<_, Order>("SELECT * FROM booking_orders WHERE manage_token = $1")
        .bind(token)
        .fetch_optional(conn)
        .await?
        .ok_or(BookingError::NotFound("booking"))
}

pub async fn manage_view(pool: &PgPool, token: &str) -> Result<ManageView> {
    let mut conn = pool.acquire().await?;
    let order = order_by_token(&mut conn, token).await?;
    let cal = calendar_by_id(&mut conn, order.calendar_id).await?;
    let rows = sqlx::query_as::<_, (Uuid, String, String, DateTime<Utc>, DateTime<Utc>, i32, i64, String, Option<String>)>(
        "SELECT b.id, r.name, COALESCE(s.label, ''), b.starts_at, b.ends_at, b.quantity, b.price_cents,
                b.status, b.meta->>'creative_status'
           FROM booking_bookings b
           JOIN booking_resources r ON r.id = b.resource_id
           LEFT JOIN booking_slots s ON s.id = b.slot_id
          WHERE b.order_id = $1
          ORDER BY b.starts_at",
    )
    .bind(order.id)
    .fetch_all(&mut *conn)
    .await?;
    let bookings = rows
        .into_iter()
        .map(
            |(
                id,
                resource_name,
                label,
                starts_at,
                ends_at,
                quantity,
                price_cents,
                status,
                creative_status,
            )| {
                ManageBooking {
                    id,
                    resource_name,
                    label,
                    starts_at,
                    ends_at,
                    quantity,
                    price_cents,
                    status,
                    creative_status,
                }
            },
        )
        .collect();
    Ok(ManageView {
        order_id: order.id,
        calendar_name: cal.name,
        calendar_slug: cal.slug,
        kind: serde_json::to_value(&cal.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        timezone: cal.timezone,
        currency: cal.currency,
        customer_name: order.customer_name,
        customer_email: order.customer_email,
        status: order.status,
        subtotal_cents: order.subtotal_cents,
        discount_cents: order.discount_cents,
        total_cents: order.total_cents,
        notes: order.notes,
        created_at: order.created_at,
        hold_expires_at: order.hold_expires_at,
        bookings,
    })
}

/// Cancel by token. Frees every position the order holds.
pub async fn manage_cancel(pool: &PgPool, token: &str) -> Result<OrderWithBookings> {
    let mut conn = pool.acquire().await?;
    let order = order_by_token(&mut conn, token).await?;
    drop(conn);
    cancel_order(pool, order.id, None).await
}

/// Move ONE booking of a token's order to another position.
///
/// `slot_id` moves an inventory/date_range booking onto a dated slot; `starts_at` moves an
/// appointment (or a range, with `end_date`) to a new time. The move is refused with a conflict if
/// the target cannot take it, and nothing is changed in that case.
pub async fn manage_reschedule(
    pool: &PgPool,
    token: &str,
    booking_id: Uuid,
    slot_id: Option<Uuid>,
    starts_at: Option<DateTime<Utc>>,
    end_date: Option<chrono::NaiveDate>,
) -> Result<OrderWithBookings> {
    let mut tx = pool.begin().await?;
    let order = order_by_token(&mut tx, token).await?;
    if order.status == "cancelled" {
        return Err(BookingError::conflict("this booking was cancelled"));
    }
    let cal = calendar_by_id(&mut tx, order.calendar_id).await?;
    let tz = tz_of(&cal)?;

    let booking = sqlx::query_as::<_, Booking>(
        "SELECT * FROM booking_bookings WHERE id = $1 AND order_id = $2",
    )
    .bind(booking_id)
    .bind(order.id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(BookingError::NotFound("booking"))?;
    if booking.status == "cancelled" {
        return Err(BookingError::conflict("this booking was cancelled"));
    }

    let resource = sqlx::query_as::<_, Resource>("SELECT * FROM booking_resources WHERE id = $1")
        .bind(booking.resource_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(BookingError::NotFound("resource"))?;

    // Resolve the target position.
    let (new_slot, new_start, new_end) = if let Some(sid) = slot_id {
        let (starts, ends): (DateTime<Utc>, DateTime<Utc>) = sqlx::query_as(
            "SELECT starts_at, ends_at FROM booking_slots WHERE id = $1 AND resource_id = $2 AND status = 'open'",
        )
        .bind(sid)
        .bind(booking.resource_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| BookingError::conflict("that slot is no longer available"))?;
        (Some(sid), starts, ends)
    } else if let Some(s) = starts_at {
        let duration = booking.ends_at - booking.starts_at;
        (None, s, s + duration)
    } else if let Some(day) = end_date {
        let start = booking.starts_at;
        let end = chrono::DateTime::<Utc>::from_naive_utc_and_offset(
            day.and_hms_opt(0, 0, 0).unwrap_or_default(),
            Utc,
        );
        if end <= start {
            return Err(BookingError::invalid(
                "the end date must be after the start",
            ));
        }
        (None, start, end)
    } else {
        return Err(BookingError::invalid("nothing to move it to"));
    };

    // Free this booking's own capacity first, so the check below judges the world AFTER the move.
    sqlx::query("UPDATE booking_bookings SET status = 'cancelled' WHERE id = $1")
        .bind(booking.id)
        .execute(&mut *tx)
        .await?;

    lock_resources(&mut tx, vec![resource.id]).await?;
    if let Err(e) = check_capacity(
        &mut tx,
        cal.kind.clone(),
        tz,
        &resource,
        new_slot,
        new_start,
        new_end,
        booking.quantity,
    )
    .await
    {
        // rollback leaves the booking exactly as it was
        return Err(e);
    }

    let updated = sqlx::query_as::<_, Booking>(
        "UPDATE booking_bookings
            SET slot_id = $2, starts_at = $3, ends_at = $4, status = $5,
                hold_expires_at = GREATEST(hold_expires_at, now() + ($6::int * interval '1 minute'))
          WHERE id = $1
          RETURNING *",
    )
    .bind(booking.id)
    .bind(new_slot)
    .bind(new_start)
    .bind(new_end)
    .bind(&booking.status)
    .bind(cal.hold_minutes)
    .fetch_one(&mut *tx)
    .await?;
    let _ = updated;

    let bookings = bookings_of(&mut tx, order.id).await?;
    tx.commit().await?;
    Ok(OrderWithBookings { order, bookings })
}

/// Rebook: the same placement on the next position still open, as a NEW order so the original
/// record stays intact. This is the one-click "yes, I want that again" a newsletter advertiser asks
/// for by email otherwise.
pub async fn manage_rebook(pool: &PgPool, token: &str) -> Result<OrderWithBookings> {
    let mut conn = pool.acquire().await?;
    let order = order_by_token(&mut conn, token).await?;
    let cal = calendar_by_id(&mut conn, order.calendar_id).await?;
    let bookings = bookings_of(&mut conn, order.id).await?;
    let first = bookings
        .first()
        .ok_or(BookingError::NotFound("booking"))?
        .clone();

    // the next open position on the same resource, strictly after the one just booked
    let next = sqlx::query_as::<_, (Uuid, DateTime<Utc>, DateTime<Utc>, i64)>(
        "SELECT s.id, s.starts_at, s.ends_at, s.price_cents
           FROM booking_slots s
          WHERE s.resource_id = $1 AND s.status = 'open' AND s.starts_at > $2
            AND s.capacity > COALESCE((SELECT SUM(b.quantity) FROM booking_bookings b
                                        WHERE b.slot_id = s.id
                                          AND (b.status = 'confirmed'
                                               OR (b.status = 'held' AND b.hold_expires_at > now()))), 0)
          ORDER BY s.starts_at LIMIT 1",
    )
    .bind(first.resource_id)
    .bind(first.starts_at)
    .fetch_optional(&mut *conn)
    .await?;

    let (slot_id, starts, ends, price) = match next {
        Some((sid, st, en, pr)) => (Some(sid), st, en, pr),
        None => {
            // appointment/date_range resources have no slot rows: offer the same length one week on
            let start = first.starts_at + chrono::Duration::days(7);
            (
                None,
                start,
                start + (first.ends_at - first.starts_at),
                first.price_cents,
            )
        }
    };
    drop(conn);

    let hold = cal.hold_minutes;
    let mut tx = pool.begin().await?;
    let new_order = sqlx::query_as::<_, Order>(
        "INSERT INTO booking_orders
            (tenant_id, calendar_id, customer_name, customer_email, status, subtotal_cents,
             discount_cents, total_cents, hold_expires_at, notes)
         VALUES ($1, $2, $3, $4, 'pending', $5, 0, $5, now() + ($6::int * interval '1 minute'), $7)
         RETURNING *",
    )
    .bind(order.tenant_id)
    .bind(order.calendar_id)
    .bind(&order.customer_name)
    .bind(&order.customer_email)
    .bind(price)
    .bind(hold)
    .bind("Rebooked from ".to_string() + &order.id.to_string())
    .fetch_one(&mut *tx)
    .await?;

    let booking = sqlx::query_as::<_, Booking>(
        "INSERT INTO booking_bookings
            (order_id, resource_id, slot_id, starts_at, ends_at, quantity, price_cents, status,
             hold_expires_at, meta)
         VALUES ($1, $2, $3, $4, $5, 1, $6, 'held', now() + ($7::int * interval '1 minute'), $8)
         RETURNING *",
    )
    .bind(new_order.id)
    .bind(first.resource_id)
    .bind(slot_id)
    .bind(starts)
    .bind(ends)
    .bind(price)
    .bind(hold)
    .bind(serde_json::json!({ "rebooked_from": order.id }))
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(OrderWithBookings {
        order: new_order,
        bookings: vec![booking],
    })
}

/// The notification settings of one calendar: who hears about a booking, and when to remind.
///
/// Read and written as ONE object on purpose. These are three lists that only make sense together,
/// and "replace the whole set in a transaction" removes every partial-update bug a per-row API would
/// invite.
#[derive(Debug, serde::Serialize)]
pub struct NotifySettings {
    pub notify_emails: Vec<serde_json::Value>,
    pub reminders: Vec<serde_json::Value>,
    /// Tag rules: which tag a stage adds or removes.
    pub tag_rules: Vec<serde_json::Value>,
    /// List rules: which LIST a stage adds the booker to, or removes them from. Same stages as the
    /// tag rules, and the two are deliberately independent — an owner may want only one, or both.
    pub list_rules: Vec<serde_json::Value>,
}

pub async fn get_notify_settings(
    pool: &PgPool,
    tenant: Uuid,
    calendar_id: Uuid,
) -> Result<NotifySettings> {
    calendar_for_tenant(&mut *pool.acquire().await?, calendar_id, tenant).await?;

    let emails: Vec<(String, String)> = sqlx::query_as(
        "SELECT email, label FROM booking_notify_emails WHERE calendar_id = $1 AND is_active ORDER BY email",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await?;
    let reminders: Vec<(i32, String)> = sqlx::query_as(
        "SELECT offset_minutes, audience FROM booking_reminders
          WHERE calendar_id = $1 AND is_active ORDER BY offset_minutes DESC",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await?;
    let rules: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT tag_name, stage, mode FROM booking_tag_rules
          WHERE calendar_id = $1 AND is_active ORDER BY stage, tag_name",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await?;

    let list_rules: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT r.list_id, l.name, r.stage, r.mode
           FROM booking_list_rules r
           JOIN lists l ON l.id = r.list_id AND l.tenant_id = r.tenant_id
          WHERE r.calendar_id = $1 AND r.is_active
          ORDER BY r.stage, l.name",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    Ok(NotifySettings {
        notify_emails: emails
            .into_iter()
            .map(|(email, label)| serde_json::json!({ "email": email, "label": label }))
            .collect(),
        reminders: reminders
            .into_iter()
            .map(|(offset_minutes, audience)| {
                serde_json::json!({ "offset_minutes": offset_minutes, "audience": audience })
            })
            .collect(),
        tag_rules: rules
            .into_iter()
            .map(|(tag_name, stage, mode)| {
                serde_json::json!({ "tag_name": tag_name, "stage": stage, "mode": mode })
            })
            .collect(),
        list_rules: list_rules
            .into_iter()
            .map(|(list_id, list_name, stage, mode)| {
                serde_json::json!({
                    "list_id": list_id, "list_name": list_name, "stage": stage, "mode": mode
                })
            })
            .collect(),
    })
}

/// Replace the whole notification set for a calendar. Each entry is validated here: an address must
/// look like one, an offset must be a real interval, a letter cannot be anything else.
pub async fn set_notify_settings(
    pool: &PgPool,
    tenant: Uuid,
    calendar_id: Uuid,
    body: NotifySettingsInput,
) -> Result<NotifySettings> {
    calendar_for_tenant(&mut *pool.acquire().await?, calendar_id, tenant).await?;

    let mut emails: Vec<(String, String)> = Vec::new();
    for e in body.notify_emails.unwrap_or_default() {
        let address = e.email.trim().to_lowercase();
        if address.is_empty() {
            continue;
        }
        if !address.contains('@') || !address.split('@').nth(1).is_some_and(|d| d.contains('.')) {
            return Err(BookingError::invalid(format!(
                "{address} does not look like an email address"
            )));
        }
        emails.push((address, e.label.unwrap_or_default().trim().to_string()));
    }
    emails.sort();
    emails.dedup();

    let mut reminders: Vec<(i32, String)> = Vec::new();
    for r in body.reminders.unwrap_or_default() {
        if r.offset_minutes <= 0 || r.offset_minutes > 60 * 24 * 90 {
            return Err(BookingError::invalid(
                "a reminder offset must be between 1 minute and 90 days",
            ));
        }
        let audience = r.audience.trim().to_lowercase();
        if audience != "customer" && audience != "team" {
            return Err(BookingError::invalid(
                "a reminder goes to the customer or to the team",
            ));
        }
        reminders.push((r.offset_minutes, audience));
    }
    reminders.sort();
    reminders.dedup();

    let mut rules: Vec<(String, String, String)> = Vec::new();
    for t in body.tag_rules.unwrap_or_default() {
        let name = t.tag_name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let stage = t.stage.trim().to_lowercase();
        let mode = t.mode.trim().to_lowercase();
        if stage != "confirm" && stage != "cancel" {
            return Err(BookingError::invalid(
                "a tag rule runs on confirm or on cancel",
            ));
        }
        if mode != "add" && mode != "remove" {
            return Err(BookingError::invalid("a tag rule adds or removes the tag"));
        }
        rules.push((name, stage, mode));
    }
    rules.sort();
    rules.dedup();

    // List rules: the list must belong to THIS account, the stage must be one we run, and the mode
    // must be add or remove. A list id from another tenant is refused rather than silently dropped —
    // a rule that appears saved and does nothing is how a customer loses trust in the screen.
    let mut list_rules: Vec<(Uuid, String, String)> = Vec::new();
    for l in body.list_rules.unwrap_or_default() {
        let list_id = Uuid::parse_str(l.list_id.trim())
            .map_err(|_| BookingError::invalid("a list rule needs a list"))?;
        let owned: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM lists WHERE id = $1 AND tenant_id = $2")
                .bind(list_id)
                .bind(tenant)
                .fetch_optional(pool)
                .await?;
        if owned.is_none() {
            return Err(BookingError::invalid(
                "that list does not belong to this account",
            ));
        }
        let stage = l.stage.trim().to_lowercase();
        let mode = if l.mode.trim().is_empty() {
            "add".to_string()
        } else {
            l.mode.trim().to_lowercase()
        };
        if stage != "confirm" && stage != "cancel" {
            return Err(BookingError::invalid(
                "a list rule runs on confirm or on cancel",
            ));
        }
        if mode != "add" && mode != "remove" {
            return Err(BookingError::invalid(
                "a list rule adds or removes membership",
            ));
        }
        list_rules.push((list_id, stage, mode));
    }
    list_rules.sort();
    list_rules.dedup();

    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM booking_notify_emails WHERE calendar_id = $1")
        .bind(calendar_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM booking_reminders WHERE calendar_id = $1")
        .bind(calendar_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM booking_tag_rules WHERE calendar_id = $1")
        .bind(calendar_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM booking_list_rules WHERE calendar_id = $1")
        .bind(calendar_id)
        .execute(&mut *tx)
        .await?;

    for (address, label) in &emails {
        sqlx::query(
            "INSERT INTO booking_notify_emails (id, tenant_id, calendar_id, email, label)
             VALUES (gen_random_uuid(), $1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(calendar_id)
        .bind(address)
        .bind(label)
        .execute(&mut *tx)
        .await?;
    }
    for (offset, audience) in &reminders {
        sqlx::query(
            "INSERT INTO booking_reminders (id, tenant_id, calendar_id, offset_minutes, audience)
             VALUES (gen_random_uuid(), $1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(calendar_id)
        .bind(offset)
        .bind(audience)
        .execute(&mut *tx)
        .await?;
    }
    for (name, stage, mode) in &rules {
        sqlx::query(
            "INSERT INTO booking_tag_rules (id, tenant_id, calendar_id, tag_name, stage, mode)
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(calendar_id)
        .bind(name)
        .bind(stage)
        .bind(mode)
        .execute(&mut *tx)
        .await?;
    }
    for (list_id, stage, mode) in &list_rules {
        sqlx::query(
            "INSERT INTO booking_list_rules (id, tenant_id, calendar_id, list_id, stage, mode)
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5) ON CONFLICT DO NOTHING",
        )
        .bind(tenant)
        .bind(calendar_id)
        .bind(list_id)
        .bind(stage)
        .bind(mode)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    get_notify_settings(pool, tenant, calendar_id).await
}

/// What a booking address does when it is opened with no calendar in the path.
#[derive(Debug, serde::Serialize)]
pub struct HostSettings {
    pub root_redirect_url: Option<String>,
    pub default_calendar_slug: Option<String>,
}

pub async fn get_host_settings(pool: &PgPool, tenant: Uuid) -> Result<HostSettings> {
    let row: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT root_redirect_url, default_calendar_slug FROM booking_host_settings WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(pool)
    .await?;
    Ok(match row {
        Some((url, slug)) => HostSettings {
            root_redirect_url: url,
            default_calendar_slug: slug,
        },
        None => HostSettings {
            root_redirect_url: None,
            default_calendar_slug: None,
        },
    })
}

/// Save the address-level behaviour. A redirect target is validated as a WEB address: `https://`,
/// `http://` or a path starting `/`. Anything else — `javascript:`, `data:`, a bare word — is refused,
/// because this value becomes a browser navigation for every visitor.
pub async fn set_host_settings(
    pool: &PgPool,
    tenant: Uuid,
    root_redirect_url: Option<String>,
    default_calendar_slug: Option<String>,
) -> Result<HostSettings> {
    let url = root_redirect_url
        .map(|u| u.trim().to_string())
        .filter(|u| !u.is_empty());
    if let Some(u) = &url {
        let ok = u.starts_with("https://") || u.starts_with("http://") || u.starts_with('/');
        if !ok {
            return Err(BookingError::invalid(
                "a redirect must start with https://, http:// or /",
            ));
        }
        if u.starts_with("//") {
            return Err(BookingError::invalid("a redirect cannot start with //"));
        }
    }
    let slug = default_calendar_slug
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    sqlx::query(
        "INSERT INTO booking_host_settings (tenant_id, root_redirect_url, default_calendar_slug, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (tenant_id) DO UPDATE
            SET root_redirect_url = EXCLUDED.root_redirect_url,
                default_calendar_slug = EXCLUDED.default_calendar_slug,
                updated_at = now()",
    )
    .bind(tenant)
    .bind(&url)
    .bind(&slug)
    .execute(pool)
    .await?;

    Ok(HostSettings {
        root_redirect_url: url,
        default_calendar_slug: slug,
    })
}

/// The public answer for a booking address's root: where to send the visitor, and what this workspace
/// actually sells. Never empty — a visitor who lands here always gets a way forward.
#[derive(Debug, serde::Serialize)]
pub struct PublicHost {
    pub workspace: String,
    pub root_redirect_url: Option<String>,
    pub default_calendar_slug: Option<String>,
    pub calendars: Vec<serde_json::Value>,
}

pub async fn public_host(pool: &PgPool, workspace: Option<&str>) -> Result<PublicHost> {
    let Some(ws) = workspace.map(str::trim).filter(|w| !w.is_empty()) else {
        return Err(BookingError::NotFound("workspace"));
    };
    let tenant: Uuid = sqlx::query_scalar(
        "SELECT id FROM tenants WHERE (slug = $1 OR lower(name) = lower($1)) AND is_active LIMIT 1",
    )
    .bind(ws)
    .fetch_optional(pool)
    .await?
    .ok_or(BookingError::NotFound("workspace"))?;

    let settings = get_host_settings(pool, tenant).await?;
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT slug, name, kind::text FROM booking_calendars WHERE tenant_id = $1 AND is_active ORDER BY name",
    )
    .bind(tenant)
    .fetch_all(pool)
    .await?;

    Ok(PublicHost {
        workspace: ws.to_string(),
        root_redirect_url: settings.root_redirect_url,
        default_calendar_slug: settings.default_calendar_slug,
        calendars: rows
            .into_iter()
            .map(|(slug, name, kind)| serde_json::json!({ "slug": slug, "name": name, "kind": kind }))
            .collect(),
    })
}

/// An operator books a position on behalf of a business — the phone call, the invoice, the sponsor
/// who pays by bank transfer and never opens the page.
///
/// It goes through the SAME writer as a customer's booking (`create_order`, which does the capacity
/// check inside a transaction and locks the resource), and is then confirmed. There is deliberately no
/// second insert path: a manually filled slot must be indistinguishable from one sold on the page, or
/// the two sides drift and a slot gets sold twice.
pub async fn admin_create_booking(
    pool: &PgPool,
    tenant: Uuid,
    calendar_id: Uuid,
    customer_name: String,
    customer_email: String,
    notes: Option<String>,
    item: OrderItemInput,
    confirm_now: bool,
) -> Result<OrderWithBookings> {
    let cal = calendar_for_tenant(&mut *pool.acquire().await?, calendar_id, tenant).await?;

    let name = customer_name.trim().to_string();
    let email = customer_email.trim().to_lowercase();
    if name.is_empty() {
        return Err(BookingError::invalid(
            "whose booking is this? a name is needed",
        ));
    }
    if !email.contains('@') || !email.split('@').nth(1).is_some_and(|d| d.contains('.')) {
        return Err(BookingError::invalid(
            "that does not look like an email address for the business",
        ));
    }

    let order = create_order(
        pool,
        &cal,
        NewOrder {
            customer_name: name,
            customer_email: email,
            contact_id: None,
            notes: notes.filter(|n| !n.trim().is_empty()),
            // The operator filling a position by hand has no buyer consent to record: the buyer did not
            // tick anything in front of them. Both stay empty rather than being guessed. Their answers
            // are the operator's to type, so an empty set is passed rather than invented.
            answers: None,
            phone: None,
            sms_opt_in: None,
            items: vec![item],
        },
    )
    .await?;

    if confirm_now {
        return confirm_order(pool, order.order.id, Some(tenant), None).await;
    }
    Ok(order)
}

/// Fill a window across several positions of one calendar, reporting what each position got.
///
/// A per-position report is the point: the console can then say "Sponsored Post: 12 dates created,
/// 3 corrected" instead of leaving the operator to load the public page and count tiles.
pub async fn generate_slots_for_calendar(
    pool: &PgPool,
    tenant: Uuid,
    calendar_id: Uuid,
    req: GenerateForCalendar,
) -> Result<Vec<serde_json::Value>> {
    let mut c = pool.acquire().await?;
    let cal = calendar_for_tenant(&mut c, calendar_id, tenant).await?;
    let resources: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, name FROM booking_resources WHERE calendar_id = $1 ORDER BY name",
    )
    .bind(calendar_id)
    .fetch_all(&mut *c)
    .await?;
    drop(c);
    if resources.is_empty() {
        return Err(BookingError::invalid("add a bookable position first"));
    }
    let chosen: Vec<(Uuid, String)> = if req.resource_ids.is_empty() {
        resources
    } else {
        let wanted: Vec<Uuid> = req.resource_ids.clone();
        let picked: Vec<(Uuid, String)> = resources
            .into_iter()
            .filter(|(id, _)| wanted.contains(id))
            .collect();
        if picked.is_empty() {
            return Err(BookingError::invalid(
                "none of those positions belong to this calendar",
            ));
        }
        picked
    };

    let mut report = Vec::new();
    for (rid, name) in chosen {
        let before: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM booking_slots WHERE resource_id = $1 AND status = 'open'",
        )
        .bind(rid)
        .fetch_one(pool)
        .await?;
        let mut c = pool.acquire().await?;
        let (res, cal2) = resource_for_tenant(&mut c, rid, tenant).await?;
        drop(c);
        let window = GenerateSlots {
            from: req.window.from,
            to: req.window.to,
            weekdays: req.window.weekdays.clone(),
            time: req.window.time,
            duration_min: req.window.duration_min,
            capacity: req.window.capacity,
            price_cents: req.window.price_cents,
            weekday_prices: req.window.weekday_prices.clone(),
            label: req.window.label.clone(),
        };
        let _ = &cal;
        let result = generate_slots(pool, &res, &cal2, window).await;
        let (created, corrected, gone, note) = match result {
            Ok(slots) => {
                let after: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM booking_slots WHERE resource_id = $1 AND status = 'open'",
                )
                .bind(rid)
                .fetch_one(pool)
                .await?;
                let touched = slots.len() as i64;
                let delta = (after - before).max(0);
                (delta, (touched - delta).max(0), 0_i64, None::<String>)
            }
            Err(e) => (0, 0, 0, Some(e.to_string())),
        };
        // One row per position, prices read back from the database so the report cannot disagree
        // with what a buyer will be charged.
        let prices: Option<String> = sqlx::query_scalar(
            "SELECT string_agg(DISTINCT (price_cents / 100.0)::text, ', ') \
               FROM booking_slots WHERE resource_id = $1 AND status = 'open'",
        )
        .bind(rid)
        .fetch_one(pool)
        .await?;
        report.push(json!({
            "resource_id": rid,
            "resource": name,
            "created": created,
            "corrected": corrected,
            "removed": gone,
            "open_slots": before + created,
            "prices": prices.unwrap_or_default(),
            "error": note,
        }));
    }
    Ok(report)
}
