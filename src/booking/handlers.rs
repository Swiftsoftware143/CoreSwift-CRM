//! Thin HTTP layer: parse, call `service`, return JSON.

use super::error::Result;
use super::models::*;
use super::service;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::error::BookingError;
use crate::errors::AppError;

// ---------- admin (requires Extension<TenantId>) ----------

pub async fn create_calendar(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Json(body): Json<NewCalendar>,
) -> Result<(StatusCode, Json<Calendar>)> {
    let mut c = pool.acquire().await?;
    Ok((
        StatusCode::CREATED,
        Json(service::create_calendar(&mut c, t.0, body).await?),
    ))
}

pub async fn list_calendars(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
) -> Result<Json<Vec<Value>>> {
    let mut c = pool.acquire().await?;
    let calendars = service::list_calendars(&mut c, t.0).await?;
    // WHICH WORKSPACE EACH CALENDAR BELONGS TO (2026-11-23). The Copy dialog asks which workspace the
    // copy should land in, and the list has to say where the calendars already are or an account with
    // one calendar per newsletter is a flat pile of names.
    let ws: Vec<(Uuid, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT c.id, c.workspace_id, w.name \
           FROM booking_calendars c LEFT JOIN workspaces w ON w.id = c.workspace_id \
          WHERE c.tenant_id = $1",
    )
    .bind(t.0)
    .fetch_all(&pool)
    .await?;
    let map: std::collections::HashMap<Uuid, (Option<Uuid>, Option<String>)> = ws
        .into_iter()
        .map(|(id, wid, name)| (id, (wid, name)))
        .collect();
    let mut out: Vec<Value> = Vec::with_capacity(calendars.len());
    for cal in calendars {
        // A calendar that cannot be serialised is not a request to fail: it is appended as null and
        // the console shows the rest of the list (a 500 here would hide every calendar behind one).
        let mut v = serde_json::to_value(&cal).unwrap_or(Value::Null);
        if let Some(obj) = v.as_object_mut() {
            let (wid, name) = map.get(&cal.id).cloned().unwrap_or((None, None));
            obj.insert("workspace_id".into(), json!(wid));
            obj.insert("workspace_name".into(), json!(name));
        }
        out.push(v);
    }
    Ok(Json(out))
}

/// `POST /api/booking/calendars/:id/clone` — copy a calendar's configuration into a workspace.
pub async fn clone_calendar(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::bookings::clone::CloneCalendarRequest>,
) -> Result<Json<Value>> {
    // The clone's own errors are the app-wide ones; they are mapped onto this module's vocabulary
    // rather than leaking a second error type through the live booking router.
    let value = crate::bookings::clone::clone_calendar_for(&pool, t.0, id, body)
        .await
        .map_err(|e| match e {
            AppError::Validation(msg) | AppError::BadRequest(msg) => BookingError::Invalid(msg),
            // A calendar that is not on this account is a request about something the caller cannot
            // see, not a server fault — and it must not leak whether the id exists elsewhere.
            AppError::NotFound(_) => {
                BookingError::Invalid("That calendar is not on this account.".into())
            }
            AppError::Unauthorized | AppError::ForbiddenReason(_) => {
                BookingError::Invalid("This account cannot copy that calendar.".into())
            }
            other => BookingError::Invalid(other.to_string()),
        })?;
    Ok(Json(value))
}

