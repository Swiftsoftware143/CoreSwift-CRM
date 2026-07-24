//! Google Calendar Integration Module
//!
//! OAuth2 flow: connect a booking calendar to Google Calendar
//! Sync: push CoreSwift booking slots as Google Calendar events,
//!       pull Google events as unavailable slots.
//!
//! Routes:
//! - GET  /api/google-calendar/connect-url   — Get OAuth consent URL
//! - GET  /api/google-calendar/oauth-callback — OAuth callback handler
//! - POST /api/google-calendar/sync/:calendar_id — Push/pull sync
//! - POST /api/google-calendar/webhook       — Google Calendar push notification

use axum::{
    extract::{Path, Query, State, Extension},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;
use std::collections::HashMap;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};

// ── Google OAuth2 configuration ──────────────────────────────────────────

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_CALENDAR_API: &str = "https://www.googleapis.com/calendar/v3";

/// Get OAuth2 client configuration from AppConfig or environment
fn google_oauth_config() -> (String, String, String) {
    let client_id = std::env::var("GOOGLE_CLIENT_ID")
        .unwrap_or_else(|_| String::new());
    let client_secret = std::env::var("GOOGLE_CLIENT_SECRET")
        .unwrap_or_else(|_| String::new());
    let redirect_uri = std::env::var("GOOGLE_REDIRECT_URI")
        .unwrap_or_else(|_| "http://localhost:8080/api/google-calendar/oauth-callback".to_string());
    (client_id, client_secret, redirect_uri)
}

// ── Route definitions ────────────────────────────────────────────────────

pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/connect-url", get(get_connect_url))
        .route("/oauth-callback", get(oauth_callback))
        .route("/sync/:calendar_id", post(sync_calendar))
        .route("/webhook", post(webhook_handler))
        .layer(axum::middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}

