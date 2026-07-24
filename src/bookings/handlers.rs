use axum::{
    extract::{Path, Query, State, Extension},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::scoring::engine;

use super::models::*;

// ── Calendar CRUD ────────────────────────────────────────────────────────

pub async fn list_calendars(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let calendars = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!(calendars)))
}

pub async fn create_calendar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CreateCalendarRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let cal = sqlx::query_as::<_, BookingCalendar>(
        r#"INSERT INTO booking_calendars (tenant_id, name, slug, description, calendar_type, metadata)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id, tenant_id, name, slug, description, calendar_type, metadata, is_active, created_at, updated_at"#
    )
    .bind(tid)
    .bind(&body.name)
    .bind(&body.slug)
    .bind(&body.description)
    .bind(body.calendar_type.as_deref().unwrap_or("generic"))
    .bind(body.metadata)
    .fetch_one(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(cal))))
}

/// Internal calendar creation — validated by x-internal-key header, no JWT.
pub async fn internal_create_calendar(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateCalendarRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validate internal key
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key != s.config.internal_sync_key {
        return Err(AppError::Unauthorized);
    }

    // tenant_id is extracted from the request body for internal endpoints
    let _tenant_id = Uuid::parse_str(body.metadata.as_ref()
        .and_then(|m| m.get("tenant_id"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("metadata.tenant_id required for internal creation".into()))?)
        .map_err(|_| AppError::BadRequest("Invalid tenant_id in metadata".into()))?;

    let insert_result = sqlx::query_as::<_, BookingCalendar>(
        r#"INSERT INTO booking_calendars (tenant_id, name, slug, description, calendar_type, metadata)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING id, tenant_id, name, slug, description, calendar_type, metadata, is_active, created_at, updated_at"#
    )
    .bind(_tenant_id)
    .bind(&body.name)
    .bind(&body.slug)
    .bind(&body.description)
    .bind(body.calendar_type.as_deref().unwrap_or("city"))
    .bind(body.metadata)
    .fetch_one(&s.db)
    .await;

    match insert_result {
        Ok(cal) => Ok((StatusCode::CREATED, Json(json!(cal)))),
        Err(sqlx::Error::Database(ref db_err)) if db_err.constraint() == Some("booking_calendars_tenant_id_slug_key") => {
            // Calendar already exists — return success with existing calendar
            let existing = sqlx::query_as::<_, BookingCalendar>(
                "SELECT * FROM booking_calendars WHERE tenant_id = $1 AND slug = $2"
            )
            .bind(_tenant_id)
            .bind(&body.slug)
            .fetch_one(&s.db)
            .await?;
            Ok((StatusCode::OK, Json(json!(existing))))
        },
        Err(e) => Err(e.into()),
    }
}

/// Internal default slot creation — creates a default "Appointment Booking" slot
/// for a given calendar. Validated by x-internal-key header, no JWT.
/// Accepts: { tenant_id, calendar_slug, slot_name, total_slots, default_duration_days }
pub async fn internal_create_default_slot(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    // Validate internal key
    let key = headers.get("x-internal-key").and_then(|v| v.to_str().ok()).unwrap_or("");
    if key != s.config.internal_sync_key {
        return Err(AppError::Unauthorized);
    }

    let tenant_id = body.get("tenant_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| AppError::BadRequest("tenant_id required as UUID string".into()))?;

    let calendar_slug = body.get("calendar_slug")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("calendar_slug required".into()))?;

    let slot_name = body.get("slot_name")
        .and_then(|v| v.as_str())
        .unwrap_or("Appointment Booking");

    let total_slots = body.get("total_slots")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1) as i32;

    let default_duration_days = body.get("default_duration_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(1) as i32;

    // Find calendar by slug and tenant_id
    let cal = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 AND slug = $2"
    )
    .bind(tenant_id)
    .bind(calendar_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?;

    // Check if slot already exists with this name for this calendar
    let existing = sqlx::query_scalar::<_, i32>(
        "SELECT COUNT(*) FROM calendar_slots WHERE calendar_id = $1 AND slot_name = $2"
    )
    .bind(cal.id)
    .bind(slot_name)
    .fetch_one(&s.db)
    .await
    .unwrap_or(0);

    if existing > 0 {
        return Ok((StatusCode::CONFLICT, Json(json!({
            "message": "Slot already exists",
            "slot_name": slot_name,
        }))));
    }

    // Get max sort_order
    let max_order: (Option<i32>,) = sqlx::query_as(
        "SELECT MAX(sort_order) FROM calendar_slots WHERE calendar_id = $1"
    )
    .bind(cal.id)
    .fetch_one(&s.db)
    .await?;

    let slot = sqlx::query_as::<_, CalendarSlot>(
        r#"INSERT INTO calendar_slots (calendar_id, slot_name, total_slots, filled_slots, default_duration_days, sort_order)
           VALUES ($1, $2, $3, 0, $4, $5)
           RETURNING *"#
    )
    .bind(cal.id)
    .bind(slot_name)
    .bind(total_slots)
    .bind(default_duration_days)
    .bind(max_order.0.unwrap_or(0) + 1)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(slot))))
}