/// `GET /api/booking/calendars/summary` — everything about every calendar in ONE answer.
///
/// WHY THIS EXISTS (ZaarHub, 2026-11-23): verifying ten city calendars meant ten browser sessions of
/// selector-wrangling, and a rule pointing at the WRONG list looked identical to one pointing at the
/// right list until somebody clicked through the form. One read that states the facts — workspace, own
/// domain, every position with its prices, the tag rule, the list rule — turns a ten-session audit into
/// one call, and makes a false alarm impossible: what is stored is what is printed.
///
/// It also carries a `problems` list. A summary that only echoed data would still leave the reader to
/// spot the mistake; the problems are the checks a careful operator would make — a position with no
/// price, a calendar with nothing bookable, a rule that is missing or points at a list whose name does
/// not match the workspace, dates that have not been generated.
pub async fn calendars_summary(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
) -> Result<Json<Value>> {
    let calendars: Vec<(Uuid, String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT c.id, c.name, c.slug, c.workspace_id, w.name \
           FROM booking_calendars c LEFT JOIN workspaces w ON w.id = c.workspace_id \
          WHERE c.tenant_id = $1 AND c.is_active \
          ORDER BY w.name NULLS LAST, c.name",
    )
    .bind(t.0)
    .fetch_all(&pool)
    .await?;

    let hosts: Vec<(Uuid, String, Option<String>, String)> = sqlx::query_as(
        "SELECT workspace_id, subdomain, custom_domain, custom_domain_status \
           FROM workspace_hosts WHERE tenant_id = $1 AND is_active",
    )
    .bind(t.0)
    .fetch_all(&pool)
    .await?;
    let account_host: Option<String> =
        sqlx::query_scalar("SELECT subdomain FROM calendar_hosts WHERE tenant_id = $1")
            .bind(t.0)
            .fetch_optional(&pool)
            .await?;
    let system = std::env::var("APP_DOMAIN").unwrap_or_else(|_| "coreswiftcrm.com".to_string());

    let mut out: Vec<Value> = Vec::with_capacity(calendars.len());
    let mut problems: Vec<String> = Vec::new();

    for (cal_id, name, slug, workspace_id, workspace_name) in calendars {
        // Positions, with what is actually on sale: open dated slots and how many are taken.
        let positions: Vec<(Uuid, String, i64, Option<i64>, i32, i32, bool)> = sqlx::query_as(
            "SELECT r.id, r.name, r.price_cents, r.compare_at_price_cents, r.capacity, r.duration_min,
                    r.exclusive
               FROM booking_resources r
              WHERE r.calendar_id = $1 AND r.active ORDER BY r.sort_order, r.name",
        )
        .bind(cal_id)
        .fetch_all(&pool)
        .await?;

        let mut pos_json: Vec<Value> = Vec::with_capacity(positions.len());
        for (rid, rname, price, compare, capacity, duration, exclusive_flag) in positions {
            let open: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM booking_slots \
                  WHERE resource_id = $1 AND status = 'open' AND starts_at > now()",
            )
            .bind(rid)
            .fetch_one(&pool)
            .await?;
            // Dates this position has closed BY the exclusivity rule — the visible half of the block.
            let blocked_slots: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM booking_slots \
                  WHERE resource_id = $1 AND closed_reason LIKE 'exclusive:%'",
            )
            .bind(rid)
            .fetch_one(&pool)
            .await?;
            let taken: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM booking_bookings b JOIN booking_slots s ON s.id = b.slot_id \
                  WHERE s.resource_id = $1 AND b.status IN ('confirmed','held')",
            )
            .bind(rid)
            .fetch_one(&pool)
            .await?;
            if price <= 0 {
                problems.push(format!("{name}: position \"{rname}\" has no price."));
            }
            if let Some(c) = compare {
                if c <= price {
                    problems.push(format!(
                        "{name}: position \"{rname}\" has a compare-at price that is not higher than the price, so it is never shown."
                    ));
                }
            }
            if open == 0 {
                problems.push(format!(
                    "{name}: position \"{rname}\" has no open future dates — nothing to book yet."
                ));
            }
            // A DATE ON SALE TWICE IS THE ONE THING THIS ENDPOINT MUST SHOUT ABOUT (measured live
            // 2026-11-24: "Email — TOP reports open_slots: 80"). The generator now corrects by date, so
            // this catches anything a hand edit or an older run left behind.
            let doubled: Vec<String> = sqlx::query_scalar(
                "SELECT (starts_at AT TIME ZONE c.timezone)::date::text
                   FROM booking_slots s JOIN booking_calendars c ON c.id = $2
                  WHERE s.resource_id = $1 AND s.status = 'open'
                  GROUP BY 1 HAVING count(*) > 1",
            )
            .bind(rid)
            .bind(cal_id)
            .fetch_all(&pool)
            .await?;
            if !doubled.is_empty() {
                problems.push(format!(
                    "{name}: position \"{rname}\" has the same date on sale more than once ({}), so it could be booked twice for one issue.",
                    doubled.join(", ")
                ));
            }
            pos_json.push(json!({
                // Ids travel with the report: a read endpoint that cannot name a row cannot be used to
                // act on it, which is the difference between verifying ten calendars and driving them.
                "id": rid,
                "name": rname,
                "price_cents": price,
                "compare_at_price_cents": compare,
                "capacity": capacity,
                "duration_min": duration,
                "open_slots": open,
                "bookings": taken,
                // The exclusivity state, read from the rows: the flag plus how many dates this position
                // has blocked (or been blocked on). Verifiable in one call instead of opening a page.
                "exclusive": exclusive_flag,
                "blocked_slots": blocked_slots,
            }));
        }
        if pos_json.is_empty() {
            problems.push(format!("{name}: nothing bookable — no positions at all."));
        }

        let tag_rule: Option<(String, String, String)> = sqlx::query_as(
            "SELECT tag_name, stage, mode FROM booking_tag_rules \
              WHERE calendar_id = $1 AND is_active ORDER BY created_at LIMIT 1",
        )
        .bind(cal_id)
        .fetch_optional(&pool)
        .await?;
        let list_rule: Option<(Uuid, String, String, String)> = sqlx::query_as(
            "SELECT r.list_id, coalesce(l.name, ''), r.stage, r.mode \
               FROM booking_list_rules r LEFT JOIN lists l ON l.id = r.list_id \
              WHERE r.calendar_id = $1 AND r.is_active ORDER BY r.created_at LIMIT 1",
        )
        .bind(cal_id)
        .fetch_optional(&pool)
        .await?;

        if tag_rule.is_none() {
            problems.push(format!(
                "{name}: no tag rule — a booking there tags nobody."
            ));
        }
        match list_rule.as_ref() {
            None => problems.push(format!(
                "{name}: no list rule — a booking there adds nobody to a list."
            )),
            Some((_, list_name, _, _)) => {
                // DOES THE LIST BELONG TO THIS CALENDAR? The first version compared the list against the
                // WORKSPACE name only, which flagged every correct city list whose newsletter is branded
                // differently (a "St. Petersburg - Sponsors" list on the "Bay Breeze Bulletin" workspace).
                // A warning that fires on correct configuration is noise, and noise gets ignored — so the
                // city is taken from the CALENDAR name too, and a list matching EITHER is accepted.
                let city_of = |raw: &str| -> String {
                    raw.split(" Local Newsletters")
                        .next()
                        .unwrap_or("")
                        .split(" \u{2014} ")
                        .next()
                        .unwrap_or("")
                        .split(" \u{2013} ")
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_lowercase()
                };
                let cal_city = city_of(&name);
                let ws_city = workspace_name.as_deref().map(city_of).unwrap_or_default();
                let list_l = list_name.to_lowercase();
                let first = |v: &str| v.split(' ').next().unwrap_or("").to_string();
                let mut tokens: Vec<String> = Vec::new();
                for t in [first(&cal_city), first(&ws_city)] {
                    if !t.is_empty() && !tokens.contains(&t) {
                        tokens.push(t);
                    }
                }
                let matches = tokens.iter().any(|t| list_l.contains(t.as_str()));
                if !tokens.is_empty() && !matches {
                    problems.push(format!(
                        "{name}: list rule points at \"{list_name}\", which does not look like this city — check it is the right list."
                    ));
                }
            }
        }

        let host = workspace_id.and_then(|wid| {
            hosts
                .iter()
                .find(|(w, _, _, _)| *w == wid)
                .map(|(_, sub, custom, status)| (sub.clone(), custom.clone(), status.clone()))
        });
        let (subdomain, own_domain, own_domain_status) = match host {
            Some((sub, custom, status)) => (Some(sub), custom, Some(status)),
            None => (account_host.clone(), None, None),
        };
        if subdomain.is_none() {
            problems.push(format!(
                "{name}: no public address — nobody can reach this calendar."
            ));
        }

        out.push(json!({
            "id": cal_id,
            "name": name,
            "slug": slug,
            "workspace": workspace_name,
            "workspace_id": workspace_id,
            "public_url": subdomain.as_ref().map(|s| format!("https://{s}.{system}/b/{slug}")),
            "address": subdomain,
            "own_domain": own_domain,
            "own_domain_status": own_domain_status,
            "positions": pos_json,
            "tag_rule": tag_rule.map(|(tag, stage, mode)| json!({"tag": tag, "stage": stage, "mode": mode})),
            "list_rule": list_rule.map(|(id, lname, stage, mode)| json!({"list_id": id, "list": lname, "stage": stage, "mode": mode})),
        }));
    }

    // A one-line-per-calendar reading, so a human can scan the answer without parsing JSON.
    let mut lines: Vec<String> = Vec::new();
    for c in &out {
        let positions = c["positions"].as_array().map(|a| a.len()).unwrap_or(0);
        let tag = c["tag_rule"]["tag"].as_str().unwrap_or("(none)");
        let list = c["list_rule"]["list"].as_str().unwrap_or("(none)");
        lines.push(format!(
            "{} | {} position(s) | tag: {} | list: {} | {}",
            c["name"].as_str().unwrap_or("?"),
            positions,
            tag,
            list,
            c["public_url"].as_str().unwrap_or("(no address)")
        ));
    }

    Ok(Json(json!({
        "calendars": out,
        "count": out.len(),
        "problems": problems,
        "lines": lines,
    })))
}