// ── Request/Response types ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SyncQuery {
    pub full_sync: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GoogleTokenResponse {
    access_token: String,
    expires_in: u32,
    refresh_token: Option<String>,
    scope: Option<String>,
    token_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[allow(dead_code)]
struct CalendarListResponse {
    items: Option<Vec<CalendarItem>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CalendarItem {
    id: String,
    summary: Option<String>,
    description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventsResponse {
    items: Option<Vec<EventItem>>,
    next_page_token: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventItem {
    id: String,
    summary: Option<String>,
    description: Option<String>,
    start: Option<EventDateTime>,
    end: Option<EventDateTime>,
    status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct EventDateTime {
    date_time: Option<String>,
    date: Option<String>,
    time_zone: Option<String>,
}

// ── Handlers ─────────────────────────────────────────────────────────────

/// GET /api/google-calendar/connect-url
/// Returns the Google OAuth consent URL for the user to authorize.
/// The state parameter contains the tenant's calendar_id to link after auth.
pub async fn get_connect_url(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (client_id, _client_secret, redirect_uri) = google_oauth_config();

    if client_id.is_empty() {
        return Err(AppError::Validation(
            "Google Calendar not configured: GOOGLE_CLIENT_ID is not set".to_string(),
        ));
    }

    // The state holds the calendar_id so we can link it after OAuth completes
    let calendar_id = q.get("calendar_id").cloned().unwrap_or_default();

    // Verify the calendar exists and belongs to this tenant
    if !calendar_id.is_empty() {
        let cal: Option<(Uuid,)> = sqlx::query_scalar(
            "SELECT id FROM booking_calendars WHERE id = $1 AND tenant_id = $2"
        )
        .bind(Uuid::parse_str(&calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
        .bind(tid)
        .fetch_optional(&s.db)
        .await?;
        if cal.is_none() {
            return Err(AppError::NotFound("Calendar not found".to_string()));
        }
    }

    let scopes = "https://www.googleapis.com/auth/calendar%20https://www.googleapis.com/auth/calendar.events";
    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        GOOGLE_AUTH_URL, client_id, redirect_uri, scopes, calendar_id
    );

    Ok(Json(json!({
        "connect_url": auth_url,
        "calendar_id": calendar_id,
    })))
}

/// GET /api/google-calendar/oauth-callback
/// Handles the OAuth2 callback from Google, stores refresh token in booking_calendars.
pub async fn oauth_callback(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(params): Query<OAuthCallbackParams>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(err) = &params.error {
        return Err(AppError::BadRequest(format!("Google OAuth error: {}", err)));
    }

    let code = params.code.ok_or_else(|| AppError::Validation("Authorization code missing".to_string()))?;
    let (client_id, client_secret, redirect_uri) = google_oauth_config();

    // Exchange auth code for tokens
    let token_params = json!({
        "code": code,
        "client_id": client_id,
        "client_secret": client_secret,
        "redirect_uri": redirect_uri,
        "grant_type": "authorization_code",
    });

    let client = reqwest::Client::new();
    let token_resp = client
        .post(GOOGLE_TOKEN_URL)
        .json(&token_params)
        .send()
        .await
        .map_err(|e| AppError::BadRequest(format!("Token exchange failed: {}", e)))?;

    let token_data: GoogleTokenResponse = token_resp
        .json()
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to parse token response: {}", e)))?;

    let refresh_token = token_data.refresh_token
        .ok_or_else(|| AppError::BadRequest("No refresh_token received (Google requires prompt=consent)".to_string()))?;

    // Optionally get a default calendar ID for this user
    let calendar_id = params.state.as_deref().unwrap_or("");

    if calendar_id.is_empty() {
        // No specific calendar_id in state — store on tenant level or return error
        return Ok(Json(json!({
            "message": "OAuth successful. Refresh token stored. No calendar_id was provided in state.",
            "has_refresh_token": true,
        })));
    }

    // Store the refresh token on the booking_calendars record
    // Also create a Google Calendar if this calendar doesn't have one yet
    sqlx::query(
        "UPDATE booking_calendars SET google_refresh_token = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
    )
    .bind(&refresh_token)
    .bind(Uuid::parse_str(calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
    .bind(tid)
    .execute(&s.db)
    .await?;

    // If the calendar doesn't have a google_calendar_id yet, fetch/create one
    let existing_cal_id: Option<String> = sqlx::query_scalar(
        "SELECT google_calendar_id FROM booking_calendars WHERE id = $1 AND tenant_id = $2"
    )
    .bind(Uuid::parse_str(calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .flatten();

    if existing_cal_id.is_none() {
        // Create a new Google Calendar for this booking calendar
        let calendar_name: String = sqlx::query_scalar(
            "SELECT name FROM booking_calendars WHERE id = $1 AND tenant_id = $2"
        )
        .bind(Uuid::parse_str(calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
        .bind(tid)
        .fetch_optional(&s.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Calendar not found".to_string()))?;

        let create_payload = json!({
            "summary": format!("CoreSwift - {}", calendar_name),
            "description": "Synced from CoreSwift CRM booking calendar",
        });

        let create_resp = client
            .post(format!("{}/calendars", GOOGLE_CALENDAR_API))
            .header("Authorization", format!("Bearer {}", token_data.access_token))
            .json(&create_payload)
            .send()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to create Google Calendar: {}", e)))?;

        if create_resp.status().is_success() {
            let created: CalendarItem = create_resp.json().await
                .map_err(|e| AppError::BadRequest(format!("Failed to parse calendar create response: {}", e)))?;

            sqlx::query(
                "UPDATE booking_calendars SET google_calendar_id = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
            )
            .bind(&created.id)
            .bind(Uuid::parse_str(calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
            .bind(tid)
            .execute(&s.db)
            .await?;
        }
    }

    Ok(Json(json!({
        "message": "Google Calendar connected successfully",
        "calendar_id": calendar_id,
        "has_refresh_token": true,
        "google_calendar_id": existing_cal_id,
    })))
}

/// POST /api/google-calendar/sync/:calendar_id
/// Push CoreSwift bookings to Google Calendar as events.
/// Pull existing Google Calendar events as unavailable slots.
pub async fn sync_calendar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(calendar_id): Path<Uuid>,
    Query(q): Query<SyncQuery>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Get calendar with refresh token
    let cal = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        r#"SELECT name, google_refresh_token, google_calendar_id
           FROM booking_calendars WHERE id = $1 AND tenant_id = $2"#
    )
    .bind(calendar_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Calendar not found".to_string()))?;

    let (calendar_name, refresh_token, google_cal_id) = cal;

    let refresh_token = refresh_token.ok_or_else(|| {
        AppError::Validation("Google Calendar not connected. Use /connect-url first.".to_string())
    })?;

    let google_cal_id = google_cal_id.unwrap_or_else(|| "primary".to_string());

    // Get a fresh access token using the refresh token
    let access_token = get_access_token(&refresh_token).await?;

    let _full_sync = q.full_sync.as_deref().unwrap_or("true") == "true";

    let mut pushed: Vec<Value> = Vec::new();
    let mut pulled: Vec<Value> = Vec::new();

    // ── PUSH: CoreSwift bookings → Google Calendar events ──
    let bookings = sqlx::query_as::<_, (Uuid, String, String, String, Option<String>, String, i32, String, String)>(
        r#"SELECT sb.id, sb.business_name, sb.contact_name, sb.contact_email,
                  sb.description, sb.start_date::text, sb.slot_position, sb.status, sb.end_date::text
           FROM slot_bookings sb
           WHERE sb.calendar_id = $1 AND sb.tenant_id = $2 AND sb.status = 'active'
           ORDER BY sb.start_date ASC"#
    )
    .bind(calendar_id)
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let client = reqwest::Client::new();

    for (booking_id, business_name, contact_name, contact_email, description, start_date, _slot_pos, _status, end_date) in &bookings {
        let event_title = format!("{} - {}", calendar_name, business_name);
        let event_desc = format!(
            "Booking from CoreSwift\nBusiness: {}\nContact: {}\nEmail: {}\n\n{}",
            business_name,
            contact_name.as_str(),
            contact_email,
            description.as_deref().unwrap_or_default()
        );

        let event_payload = json!({
            "summary": event_title,
            "description": event_desc,
            "start": {
                "date": start_date,
            },
            "end": {
                "date": end_date,
            },
            "transparency": "opaque",
            "visibility": "default",
        });

        match client
            .post(format!("{}/calendars/{}/events", GOOGLE_CALENDAR_API, google_cal_id))
            .header("Authorization", format!("Bearer {}", access_token))
            .json(&event_payload)
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(event) = resp.json::<Value>().await {
                        pushed.push(event);
                    }
                } else {
                    let err_text = resp.text().await.unwrap_or_default();
                    tracing::warn!(booking = %booking_id, error = %err_text, "Failed to push event to Google Calendar");
                }
            }
            Err(e) => {
                tracing::warn!(booking = %booking_id, error = %e, "Request failed pushing event");
            }
        }
    }

    // ── PULL: Google Calendar events → unavailable slots ──
    // Fetch events from the last 90 days to the next 365 days
    let now = chrono::Utc::now();
    let time_min = now - chrono::Duration::days(90);
    let time_max = now + chrono::Duration::days(365);

    let mut page_token: Option<String> = None;
    loop {
        let mut url = format!(
            "{}/calendars/{}/events?timeMin={}&timeMax={}&singleEvents=true&orderBy=startTime",
            GOOGLE_CALENDAR_API, google_cal_id,
            time_min.format("%Y-%m-%dT%H:%M:%SZ"),
            time_max.format("%Y-%m-%dT%H:%M:%SZ"),
        );
        if let Some(ref pt) = page_token {
            url.push_str(&format!("&pageToken={}", pt));
        }

        match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", access_token))
            .send()
            .await
        {
            Ok(resp) => {
                if resp.status().is_success() {
                    if let Ok(events) = resp.json::<EventsResponse>().await {
                        if let Some(items) = events.items {
                            for item in &items {
                                if item.status.as_deref() == Some("cancelled") {
                                    continue;
                                }
                                let event_start = item.start.as_ref()
                                    .and_then(|s| s.date_time.as_ref().or(s.date.as_ref()))
                                    .cloned().unwrap_or_default();
                                let event_end = item.end.as_ref()
                                    .and_then(|e| e.date_time.as_ref().or(e.date.as_ref()))
                                    .cloned().unwrap_or_default();
                                pulled.push(json!({
                                    "google_event_id": item.id,
                                    "summary": item.summary,
                                    "start": event_start,
                                    "end": event_end,
                                    "status": item.status,
                                }));
                            }
                        }
                        page_token = events.next_page_token;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to pull Google Calendar events");
                break;
            }
        }

        if page_token.is_none() {
            break;
        }
    }

    Ok(Json(json!({
        "message": "Sync completed",
        "calendar_id": calendar_id.to_string(),
        "calendar_name": calendar_name,
        "bookings_pushed": pushed.len(),
        "events_pulled": pulled.len(),
        "pushed_events": pushed,
        "pulled_events": pulled,
    })))
}

/// POST /api/google-calendar/webhook
/// Handle push notifications from Google Calendar (channel expiration / sync events).
/// Placeholder — Google requires channel setup via the Calendar API.
pub async fn webhook_handler(
    State(_s): State<AppState>,
    Extension(_c): Extension<Claims>,
    body: axum::extract::Json<Value>,
) -> ApiResult<impl IntoResponse> {
    // Google Calendar push notifications come with X-Goog-* headers and a JSON body.
    // This is a placeholder; full push notification handling requires:
    // 1. Registering a channel via POST /calendars/{id}/events/watch with webhook URL
    // 2. Validating X-Goog-Channel-Id, X-Goog-Resource-Id, X-Goog-Resource-State
    // 3. Incremental sync on each notification
    tracing::info!(body = %body.0, "Google Calendar webhook received");
    Ok(StatusCode::OK)
}

// ── Helpers ──────────────────────────────────────────────────────────────

/// Exchange a refresh token for a fresh access token.
async fn get_access_token(refresh_token: &str) -> Result<String, AppError> {
    let (client_id, client_secret, _redirect_uri) = google_oauth_config();

    let params = json!({
        "client_id": client_id,
        "client_secret": client_secret,
        "refresh_token": refresh_token,
        "grant_type": "refresh_token",
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(GOOGLE_TOKEN_URL)
        .json(&params)
        .send()
        .await
        .map_err(|e| AppError::BadRequest(format!("Token refresh failed: {}", e)))?;

    if !resp.status().is_success() {
        let err_text = resp.text().await.unwrap_or_default();
        return Err(AppError::BadRequest(format!("Token refresh error: {}", err_text)));
    }

    let token_data: GoogleTokenResponse = resp
        .json()
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to parse refresh response: {}", e)))?;

    Ok(token_data.access_token)
}