pub async fn get_calendar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let cal = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 AND slug = $2"
    )
    .bind(tid)
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?;
    Ok(Json(json!(cal)))
}

pub async fn update_calendar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(slug): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let existing = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 AND slug = $2"
    )
    .bind(tid)
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?;

    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or(&existing.name);
    let desc = body.get("description").and_then(|v| v.as_str()).or(existing.description.as_deref());
    let meta = body.get("metadata").cloned().or(existing.metadata);

    let cal = sqlx::query_as::<_, BookingCalendar>(
        r#"UPDATE booking_calendars SET name = $1, description = $2, metadata = $3, updated_at = NOW()
           WHERE id = $4 RETURNING id, tenant_id, name, slug, description, calendar_type, metadata, is_active, created_at, updated_at"#
    )
    .bind(name)
    .bind(desc)
    .bind(meta)
    .bind(existing.id)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(json!(cal)))
}

// ── Slot Types ───────────────────────────────────────────────────────────

pub async fn list_slots(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let slots = sqlx::query_as::<_, CalendarSlot>(
        r#"SELECT cs.* FROM calendar_slots cs
           JOIN booking_calendars bc ON cs.calendar_id = bc.id
           WHERE bc.tenant_id = $1 AND bc.slug = $2
           ORDER BY cs.sort_order"#
    )
    .bind(tid)
    .bind(&slug)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!(slots)))
}

pub async fn create_slot(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(slug): Path<String>,
    Json(body): Json<CreateSlotRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let cal = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE tenant_id = $1 AND slug = $2"
    )
    .bind(tid)
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?;

    let max_order: (Option<i32>,) = sqlx::query_as(
        "SELECT MAX(sort_order) FROM calendar_slots WHERE calendar_id = $1"
    )
    .bind(cal.id)
    .fetch_one(&s.db)
    .await?;

    let slot = sqlx::query_as::<_, CalendarSlot>(
        r#"INSERT INTO calendar_slots (calendar_id, slot_name, total_slots, filled_slots, default_duration_days, price_override, coreswift_tag_template, coreswift_list_id, sort_order)
           VALUES ($1, $2, $3, 0, $4, $5, $6, $7, $8)
           RETURNING *"#
    )
    .bind(cal.id)
    .bind(&body.slot_name)
    .bind(body.total_slots)
    .bind(body.default_duration_days)
    .bind(body.price_override.map(|p| rust_decimal::Decimal::try_from(p).unwrap_or_default()))
    .bind(&body.coreswift_tag_template)
    .bind(body.coreswift_list_id)
    .bind(max_order.0.unwrap_or(0) + 1)
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(slot))))
}

pub async fn update_slot(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path((slug, slot_id)): Path<(String, Uuid)>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let _existing = sqlx::query_as::<_, CalendarSlot>(
        r#"SELECT cs.* FROM calendar_slots cs
           JOIN booking_calendars bc ON cs.calendar_id = bc.id
           WHERE bc.tenant_id = $1 AND bc.slug = $2 AND cs.id = $3"#
    )
    .bind(tid)
    .bind(&slug)
    .bind(slot_id)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Slot not found".into()))?;

    let slot = sqlx::query_as::<_, CalendarSlot>(
        r#"UPDATE calendar_slots SET
            slot_name = COALESCE($1, slot_name),
            total_slots = COALESCE($2, total_slots),
            default_duration_days = COALESCE($3, default_duration_days),
            price_override = COALESCE($4, price_override),
            coreswift_tag_template = COALESCE($5, coreswift_tag_template),
            updated_at = NOW()
           WHERE id = $6 RETURNING *"#
    )
    .bind(body.get("slot_name").and_then(|v| v.as_str()))
    .bind(body.get("total_slots").and_then(|v| v.as_i64()).map(|v| v as i32))
    .bind(body.get("default_duration_days").and_then(|v| v.as_i64()).map(|v| v as i32))
    .bind(body.get("price_override").and_then(|v| v.as_f64()).map(|v| rust_decimal::Decimal::try_from(v).unwrap_or_default()))
    .bind(body.get("coreswift_tag_template").and_then(|v| v.as_str()))
    .bind(slot_id)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!(slot)))
}