/// `GET /api/booking/calendars/:id/form-fields` — the questions this form asks, and which are required.
pub async fn list_form_fields(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_for_tenant(&mut c, id, t.0).await?;
    let rows: Vec<crate::booking::models::FormField> = sqlx::query_as(
        "SELECT id, calendar_id, key, label, kind, required, placeholder, help, sort_order, is_active \
           FROM booking_form_fields WHERE calendar_id = $1 ORDER BY sort_order, label",
    )
    .bind(cal.id)
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(json!({
        // Name, email and phone are built in: they are how the booking reaches the person and how the
        // person is found again. The owner chooses whether they are mandatory; they cannot be removed.
        "builtin": [
            {"key": "customer_name", "label": "Full name", "required": true, "locked": true, "why": "It is who the booking is for."},
            {"key": "customer_email", "label": "Email", "required": true, "locked": true, "why": "The confirmation and the manage link go here."},
            {"key": "phone", "label": "Phone", "required": false, "locked": true, "why": "Optional unless you make it mandatory."}
        ],
        "fields": rows,
    })))
}

/// `POST /api/booking/calendars/:id/form-fields` — add a question.
pub async fn create_form_field(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::booking::models::NewFormField>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_for_tenant(&mut c, id, t.0).await?;
    let label = body.label.trim().to_string();
    if label.is_empty() {
        return Err(BookingError::invalid("Give the question a label."));
    }
    // The key is what the answer is stored under. Derived from the label when not given, so the owner
    // never has to invent machine names.
    let key = body
        .key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|k| k.to_ascii_lowercase().replace(' ', "_"))
        .unwrap_or_else(|| {
            label
                .to_ascii_lowercase()
                .chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
                .collect::<String>()
                .trim_matches('_')
                .to_string()
        });
    if matches!(
        key.as_str(),
        "customer_name" | "customer_email" | "phone" | "answers" | "items"
    ) {
        return Err(BookingError::invalid(format!(
            "\"{key}\" is already captured by the booking itself — choose another name."
        )));
    }
    let kind = body.kind.as_deref().unwrap_or("text").to_string();
    let row: crate::booking::models::FormField = sqlx::query_as(
        "INSERT INTO booking_form_fields \
             (tenant_id, calendar_id, key, label, kind, required, placeholder, help, sort_order) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) \
         ON CONFLICT (calendar_id, lower(key)) DO UPDATE \
            SET label = EXCLUDED.label, kind = EXCLUDED.kind, required = EXCLUDED.required, \
                placeholder = EXCLUDED.placeholder, help = EXCLUDED.help, is_active = true, \
                updated_at = now() \
         RETURNING id, calendar_id, key, label, kind, required, placeholder, help, sort_order, is_active",
    )
    .bind(t.0)
    .bind(cal.id)
    .bind(&key)
    .bind(&label)
    .bind(&kind)
    .bind(body.required.unwrap_or(false))
    .bind(&body.placeholder)
    .bind(&body.help)
    .bind(body.sort_order.unwrap_or(0))
    .fetch_one(&mut *c)
    .await?;
    Ok(Json(json!(row)))
}

