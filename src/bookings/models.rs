use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct BookingCalendar {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub calendar_type: String,           // city, product, generic
    pub metadata: Option<serde_json::Value>,  // city_slug, directory_id, etc
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CalendarSlot {
    pub id: Uuid,
    pub calendar_id: Uuid,
    pub slot_name: String,               // Featured Listing, Banner Ad, etc
    pub total_slots: i32,                // -1 = unlimited
    pub filled_slots: i32,
    pub default_duration_days: i32,
    pub price_override: Option<rust_decimal::Decimal>,
    pub coreswift_tag_template: Option<String>,
    pub coreswift_list_id: Option<Uuid>,
    pub is_active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SlotBooking {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub calendar_id: Uuid,
    pub slot_id: Uuid,
    pub contact_id: Option<Uuid>,
    pub business_name: String,
    pub contact_name: Option<String>,
    pub contact_email: String,
    pub contact_phone: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
    pub target_audience: Option<String>,
    pub call_booking: Option<String>,    // Yes call, Email instead, No
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub slot_position: i32,
    pub status: String,                   // active, cancelled, expired
    pub price_paid: Option<rust_decimal::Decimal>,
    pub currency: String,
    pub stripe_payment_intent_id: Option<String>,
    pub stripe_subscription_id: Option<String>,
    pub metadata: Option<serde_json::Value>, // uploaded files, Q&A answers, checkout session
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateCalendarRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub calendar_type: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSlotRequest {
    pub slot_name: String,
    pub total_slots: i32,
    pub default_duration_days: i32,
    pub price_override: Option<f64>,
    pub coreswift_tag_template: Option<String>,
    pub coreswift_list_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateBookingRequest {
    pub calendar_slug: String,
    pub slot_id: Option<Uuid>,
    pub slot_name: Option<String>,  // fallback: match on slot name if slot_id omitted
    pub contact_id: Option<Uuid>,
    pub business_name: String,
    pub contact_name: Option<String>,
    pub contact_email: String,
    pub contact_phone: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
    pub target_audience: Option<String>,
    pub call_booking: Option<String>,
    pub start_date: String,   // YYYY-MM-DD
    pub duration_days: Option<i32>,
    pub stripe_payment_intent_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookingQuestionsResponse {
    pub questions: Vec<BookingQuestion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookingQuestion {
    pub key: String,
    pub label: String,
    pub r#type: String,
    pub required: bool,
    pub options: Option<Vec<String>>,
}
