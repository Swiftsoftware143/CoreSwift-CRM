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
    Ok(Json(json!({
        "calendar": { "name": cal.name, "slug": cal.slug, "kind": cal.kind, "timezone": cal.timezone,
                      "currency": cal.currency, "hold_minutes": cal.hold_minutes, "settings": cal.settings },
        "resources": resources,
        "bundles": bundles,
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