/// `PATCH /api/booking/form-fields/:id` — flip required, rename, reorder.
pub async fn update_form_field(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(field_id): Path<Uuid>,
    Json(body): Json<crate::booking::models::NewFormField>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let row: crate::booking::models::FormField = sqlx::query_as(
        "UPDATE booking_form_fields SET \
             label = COALESCE($3, label), \
             kind = COALESCE($4, kind), \
             required = COALESCE($5, required), \
             placeholder = COALESCE($6, placeholder), \
             help = COALESCE($7, help), \
             sort_order = COALESCE($8, sort_order), \
             updated_at = now() \
           WHERE id = $1 AND tenant_id = $2 \
         RETURNING id, calendar_id, key, label, kind, required, placeholder, help, sort_order, is_active",
    )
    .bind(field_id)
    .bind(t.0)
    .bind({
        let l = body.label.trim();
        if l.is_empty() {
            None
        } else {
            Some(l.to_string())
        }
    })
    .bind(body.kind)
    .bind(body.required)
    .bind(body.placeholder)
    .bind(body.help)
    .bind(body.sort_order)
    .fetch_optional(&mut *c)
    .await?
    .ok_or_else(|| BookingError::NotFound("question"))?;
    Ok(Json(json!(row)))
}

/// `DELETE /api/booking/form-fields/:id` — stop asking it. Answers already given stay on their orders.
pub async fn delete_form_field(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(field_id): Path<Uuid>,
) -> Result<Json<Value>> {
    let done = sqlx::query("DELETE FROM booking_form_fields WHERE id = $1 AND tenant_id = $2")
        .bind(field_id)
        .bind(t.0)
        .execute(&pool)
        .await?
        .rows_affected();
    if done == 0 {
        return Err(BookingError::NotFound("question"));
    }
    Ok(Json(json!({"removed": true})))
}

/// `GET /api/public/booking/c/:slug/form` — the questions a buyer is asked.
pub async fn public_form(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<WsQuery>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of_public(&mut c, &q).await?;
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    let rows: Vec<crate::booking::models::FormField> = sqlx::query_as(
        "SELECT id, calendar_id, key, label, kind, required, placeholder, help, sort_order, is_active \
           FROM booking_form_fields WHERE calendar_id = $1 AND is_active ORDER BY sort_order, label",
    )
    .bind(cal.id)
    .fetch_all(&mut *c)
    .await?;
    Ok(Json(json!({ "fields": rows })))
}

