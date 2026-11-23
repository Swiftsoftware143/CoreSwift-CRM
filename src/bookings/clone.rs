//! Clone a calendar, into a workspace the owner chooses (David, 2026-11-23).
//!
//! > *"can you create the clone feature and make sure cloning is available across all workspaces — if
//! > a user clicks on clone a calendar this should be another thing to say which workspace as well as
//! > a rename so it's just not a clone hyphen."*
//!
//! A calendar that sells newsletter slots is a configuration: its hours, its resources, its prices, its
//! bundles, and the tags and lists a booking feeds. Rebuilding that by hand for every newsletter is the
//! work this exists to remove. The clone copies the CONFIGURATION and nothing else — no bookings, no
//! orders, no slots, and deliberately no Google mirror credential (two calendars pointing at one Google
//! calendar would fight over the same events).
//!
//! Three things this door refuses to do, each of them a real failure seen on this module:
//!
//! * **No "clone-1" name.** The name is required and the slug is derived from it, so the result is a
//!   calendar the owner recognises. The slug is made unique inside the account by suffixing -2, -3 …
//!   only when the derived one is already taken — never because the clone was unnamed.
//! * **No orphan workspace.** A calendar belongs to a workspace, so the clone names one. A workspace id
//!   from another account is refused rather than silently defaulted: that would put one account's
//!   calendar in a stranger's workspace.
//! * **No bypass of the plan ceiling.** A clone is a new calendar, so it pays the same price a create
//!   does (`limit_bookings`). A copy button that ignored the cap would be a way around the plan.

use axum::{
    extract::{Extension, Path, State},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct CloneCalendarRequest {
    /// The clone's own name. Required: an unnamed copy is the "clone hyphen" the owner rejected.
    pub name: String,
    /// The web address, pre-filled from the name in the dialog and editable there. Absent or blank
    /// means "derive it from the name".
    #[serde(default)]
    pub slug: Option<String>,
    /// Which workspace the clone belongs to. Absent means the source calendar's workspace, and a
    /// source with none means the account's default workspace.
    pub workspace_id: Option<Uuid>,
}

/// Derive a web address from the name: lowercase, runs of anything else become one hyphen.
fn slug_from_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = false;
    for ch in raw.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "calendar".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

/// `POST /api/booking/calendars/:id/clone` — the door the console calls.
///
/// The work lives in `clone_calendar_for` so the LIVE booking module (which extracts a `PgPool` and a
/// `TenantId`, not the app-wide `AppState`/`Claims`) can call the same implementation. One
/// implementation behind both doors is the point: a second copy of this transaction is a second set of
/// rules waiting to drift.
pub async fn clone_calendar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<CloneCalendarRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(clone_calendar_for(&s.db, tid, id, body).await?))
}

