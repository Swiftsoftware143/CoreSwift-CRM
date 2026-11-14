use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use std::collections::HashMap;
use uuid::Uuid;

/// Inserted into request extensions by the host app's auth middleware (admin routes only).
#[derive(Debug, Clone, Copy)]
pub struct TenantId(pub Uuid);

/// To add a calendar type: add a variant here, in the migration CHECK, and one arm in
/// `service::resolve_item`, `service::check_capacity` and `service::availability`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarKind {
    /// Calendly-style: times generated from weekly availability rules.
    Appointment,
    /// Fixed, individually priced slots (newsletter ad placements).
    Inventory,
    /// Multi-day bookings priced per day (directory features, rentals).
    DateRange,
}

impl CalendarKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Appointment => "appointment",
            Self::Inventory => "inventory",
            Self::DateRange => "date_range",
        }
    }
}

impl TryFrom<String> for CalendarKind {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        match s.as_str() {
            "appointment" => Ok(Self::Appointment),
            "inventory" => Ok(Self::Inventory),
            "date_range" => Ok(Self::DateRange),
            other => Err(format!("unknown calendar kind: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Calendar {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    #[sqlx(try_from = "String")]
    pub kind: CalendarKind,
    pub timezone: String,
    pub currency: String,
    pub hold_minutes: i32,
    pub settings: Value,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Resource {
    pub id: Uuid,
    pub calendar_id: Uuid,
    pub name: String,
    pub description: String,
    pub capacity: i32,
    pub price_cents: i64,
    pub duration_min: i32,
    pub slot_interval_min: i32,
    pub buffer_min: i32,
    pub min_notice_min: i32,
    pub sort_order: i32,
    pub metadata: Value,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct AvailabilityRule {
    /// 0 = Monday .. 6 = Sunday
    pub weekday: i16,
    pub start_time: NaiveTime,
    pub end_time: NaiveTime,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Slot {
    pub id: Uuid,
    pub resource_id: Uuid,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub capacity: i32,
    pub price_cents: i64,
    pub label: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Bundle {
    pub id: Uuid,
    pub calendar_id: Uuid,
    pub name: String,
    /// "percent_off" | "fixed_price"
    pub kind: String,
    pub min_items: i32,
    pub percent_off: i32,
    pub fixed_price_cents: i64,
    pub eligible_resource_ids: Vec<Uuid>,
    pub required_resource_ids: Vec<Uuid>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Order {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub calendar_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub customer_name: String,
    pub customer_email: String,
    pub status: String,
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub total_cents: i64,
    pub bundle_id: Option<Uuid>,
    pub hold_expires_at: DateTime<Utc>,
    pub payment_ref: Option<String>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub order_id: Uuid,
    pub resource_id: Uuid,
    pub slot_id: Option<Uuid>,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub quantity: i32,
    pub price_cents: i64,
    pub status: String,
    pub hold_expires_at: DateTime<Utc>,
    pub meta: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct OrderWithBookings {
    #[serde(flatten)]
    pub order: Order,
    pub bookings: Vec<Booking>,
}

/// One bookable opening returned by the availability endpoint (all calendar kinds).
#[derive(Debug, Clone, Serialize)]
pub struct Opening {
    pub resource_id: Uuid,
    pub resource_name: String,
    /// Set for inventory calendars; pass it back as `slot_id` when ordering.
    pub slot_id: Option<Uuid>,
    pub label: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub price_cents: i64,
    pub remaining: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Quote {
    pub subtotal_cents: i64,
    pub discount_cents: i64,
    pub total_cents: i64,
    pub bundle_id: Option<Uuid>,
    pub bundle_name: Option<String>,
}

// ---------- request payloads ----------

#[derive(Debug, Deserialize)]
pub struct NewCalendar {
    pub name: String,
    pub slug: String,
    pub kind: CalendarKind,
    pub timezone: Option<String>,
    pub currency: Option<String>,
    pub hold_minutes: Option<i32>,
    pub settings: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct NewResource {
    pub name: String,
    pub description: Option<String>,
    pub capacity: Option<i32>,
    pub price_cents: Option<i64>,
    pub duration_min: Option<i32>,
    pub slot_interval_min: Option<i32>,
    pub buffer_min: Option<i32>,
    pub min_notice_min: Option<i32>,
    pub sort_order: Option<i32>,
    pub metadata: Option<Value>,
}

/// Bulk-create inventory slots, e.g. every Tue/Thu issue for a quarter.
#[derive(Debug, Deserialize)]
pub struct GenerateSlots {
    pub from: NaiveDate,
    /// inclusive
    pub to: NaiveDate,
    /// 0 = Monday .. 6 = Sunday
    pub weekdays: Vec<u32>,
    /// Local send/publish time in the calendar timezone.
    pub time: NaiveTime,
    pub duration_min: Option<i32>,
    pub capacity: Option<i32>,
    /// Defaults to the resource price.
    pub price_cents: Option<i64>,
    /// Per-weekday override, e.g. {"1": 15000} makes Tuesdays $150.
    #[serde(default)]
    pub weekday_prices: HashMap<u32, i64>,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSlot {
    pub price_cents: Option<i64>,
    pub capacity: Option<i32>,
    pub label: Option<String>,
    /// "open" | "closed"
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewBundle {
    pub name: String,
    pub kind: String,
    pub min_items: i32,
    pub percent_off: Option<i32>,
    pub fixed_price_cents: Option<i64>,
    #[serde(default)]
    pub eligible_resource_ids: Vec<Uuid>,
    #[serde(default)]
    pub required_resource_ids: Vec<Uuid>,
}

/// What to send depends on the calendar kind:
///   inventory   -> slot_id
///   appointment -> starts_at
///   date_range  -> start_date + end_date (end exclusive)
#[derive(Debug, Clone, Deserialize)]
pub struct OrderItemInput {
    pub resource_id: Uuid,
    pub slot_id: Option<Uuid>,
    pub starts_at: Option<DateTime<Utc>>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub quantity: Option<i32>,
    pub meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct QuoteRequest {
    pub items: Vec<OrderItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct NewOrder {
    pub customer_name: String,
    pub customer_email: String,
    pub contact_id: Option<Uuid>,
    pub notes: Option<String>,
    pub items: Vec<OrderItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmOrder {
    pub payment_ref: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    pub from: NaiveDate,
    /// inclusive
    pub to: NaiveDate,
}

#[derive(Debug, Deserialize)]
pub struct CreativeReview {
    pub approved: bool,
    pub note: Option<String>,
}