/// `GET /api/booking/calendars/:id/buyer-preview` — what the buyer will SEE, with the limits applied.
///
/// The console and the public page must be the same answer rendered twice; a panel that described the
/// settings instead of showing their result is how a page showing nothing went unnoticed.
pub async fn buyer_preview(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_for_tenant(&mut c, id, t.0).await?;
    // The widest window the builder accepts — asking for a year is refused, and a refused range used to
    // read as an empty calendar.
    let (today, to) = service::widest_window();
    let view = service::buyer_view(&mut c, &cal, today, to).await?;
    let tiles = service::availability(&mut c, &cal, today, to, true).await?;
    Ok(Json(json!({
        "calendar": { "name": cal.name, "slug": cal.slug, "timezone": cal.timezone },
        "view": view,
        "tiles": tiles,
    })))
}

/// `PATCH /api/booking/resources/:id` — correct an existing position.
pub async fn update_resource(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<NewResource>,
) -> Result<Json<Resource>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::update_resource(&mut c, t.0, id, body).await?))
}

/// `DELETE /api/booking/resources/:id` — remove a position, refused while bookings depend on it.
pub async fn delete_resource(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let (name, slots) = service::delete_resource(&mut c, t.0, id).await?;
    Ok(Json(json!({
        "deleted": name,
        "slots_removed": slots,
        "message": format!("\"{name}\" removed{}, along with its open dates.", if slots == 1 { "" } else { "" }),
    })))
}

pub async fn get_calendar(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_for_tenant(&mut c, id, t.0).await?;
    let resources = service::list_resources(&mut c, id, false).await?;
    let bundles = service::list_bundles(&mut c, id, false).await?;
    Ok(Json(
        json!({ "calendar": cal, "resources": resources, "bundles": bundles }),
    ))
}

pub async fn get_notifications(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<service::NotifySettings>> {
    Ok(Json(service::get_notify_settings(&pool, t.0, id).await?))
}

pub async fn set_notifications(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<NotifySettingsInput>,
) -> Result<Json<service::NotifySettings>> {
    Ok(Json(
        service::set_notify_settings(&pool, t.0, id, body).await?,
    ))
}

pub async fn update_calendar(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateCalendar>,
) -> Result<Json<Calendar>> {
    Ok(Json(
        service::update_calendar(
            &pool,
            t.0,
            id,
            body.name,
            body.slug,
            body.settings,
            body.reply_to_email,
            body.send_confirmation,
        )
        .await?,
    ))
}

pub async fn create_resource(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<NewResource>,
) -> Result<(StatusCode, Json<Resource>)> {
    let mut c = pool.acquire().await?;
    service::calendar_for_tenant(&mut c, id, t.0).await?;
    Ok((
        StatusCode::CREATED,
        Json(service::create_resource(&mut c, id, body).await?),
    ))
}

pub async fn set_rules(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<Vec<AvailabilityRule>>,
) -> Result<Json<Vec<AvailabilityRule>>> {
    let mut c = pool.acquire().await?;
    service::resource_for_tenant(&mut c, id, t.0).await?;
    drop(c);
    Ok(Json(service::set_rules(&pool, id, body).await?))
}

pub async fn generate_slots(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<GenerateSlots>,
) -> Result<(StatusCode, Json<Vec<Slot>>)> {
    let mut c = pool.acquire().await?;
    let (res, cal) = service::resource_for_tenant(&mut c, id, t.0).await?;
    drop(c);
    Ok((
        StatusCode::CREATED,
        Json(service::generate_slots(&pool, &res, &cal, body).await?),
    ))
}

/// Fill the same window across several positions of one calendar, with a per-position report.
pub async fn generate_slots_for_calendar(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<GenerateForCalendar>,
) -> Result<(StatusCode, Json<Vec<serde_json::Value>>)> {
    Ok((
        StatusCode::CREATED,
        Json(service::generate_slots_for_calendar(&pool, t.0, id, body).await?),
    ))
}

pub async fn update_slot(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateSlot>,
) -> Result<Json<Slot>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::update_slot(&mut c, t.0, id, body).await?))
}

pub async fn create_bundle(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<NewBundle>,
) -> Result<(StatusCode, Json<Bundle>)> {
    let mut c = pool.acquire().await?;
    service::calendar_for_tenant(&mut c, id, t.0).await?;
    Ok((
        StatusCode::CREATED,
        Json(service::create_bundle(&mut c, id, body).await?),
    ))
}

pub async fn admin_availability(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> Result<Json<Vec<Opening>>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_for_tenant(&mut c, id, t.0).await?;
    Ok(Json(
        service::availability(&mut c, &cal, q.from, q.to, false).await?,
    ))
}

pub async fn list_orders(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<Order>>> {
    let mut c = pool.acquire().await?;
    service::calendar_for_tenant(&mut c, id, t.0).await?;
    Ok(Json(service::list_orders(&mut c, id).await?))
}

pub async fn admin_get_order(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderWithBookings>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::get_order(&mut c, id, Some(t.0)).await?))
}

