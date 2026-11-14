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
) -> Result<Json<Vec<Calendar>>> {
    let mut c = pool.acquire().await?;
    Ok(Json(service::list_calendars(&mut c, t.0).await?))
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
        service::availability(&mut c, &cal, q.from, q.to).await?,
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
    Ok(Json(
        service::confirm_order(&pool, id, Some(t.0), payment_ref).await?,
    ))
}

pub async fn cancel_order(
    State(pool): State<PgPool>,
    Extension(t): Extension<TenantId>,
    Path(id): Path<Uuid>,
) -> Result<Json<OrderWithBookings>> {
    Ok(Json(service::cancel_order(&pool, id, Some(t.0)).await?))
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

pub async fn public_calendar(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
) -> Result<Json<Value>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_by_slug(&mut c, &slug).await?;
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
) -> Result<Json<Vec<Opening>>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_by_slug(&mut c, &slug).await?;
    Ok(Json(
        service::availability(&mut c, &cal, q.from, q.to).await?,
    ))
}

pub async fn public_quote(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Json(body): Json<QuoteRequest>,
) -> Result<Json<Quote>> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_by_slug(&mut c, &slug).await?;
    Ok(Json(service::quote(&mut c, &cal, &body.items).await?))
}

pub async fn public_create_order(
    State(pool): State<PgPool>,
    Path(slug): Path<String>,
    Json(body): Json<NewOrder>,
) -> Result<(StatusCode, Json<OrderWithBookings>)> {
    let mut c = pool.acquire().await?;
    let cal = service::calendar_by_slug(&mut c, &slug).await?;
    drop(c);
    Ok((
        StatusCode::CREATED,
        Json(service::create_order(&pool, &cal, body).await?),
    ))
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