/// The clone itself, against a pool and an account id.
pub async fn clone_calendar_for(
    db: &sqlx::PgPool,
    tid: Uuid,
    id: Uuid,
    body: CloneCalendarRequest,
) -> Result<serde_json::Value, AppError> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Validation(
            "Give the copy its own name — a calendar called \"clone\" tells nobody which newsletter it is."
                .into(),
        ));
    }

    // The source must be this account's own calendar. Addressed by id, not slug: a clone is a button on
    // a row the owner is already looking at.
    let source: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, workspace_id FROM booking_calendars WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(db)
    .await?;
    let source_workspace = source
        .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?
        .1;

    // WHERE THE COPY LANDS. An id that is not this account's workspace is refused, never defaulted:
    // defaulting would drop the calendar into an account the caller did not name.
    let target: (Uuid, String) = match body.workspace_id {
        Some(wid) => {
            sqlx::query_as("SELECT id, name FROM workspaces WHERE id = $1 AND tenant_id = $2")
                .bind(wid)
                .bind(tid)
                .fetch_optional(db)
                .await?
                .ok_or_else(|| {
                    AppError::Validation(
                        "That workspace is not on this account, so the copy was not made.".into(),
                    )
                })?
        }
        None => sqlx::query_as(
            "SELECT id, name FROM workspaces WHERE tenant_id = $1 \
              ORDER BY (id = $2) DESC, is_default DESC, created_at ASC LIMIT 1",
        )
        .bind(tid)
        .bind(source_workspace)
        .fetch_optional(db)
        .await?
        .ok_or_else(|| {
            AppError::Validation(
                "This account has no workspace yet, so there is nowhere to put the copy.".into(),
            )
        })?,
    };

    // A clone is a new calendar: same ceiling as a create, or the cap means nothing.
    let existing: i64 =
        sqlx::query_scalar("SELECT count(*)::bigint FROM booking_calendars WHERE tenant_id = $1")
            .bind(tid)
            .fetch_one(db)
            .await?;
    crate::features::enforce_usage_limit(
        db,
        tid,
        crate::features::BOOKINGS_LIMIT_KEY,
        "Booking calendars",
        "calendars",
        existing,
    )
    .await?;

    // Slug from the NAME, made unique inside the account.
    // The dialog pre-fills this from the name, one hyphen between each word, and lets the owner edit
    // it; the server derives it only when nothing usable arrived, so what the owner saw is what they
    // get.
    let base = match body
        .slug
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(raw) => slug_from_name(raw),
        None => slug_from_name(&name),
    };
    let mut slug = base.clone();
    let mut n = 2;
    while sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM booking_calendars WHERE tenant_id = $1 AND slug = $2)",
    )
    .bind(tid)
    .bind(&slug)
    .fetch_one(db)
    .await?
    {
        slug = format!("{base}-{n}");
        n += 1;
        if n > 50 {
            return Err(AppError::Validation(
                "Too many calendars share that name — give the copy a different one.".into(),
            ));
        }
    }

    let mut tx = db.begin().await?;

    // The calendar row. google_refresh_token / google_calendar_id are NOT copied on purpose: a clone
    // that mirrored into the source's Google calendar would write two calendars' events into one.
    let (new_id, new_name, new_slug): (Uuid, String, String) = sqlx::query_as(
        "INSERT INTO booking_calendars \
             (tenant_id, name, slug, description, calendar_type, metadata, is_active, kind, timezone, \
              currency, hold_minutes, settings, reply_to_email, send_confirmation, workspace_id) \
         SELECT tenant_id, $2, $3, description, calendar_type, metadata, is_active, kind, timezone, \
                currency, hold_minutes, settings, reply_to_email, send_confirmation, $4 \
           FROM booking_calendars WHERE id = $1 AND tenant_id = $5 \
         RETURNING id, name, slug",
    )
    .bind(id)
    .bind(&name)
    .bind(&slug)
    .bind(target.0)
    .bind(tid)
    .fetch_one(&mut *tx)
    .await?;

    // RESOURCES, then the hours that hang off each one. The old→new id map is what keeps the hours and
    // the bundle rules pointing at the copy's own resources instead of the source's.
    let resources: Vec<(
        Uuid,
        String,
        Option<String>,
        i32,
        i64,
        Option<i64>,
        i32,
        i32,
        i32,
        i32,
        i32,
        Option<serde_json::Value>,
        bool,
    )> = sqlx::query_as(
        "SELECT id, name, description, capacity, price_cents, compare_at_price_cents, duration_min, \
                slot_interval_min, buffer_min, min_notice_min, sort_order, metadata, active \
           FROM booking_resources WHERE calendar_id = $1 ORDER BY sort_order, created_at",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;

    let mut map: Vec<(Uuid, Uuid)> = Vec::with_capacity(resources.len());
    let mut copied_rules = 0u32;
    for (
        rid,
        rname,
        rdesc,
        cap,
        price,
        compare_at,
        dur,
        interval,
        buffer,
        notice,
        order,
        meta,
        active,
    ) in resources
    {
        let new_rid: Uuid = sqlx::query_scalar(
            "INSERT INTO booking_resources \
                 (id, calendar_id, name, description, capacity, price_cents, compare_at_price_cents, \
                  duration_min, slot_interval_min, buffer_min, min_notice_min, sort_order, metadata, active) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
        )
        .bind(new_id)
        .bind(&rname)
        .bind(&rdesc)
        .bind(cap)
        .bind(price)
        .bind(compare_at)
        .bind(dur)
        .bind(interval)
        .bind(buffer)
        .bind(notice)
        .bind(order)
        .bind(&meta)
        .bind(active)
        .fetch_one(&mut *tx)
        .await?;
        copied_rules += sqlx::query(
            "INSERT INTO booking_availability_rules (id, resource_id, weekday, start_time, end_time) \
             SELECT gen_random_uuid(), $1, weekday, start_time, end_time \
               FROM booking_availability_rules WHERE resource_id = $2",
        )
        .bind(new_rid)
        .bind(rid)
        .execute(&mut *tx)
        .await?
        .rows_affected() as u32;
        map.push((rid, new_rid));
    }

    // BUNDLES — including the two resource-id arrays, remapped to the copy's resources. An id that is
    // not on this calendar (a bundle that names another calendar's resource) is dropped rather than
    // carried across, so the clone cannot reference something it does not own.
    let bundles: Vec<(
        String,
        String,
        i32,
        i32,
        Option<i64>,
        Option<Vec<Uuid>>,
        Option<Vec<Uuid>>,
        bool,
    )> = sqlx::query_as(
        "SELECT name, kind, min_items, percent_off, fixed_price_cents, \
                    eligible_resource_ids, required_resource_ids, active \
               FROM booking_bundles WHERE calendar_id = $1",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    let remap = |ids: Option<Vec<Uuid>>| -> Option<Vec<Uuid>> {
        ids.map(|list| {
            list.into_iter()
                .filter_map(|old| map.iter().find(|(o, _)| *o == old).map(|(_, n)| *n))
                .collect()
        })
    };
    let mut copied_bundles = 0u32;
    for (bname, bkind, min_items, pct, fixed, eligible, required, bactive) in bundles {
        sqlx::query(
            "INSERT INTO booking_bundles \
                 (id, calendar_id, name, kind, min_items, percent_off, fixed_price_cents, \
                  eligible_resource_ids, required_resource_ids, active) \
             VALUES (gen_random_uuid(), $1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(new_id)
        .bind(&bname)
        .bind(&bkind)
        .bind(min_items)
        .bind(pct)
        .bind(fixed)
        .bind(remap(eligible))
        .bind(remap(required))
        .bind(bactive)
        .execute(&mut *tx)
        .await?;
        copied_bundles += 1;
    }

    // The wiring a booking carries: which tags it applies, which lists it joins, who gets told, and the
    // reminder schedule. Tag and list NAMES/ids stay as they are — they name account-level objects, and
    // the copy is meant to do the same thing.
    let copied_tags = sqlx::query(
        "INSERT INTO booking_tag_rules (id, tenant_id, calendar_id, tag_name, stage, mode, is_active) \
         SELECT gen_random_uuid(), tenant_id, $1, tag_name, stage, mode, is_active \
           FROM booking_tag_rules WHERE calendar_id = $2",
    )
    .bind(new_id)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let copied_lists = sqlx::query(
        "INSERT INTO booking_list_rules (id, tenant_id, calendar_id, list_id, stage, mode, is_active) \
         SELECT gen_random_uuid(), tenant_id, $1, list_id, stage, mode, is_active \
           FROM booking_list_rules WHERE calendar_id = $2",
    )
    .bind(new_id)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let copied_notify = sqlx::query(
        "INSERT INTO booking_notify_emails (id, tenant_id, calendar_id, email, label, is_active) \
         SELECT gen_random_uuid(), tenant_id, $1, email, label, is_active \
           FROM booking_notify_emails WHERE calendar_id = $2",
    )
    .bind(new_id)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let copied_reminders = sqlx::query(
        "INSERT INTO booking_reminders (id, tenant_id, calendar_id, offset_minutes, audience, is_active) \
         SELECT gen_random_uuid(), tenant_id, $1, offset_minutes, audience, is_active \
           FROM booking_reminders WHERE calendar_id = $2",
    )
    .bind(new_id)
    .bind(id)
    .execute(&mut *tx)
    .await?
    .rows_affected();

    tx.commit().await?;

    Ok(json!({
        "id": new_id,
        "name": new_name,
        "slug": new_slug,
        "workspace_id": target.0,
        "workspace_name": target.1,
        "copied": {
            "resources": map.len(),
            "availability_rules": copied_rules,
            "bundles": copied_bundles,
            "tag_rules": copied_tags,
            "list_rules": copied_lists,
            "notify_emails": copied_notify,
            "reminders": copied_reminders,
        },
        "message": format!("Copied into {}. Bookings, orders and the Google mirror were not copied.", target.1),
    }))
}