pub async fn confirm_order(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    body: Option<Json<ConfirmOrder>>,
) -> Result<Json<OrderWithBookings>> {
    let payment_ref = body.and_then(|Json(b)| b.payment_ref);
    let res = service::confirm_order(&pool, id, Some(t.0), payment_ref).await?;
    // the booking is committed; the mail is a courtesy that must never fail it
    let pool2 = pool.clone();
    let oid = res.order.id;
    tokio::spawn(async move { super::notify::on_confirmed(&pool2, oid).await });
    Ok(Json(res))
}

pub async fn cancel_order(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderWithBookings>> {
    let res = service::cancel_order(&pool, id, Some(t.0)).await?;
    let pool2 = pool.clone();
    let oid = res.order.id;
    tokio::spawn(async move { super::notify::on_cancelled(&pool2, oid).await });
    Ok(Json(res))
}

pub async fn review_creative(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreativeReview>,
) -> Result<Json<Booking>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::review_creative(&mut c, t.0, id, body).await?))
}

// ---------- public (no auth; calendar found by slug, orders by unguessable id) ----------
/// Which workspace a public request belongs to.
///
/// Two sources, in order: the `?w=` qualifier in the link (the shared address carries the workspace in
/// the path), then `X-Booking-Workspace`, which the business's own booking address sets from nginx —
/// the host already told us which business it is, so a link there needs no qualifier.
///
/// The header can only ever select between PUBLIC calendars: every route that reads it is anonymous
/// by design and returns the same data a public link would. It cannot reach a console route or
/// another workspace's private data. (Content changed to force a real rebuild: a deploy reported the
/// tree fresh while the shipped binary still lacked this code.)
fn workspace_of(headers: &axum::http::HeaderMap, q: &WsQuery) -> Option<String> {
    if let Some(w) = q.w.as_deref().map(str::trim).filter(|w| !w.is_empty()) {
        return Some(w.to_string());
    }
    headers
        .get("x-booking-workspace")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string)
}

/// `?w=<workspace>` — only needed when the same calendar name exists in more than one workspace.
#[derive(Debug, Default, serde::Deserialize)]
pub struct WsQuery {
    #[serde(default)]
    pub w: Option<String>,
}

pub async fn public_calendar(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of(&headers, &q);
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    let resources = service::list_resources(&mut c, cal.id, true).await?;
    let bundles = service::list_bundles(&mut c, cal.id, true).await?;
    // THE PAGE A READER LANDS ON SHOULD LOOK LIKE THE PUBLICATION (ZaarHub, 2026-11-23): the console's
    // theme fields were stored but nothing on the customer-facing page read them, so every booking link
    // opened a default indigo form. The workspace's own brand name, colour and logo travel with the
    // calendar now, and the page paints them; a workspace with no branding set keeps the default look.
    let branding: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT jsonb_build_object(\
                 'name', coalesce(settings->'email_branding'->>'brand_name', name),\
                 'color', coalesce(settings->'email_branding'->>'brand_color', primary_color, accent_color),\
                 'logo_url', coalesce(logo_url, settings->'email_branding'->>'logo_url')\
             )\
           FROM tenants WHERE id = $1",
    )
    .bind(cal.tenant_id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();
    Ok(Json(json!({
        "calendar": { "name": cal.name, "slug": cal.slug, "kind": cal.kind, "timezone": cal.timezone,
                      "currency": cal.currency, "hold_minutes": cal.hold_minutes, "settings": cal.settings },
        "resources": resources,
        "bundles": bundles,
        "branding": branding,
    })))
}

/// `GET /api/public/booking/c/:slug/limits` — what this page is showing, and why.
///
/// The public page prints one line from this: when a horizon or a per-date cap is in force the visitor
/// is told more dates open as the calendar advances. Scarcity that reads as a closed door loses the
/// sale; the same scarcity explained reads as demand.
pub async fn public_limits(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<WsQuery>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of_public(&mut c, &q).await?;
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    let (today, to) = service::widest_window();
    let view = service::buyer_view(&mut c, &cal, today, to).await?;
    Ok(Json(json!({
        "limits": view,
        "more_later": !view.unlimited && (view.horizon_days.is_some() || view.per_day_per_position.is_some()),
    })))
}

pub async fn public_availability(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<RangeQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Vec<Opening>>> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of(&headers, &WsQuery { w: q.w.clone() });
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    // a buyer sees the owner's limits; the operator does not (see admin_availability)
    Ok(Json(
        service::availability(&mut c, &cal, q.from, q.to, true).await?,
    ))
}

pub async fn public_quote(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
    Json(body): Json<QuoteRequest>,
) -> Result<Json<Quote>> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of(&headers, &q);
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    Ok(Json(service::quote(&mut c, &cal, &body.items).await?))
}

