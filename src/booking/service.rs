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

pub async fn calendar_by_slug(conn: &mut PgConnection, slug: &str) -> Result<Calendar> {
    sqlx::query_as::<_, Calendar>("SELECT * FROM booking_calendars WHERE slug = $1 AND is_active")
        .bind(slug)
        .fetch_optional(conn)
        .await?
        .ok_or(BookingError::NotFound("calendar"))
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
           (calendar_id, name, description, capacity, price_cents, duration_min, slot_interval_min, buffer_min, min_notice_min, sort_order, metadata)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
    )
    .bind(calendar_id)
    .bind(r.name.trim())
    .bind(r.description.unwrap_or_default())
    .bind(r.capacity.unwrap_or(1).max(1))
    .bind(r.price_cents.unwrap_or(0).max(0))
    .bind(r.duration_min.unwrap_or(30).max(1))
    .bind(r.slot_interval_min.or(r.duration_min).unwrap_or(30).max(1))
    .bind(r.buffer_min.unwrap_or(0).max(0))
    .bind(r.min_notice_min.unwrap_or(0).max(0))
    .bind(r.sort_order.unwrap_or(0))
    .bind(r.metadata.unwrap_or_else(|| json!({})))
    .fetch_one(conn)
    .await?)
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
        let price = g
            .weekday_prices
            .get(&wd)
            .copied()
            .or(g.price_cents)
            .unwrap_or(res.price_cents);
        let slot = sqlx::query_as::<_, Slot>(
            "INSERT INTO booking_slots (resource_id, starts_at, ends_at, capacity, price_cents, label)
             VALUES ($1,$2,$3,$4,$5,$6) ON CONFLICT (resource_id, starts_at) DO NOTHING RETURNING *",
        )
        .bind(res.id)
        .bind(start)
        .bind(start + duration)
        .bind(g.capacity.unwrap_or(res.capacity).max(1))
        .bind(price.max(0))
        .bind(g.label.clone().unwrap_or_default())
        .fetch_optional(&mut *tx)
        .await?;
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

pub async fn availability(
    conn: &mut PgConnection,
    cal: &Calendar,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Opening>> {
    if to < from || (to - from).num_days() > MAX_RANGE_DAYS {
        return Err(BookingError::invalid(
            "range must be 0-{MAX_RANGE_DAYS} days",
        ));
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
                        remaining: (res.capacity as i64 - u).max(0),
                    });
                }
            }
            out.sort_by_key(|o| o.starts_at);
        }
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
           (tenant_id, calendar_id, contact_id, customer_name, customer_email, subtotal_cents, discount_cents, total_cents, bundle_id, hold_expires_at, notes)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING *",
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
    .bind(input.notes.unwrap_or_default())
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
    sqlx::query("UPDATE booking_bookings SET status = 'cancelled' WHERE order_id = $1")
        .bind(order.id)
        .execute(&mut *tx)
        .await?;
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