pub async fn delete_slot(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path((slug, slot_id)): Path<(String, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let result = sqlx::query(
        r#"DELETE FROM calendar_slots cs USING booking_calendars bc
           WHERE cs.calendar_id = bc.id AND bc.tenant_id = $1 AND bc.slug = $2 AND cs.id = $3"#
    )
    .bind(tid)
    .bind(&slug)
    .bind(slot_id)
    .execute(&s.db)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Slot not found".into()));
    }
    Ok(Json(json!({"message": "Slot deleted"})))
}

// ── Questions & Availability ─────────────────────────────────────────────

pub async fn get_questions(
    State(_s): State<AppState>,
    Extension(_c): Extension<Claims>,
    Path(_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let questions = BookingQuestionsResponse {
        questions: vec![
            BookingQuestion { key: "business_name".into(), label: "Business Name".into(), r#type: "text".into(), required: true, options: None },
            BookingQuestion { key: "contact_name".into(), label: "Your Name".into(), r#type: "text".into(), required: true, options: None },
            BookingQuestion { key: "contact_email".into(), label: "Email Address".into(), r#type: "email".into(), required: true, options: None },
            BookingQuestion { key: "contact_phone".into(), label: "Phone Number".into(), r#type: "tel".into(), required: false, options: None },
            BookingQuestion { key: "website".into(), label: "Business Website".into(), r#type: "url".into(), required: false, options: None },
            BookingQuestion { key: "description".into(), label: "Tell us about your business / ad copy".into(), r#type: "textarea".into(), required: true, options: None },
            BookingQuestion { key: "target_audience".into(), label: "Target Audience".into(), r#type: "text".into(), required: false, options: None },
            BookingQuestion { key: "call_booking".into(), label: "Would you like a sales call to discuss details?".into(), r#type: "select".into(), required: false, options: Some(vec!["No, proceed with booking".into(), "Yes, call me back".into(), "Email me instead".into()]) },
        ],
    };
    Ok(Json(json!(questions)))
}

#[derive(Deserialize)]
pub struct AvailableQuery {
    pub start: Option<String>,
    pub end: Option<String>,
}

pub async fn get_available(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(slug): Path<String>,
    Query(_query): Query<AvailableQuery>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let slots = sqlx::query_as::<_, CalendarSlot>(
        r#"SELECT cs.* FROM calendar_slots cs
           JOIN booking_calendars bc ON cs.calendar_id = bc.id
           WHERE bc.tenant_id = $1 AND bc.slug = $2 AND cs.is_active = true
           ORDER BY cs.sort_order"#
    )
    .bind(tid)
    .bind(&slug)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(slots)))
}

// ── Public (no-auth) endpoints for embedded booking widgets ──────────────

#[derive(Deserialize)]
pub struct PublicTenantQuery {
    pub tenant: Option<String>,
}

pub async fn public_questions(
    Query(_q): Query<PublicTenantQuery>,
) -> ApiResult<impl IntoResponse> {
    let questions = BookingQuestionsResponse {
        questions: vec![
            BookingQuestion { key: "business_name".into(), label: "Business Name".into(), r#type: "text".into(), required: true, options: None },
            BookingQuestion { key: "contact_name".into(), label: "Your Name".into(), r#type: "text".into(), required: true, options: None },
            BookingQuestion { key: "contact_email".into(), label: "Email Address".into(), r#type: "email".into(), required: true, options: None },
            BookingQuestion { key: "contact_phone".into(), label: "Phone Number".into(), r#type: "tel".into(), required: false, options: None },
            BookingQuestion { key: "website".into(), label: "Business Website".into(), r#type: "url".into(), required: false, options: None },
            BookingQuestion { key: "description".into(), label: "Tell us about your business / ad copy".into(), r#type: "textarea".into(), required: true, options: None },
            BookingQuestion { key: "target_audience".into(), label: "Target Audience".into(), r#type: "text".into(), required: false, options: None },
            BookingQuestion { key: "call_booking".into(), label: "Would you like a sales call to discuss details?".into(), r#type: "select".into(), required: false, options: Some(vec!["No, proceed with booking".into(), "Yes, call me back".into(), "Email me instead".into()]) },
        ],
    };
    Ok(Json(json!(questions)))
}

pub async fn public_available(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let slots = sqlx::query_as::<_, CalendarSlot>(
        r#"SELECT cs.* FROM calendar_slots cs
           JOIN booking_calendars bc ON cs.calendar_id = bc.id
           WHERE bc.tenant_id = $1 AND bc.is_active = true AND cs.is_active = true
           ORDER BY bc.name, cs.sort_order"#
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(slots)))
}

pub async fn public_create_booking(
    State(s): State<AppState>,
    Json(body): Json<CreateBookingRequest>,
) -> ApiResult<impl IntoResponse> {
    let cal = sqlx::query_as::<_, BookingCalendar>(
        "SELECT * FROM booking_calendars WHERE slug = $1 AND is_active = true"
    )
    .bind(&body.calendar_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".into()))?;

    // Look up slot — by ID if provided, otherwise use slot_name or default to listed
    let slot = if let Some(slot_id) = body.slot_id {
        sqlx::query_as::<_, CalendarSlot>(
            "SELECT * FROM calendar_slots WHERE id = $1 AND calendar_id = $2 AND is_active = true"
        )
        .bind(slot_id)
        .bind(cal.id)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Slot type not found".into()))?
    } else {
        // Try to find by slot_name if provided, else first known slot
        let slot_name = body.slot_name.as_deref().unwrap_or("Featured Listing");
        sqlx::query_as::<_, CalendarSlot>(
            "SELECT * FROM calendar_slots WHERE slot_name = $1 AND calendar_id = $2 AND is_active = true"
        )
        .bind(slot_name)
        .bind(cal.id)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("Slot '{}' not available for this calendar", slot_name)))?
    };

    if slot.total_slots != -1 && slot.filled_slots >= slot.total_slots {
        return Err(AppError::Validation("No slots available".into()));
    }

    let start_date = chrono::NaiveDate::parse_from_str(&body.start_date, "%Y-%m-%d")
        .map_err(|_| AppError::Validation("Invalid start_date format (YYYY-MM-DD)".into()))?;
    let days = body.duration_days.unwrap_or(slot.default_duration_days);
    let end_date = start_date + chrono::Duration::days(days as i64);

    let max_pos: (Option<i32>,) = sqlx::query_as(
        "SELECT MAX(slot_position) FROM slot_bookings WHERE calendar_id = $1 AND slot_id = $2 AND status = 'active'"
    )
    .bind(cal.id)
    .bind(slot.id)
    .fetch_one(&s.db)
    .await?;
    let slot_position = max_pos.0.unwrap_or(0) + 1;

    let mut meta = serde_json::Map::new();
    meta.insert("website".into(), json!(body.website));
    meta.insert("description".into(), json!(body.description));
    meta.insert("target_audience".into(), json!(body.target_audience));
    meta.insert("call_booking".into(), json!(body.call_booking));
    meta.insert("contact_name".into(), json!(body.contact_name));
    meta.insert("source".into(), json!("directory_booking"));

    let booking = sqlx::query_as::<_, SlotBooking>(
        r#"INSERT INTO slot_bookings (tenant_id, calendar_id, slot_id, business_name, contact_name, contact_email, contact_phone,
            website, description, target_audience, call_booking, start_date, end_date, slot_position, status, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, 'active', $15)
           RETURNING *"#
    )
    .bind(cal.tenant_id)
    .bind(cal.id)
    .bind(slot.id)
    .bind(&body.business_name)
    .bind(&body.contact_name)
    .bind(&body.contact_email)
    .bind(&body.contact_phone)
    .bind(&body.website)
    .bind(&body.description)
    .bind(&body.target_audience)
    .bind(&body.call_booking)
    .bind(start_date)
    .bind(end_date)
    .bind(slot_position)
    .bind(serde_json::Value::Object(meta))
    .fetch_one(&s.db)
    .await?;

    // Free booking — increment filled_slots immediately (no payment step)
    sqlx::query("UPDATE calendar_slots SET filled_slots = filled_slots + 1 WHERE id = $1")
        .bind(slot.id)
        .execute(&s.db)
        .await?;

    // Fire scoring event: booking completed → +50 points
    if let Some(cid) = booking.contact_id {
        let _ = engine::calculate_score(&s.db, cal.tenant_id, cid, "booking_completed").await;
    }

    // Round-robin assignment: fire-and-forget so booking response is not delayed
    if let Some(team_id) = crate::round_robin::engine::find_team_for_calendar(&s.db, booking.calendar_id).await {
        let db = s.db.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::round_robin::engine::assign_lead(&db, team_id, Some(booking.id), booking.contact_id).await {
                tracing::error!("round-robin assignment failed: {}", e);
            }
        });
    }

    Ok(Json(json!({
        "booking_id": booking.id,
        "status": "active",
        "slot_position": slot_position,
        "start_date": start_date.to_string(),
        "end_date": end_date.to_string(),
        "tag": slot.coreswift_tag_template.as_ref().map(|t| t.replace("{city}", cal.metadata.as_ref()
            .and_then(|m| m.get("city_slug")).and_then(|v| v.as_str()).unwrap_or("")))
    })))
}

// ── Payment / Checkout ───────────────────────────────────────────────────

pub async fn create_checkout(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let booking_id = body.get("booking_id").and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| AppError::Validation("booking_id required".into()))?;

    let booking = sqlx::query_as::<_, SlotBooking>(
        "UPDATE slot_bookings SET status = 'active' WHERE id = $1 AND tenant_id = $2 RETURNING *"
    )
    .bind(booking_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Booking not found".into()))?;

    sqlx::query("UPDATE calendar_slots SET filled_slots = filled_slots + 1 WHERE id = $1")
        .bind(booking.slot_id)
        .execute(&s.db)
        .await?;

    // Fire scoring event: booking completed → +50 points
    if let Some(cid) = booking.contact_id {
        let _ = engine::calculate_score(&s.db, tid, cid, "booking_completed").await;
    }

    // Round-robin assignment: fire-and-forget so booking response is not delayed
    if let Some(team_id) = crate::round_robin::engine::find_team_for_calendar(&s.db, booking.calendar_id).await {
        let db = s.db.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::round_robin::engine::assign_lead(&db, team_id, Some(booking.id), booking.contact_id).await {
                tracing::error!("round-robin assignment failed: {}", e);
            }
        });
    }

    Ok(Json(json!({"success": true, "status": "active"})))
}

// ── Booking Management (Admin) ──────────────────────────────────────────

pub async fn list_bookings(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let bookings = sqlx::query_as::<_, SlotBooking>(
        "SELECT * FROM slot_bookings WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!(bookings)))
}

pub async fn get_booking(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let booking = sqlx::query_as::<_, SlotBooking>(
        "SELECT * FROM slot_bookings WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Booking not found".into()))?;

    Ok(Json(json!(booking)))
}

pub async fn cancel_booking(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let booking = sqlx::query_as::<_, SlotBooking>(
        "UPDATE slot_bookings SET status = 'cancelled', updated_at = NOW() WHERE id = $1 AND tenant_id = $2 RETURNING *"
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Booking not found".into()))?;

    sqlx::query("UPDATE calendar_slots SET filled_slots = GREATEST(filled_slots - 1, 0) WHERE id = $1")
        .bind(booking.slot_id)
        .execute(&s.db)
        .await?;

    Ok(Json(json!({"success": true, "status": "cancelled"})))
}

pub async fn adjust_slot_config(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let slot = sqlx::query_as::<_, CalendarSlot>(
        r#"UPDATE calendar_slots cs SET
            total_slots = COALESCE($1, cs.total_slots),
            updated_at = NOW()
           FROM booking_calendars bc
           WHERE cs.calendar_id = bc.id AND bc.tenant_id = $2 AND cs.id = $3
           RETURNING cs.*"#
    )
    .bind(body.get("total_slots").and_then(|v| v.as_i64()).map(|v| v as i32))
    .bind(tid)
    .bind(id)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Slot not found".into()))?;

    Ok(Json(json!(slot)))
}