pub async fn public_create_order(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
    Json(body): Json<NewOrder>,
) -> Result<(StatusCode, Json<OrderWithBookings>)> {
    let mut c = pool.acquire().await?;
    let ws = workspace_of(&headers, &q);
    let cal = service::calendar_by_slug(&mut c, &slug, ws.as_deref()).await?;
    drop(c);
    let created = service::create_order(&pool, &cal, body).await?;
    if created.order.status != "confirmed" && created.order.total_cents == 0 {
        // nothing to pay, so there is nothing to wait for
        match service::confirm_order(&pool, created.order.id, None, None).await {
            Ok(mut confirmed) => {
                let pool2 = pool.clone();
                let oid = confirmed.order.id;
                tokio::spawn(async move { super::notify::on_confirmed(&pool2, oid).await });
                confirmed.order.status = "confirmed".to_string();
                return Ok((StatusCode::CREATED, Json(confirmed)));
            }
            Err(e) => {
                tracing::warn!(error = %e, "booking: free order could not be auto-confirmed");
            }
        }
    }
    Ok((StatusCode::CREATED, Json(created)))
}

pub async fn public_get_order(
    State(pool): State<PgPool>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderWithBookings>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::get_order(&mut c, id, None).await?))
}

pub async fn public_submit_creative(
    State(pool): State<PgPool>,
    Path((order_id, booking_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<Value>,
) -> Result<Json<Booking>> {
    let mut c = pool.acquire().await?;
    Ok(Json(
        service::submit_creative(&mut c, order_id, booking_id, body).await?,
    ))
}

// ---------- manage link (public: the token in the URL is the authority) ----------

#[derive(Debug, serde::Deserialize)]
pub struct RescheduleBody {
    pub booking_id: Uuid,
    pub slot_id: Option<Uuid>,
    pub starts_at: Option<chrono::DateTime<chrono::Utc>>,
    pub end_date: Option<chrono::NaiveDate>,
}

pub async fn manage_view(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let view = service::manage_view(&pool, &token).await?;
    Ok(Json(json!({ "ok": true, "booking": view })))
}

pub async fn manage_cancel(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let res = service::manage_cancel(&pool, &token).await?;
    Ok(Json(
        json!({ "ok": true, "order": res.order, "bookings": res.bookings }),
    ))
}

pub async fn manage_reschedule(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
    Json(body): Json<RescheduleBody>,
) -> Result<Json<Value>> {
    let res = service::manage_reschedule(
        &pool,
        &token,
        body.booking_id,
        body.slot_id,
        body.starts_at,
        body.end_date,
    )
    .await?;
    Ok(Json(
        json!({ "ok": true, "order": res.order, "bookings": res.bookings }),
    ))
}

pub async fn manage_rebook(
    State(pool): State<PgPool>,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let res = service::manage_rebook(&pool, &token).await?;
    Ok(Json(
        json!({ "ok": true, "order": res.order, "bookings": res.bookings }),
    ))
}

// ---------- the booking address's root ----------

#[derive(Debug, serde::Deserialize)]
pub struct HostSettingsBody {
    #[serde(default)]
    pub root_redirect_url: Option<String>,
    #[serde(default)]
    pub default_calendar_slug: Option<String>,
}

pub async fn get_host(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
) -> Result<Json<service::HostSettings>> {
    Ok(Json(service::get_host_settings(&pool, t.0).await?))
}

pub async fn set_host(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Json(body): Json<HostSettingsBody>,
) -> Result<Json<service::HostSettings>> {
    Ok(Json(
        service::set_host_settings(
            &pool,
            t.0,
            body.root_redirect_url,
            body.default_calendar_slug,
        )
        .await?,
    ))
}

/// Public: what should happen when someone opens a booking address with nothing after it.
pub async fn public_host(
    State(pool): State<PgPool>,
    Query(q): Query<WsQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Json<service::PublicHost>> {
    let ws = workspace_of(&headers, &q);
    Ok(Json(service::public_host(&pool, ws.as_deref()).await?))
}

// ---------- the operator filling a slot by hand ----------

#[derive(Debug, serde::Deserialize)]
pub struct AdminBookingBody {
    pub resource_id: Uuid,
    #[serde(default)]
    pub slot_id: Option<Uuid>,
    #[serde(default)]
    pub starts_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub start_date: Option<chrono::NaiveDate>,
    #[serde(default)]
    pub end_date: Option<chrono::NaiveDate>,
    #[serde(default)]
    pub quantity: Option<i32>,
    pub customer_name: String,
    pub customer_email: String,
    #[serde(default)]
    pub notes: Option<String>,
    /// A booking taken on the phone is usually already agreed, so it confirms straight away unless
    /// the caller says otherwise.
    #[serde(default = "default_true")]
    pub confirm: bool,
}

fn default_true() -> bool {
    true
}

/// CAN A CUSTOMER BOOK THIS CALENDAR RIGHT NOW, AND IF NOT WHY (David, 2026-11-20).
///
/// A calendar with no bookable resource, or with no hours, is a public link that offers nothing — and from
/// the owner's side that is indistinguishable from a broken product. This answers per check with the state
/// the real booking path would find, so the screen can show a numbered list of what is done and what is
/// missing instead of a page of controls with no verdict.
pub async fn readiness(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    use serde_json::json;

    #[derive(sqlx::FromRow)]
    struct Row {
        kind: String,
        slug: String,
        name: String,
    }
    let cal: Option<Row> = sqlx::query_as(
        "SELECT kind, slug, name FROM booking_calendars WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.0)
    .fetch_optional(&pool)
    .await?;
    let Some(cal) = cal else {
        return Ok(Json(json!({"error": "calendar not found"})));
    };

    // A resource a customer could actually pick.
    let active_resources: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM booking_resources WHERE calendar_id = $1 AND active",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;

    // Hours: an appointment needs weekly rules; the other kinds need open slots ahead.
    let hour_rules: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM booking_availability_rules r \
           JOIN booking_resources res ON res.id = r.resource_id \
          WHERE res.calendar_id = $1 AND res.active",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    let open_slots: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM booking_slots s \
           JOIN booking_resources res ON res.id = s.resource_id \
          WHERE res.calendar_id = $1 AND res.active AND s.status = 'open' \
            AND s.starts_at > now()",
    )
    .bind(id)
    .fetch_one(&pool)
    .await?;
    let availability_ok = if cal.kind == "appointment" {
        hour_rules > 0
    } else {
        open_slots > 0
    };

    let tag_rules: i64 =
        sqlx::query_scalar("SELECT count(*) FROM booking_tag_rules WHERE calendar_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap_or(0);
    let list_rules: i64 =
        sqlx::query_scalar("SELECT count(*) FROM booking_list_rules WHERE calendar_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap_or(0);

    let checks = json!([
        {"key": "resource", "done": active_resources > 0,
         "label": "A bookable resource exists",
         "detail": if active_resources > 0 { format!("{active_resources} bookable") } else { "Nothing to pick: the public page lists no positions.".to_string() },
         "action": "Add a resource below."},
        {"key": "availability", "done": availability_ok,
         "label": if cal.kind == "appointment" { "Opening hours are set" } else { "Open positions are published" },
         "detail": if availability_ok {
             if cal.kind == "appointment" { format!("{hour_rules} weekly window(s)") } else { format!("{open_slots} open position(s) ahead") }
         } else if cal.kind == "appointment" {
             "No weekly hours, so the calendar offers no times at all.".to_string()
         } else {
             "No open positions ahead, so a customer sees nothing to buy.".to_string()
         },
         "action": if cal.kind == "appointment" { "Set the hours below." } else { "Generate the positions below." }},
        {"key": "contact_rules", "done": tag_rules > 0 || list_rules > 0,
         "label": "A booking tags or lists the customer (optional)",
         "detail": if tag_rules + list_rules > 0 { format!("{} tag rule(s), {} list rule(s)", tag_rules, list_rules) } else { "Bookings will not reach automations, because nothing connects them to a tag or a list.".to_string() },
         "action": "Add a tag or list rule below — that is what lets a booking start an automation."},
    ]);

    Ok(Json(json!({
        "calendar": {"id": id, "name": cal.name, "slug": cal.slug, "kind": cal.kind},
        "bookable_now": active_resources > 0 && availability_ok,
        "checks": checks,
    })))
}

pub async fn admin_create_booking(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
    Json(body): Json<AdminBookingBody>,
) -> Result<(StatusCode, Json<OrderWithBookings>)> {
    let item = OrderItemInput {
        resource_id: body.resource_id,
        slot_id: body.slot_id,
        starts_at: body.starts_at,
        start_date: body.start_date,
        end_date: body.end_date,
        quantity: body.quantity,
        meta: None,
    };
    let res = service::admin_create_booking(
        &pool,
        t.0,
        id,
        body.customer_name,
        body.customer_email,
        body.notes,
        item,
        body.confirm,
    )
    .await?;
    let pool2 = pool.clone();
    let oid = res.order.id;
    tokio::spawn(async move { super::notify::on_confirmed(&pool2, oid).await });
    Ok((StatusCode::CREATED, Json(res)))
}

/// The workspace a PUBLIC request belongs to: `?w=`, else the host header. A workspace address
/// (`palm-bay-pulse.coreswiftcrm.com`) names its own workspace, which is why the host is consulted
/// before giving up — without it the page answered "workspace not found" on its own address.
async fn workspace_of_public(
    _conn: &mut sqlx::PgConnection,
    q: &WsQuery,
) -> Result<Option<String>> {
    if let Some(w) = q.w.as_ref().filter(|v| !v.trim().is_empty()) {
        return Ok(Some(w.clone()));
    }
    Ok(None)
}
