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
    extract::{Extension, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

// ── Google OAuth2 configuration ──────────────────────────────────────────

const GOOGLE_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_CALENDAR_API: &str = "https://www.googleapis.com/calendar/v3";

/// OAuth2 client configuration for ONE TENANT.
///
/// The tenant's own BYOK slot wins (`provider_keys`, provider `google_calendar`: the client SECRET
/// in `api_key`, the client ID in `metadata.client_id`); the process environment is only a fallback
/// for a centrally-configured deployment. Reading the environment alone made this feature
/// un-configurable by a customer — the "env-var-only is a defect" rule — and the BYOK panel offers
/// this slot because `migrations/070_google_calendar_tenant_slot.sql` added it to
/// `available_providers`.
async fn google_oauth_config(s: &AppState, tid: Uuid) -> (String, String, Option<String>) {
    let nonempty = |v: Option<String>| v.filter(|x| !x.trim().is_empty());

    let row: Option<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT api_key, metadata FROM provider_keys \
         WHERE tenant_id = $1 AND provider = 'google_calendar' AND is_active = true",
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await
    .unwrap_or(None);

    // the stored value is encrypted at rest (CS-21); `open` also passes a legacy plaintext row
    // through untouched, so this works before and after the backfill
    let client_secret = nonempty(row.as_ref().map(|r| crate::secret_box::open(tid, &r.0)))
        .or_else(|| nonempty(std::env::var("GOOGLE_CLIENT_SECRET").ok()));
    let client_id = nonempty(
        row.as_ref()
            .and_then(|r| r.1.get("client_id"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
    )
    .or_else(|| nonempty(std::env::var("GOOGLE_CLIENT_ID").ok()));
    // Google requires the redirect URI presented at token exchange to be byte-identical to the
    // one used at consent time, so the VALUE is carried (signed) inside `state` and reused by the
    // callback. This is only the optional pin: a tenant running its own Google project may set
    // metadata.redirect_uri, and a centrally-configured deployment may set GOOGLE_REDIRECT_URI.
    // Otherwise the request that STARTS the flow decides (`resolve_redirect_uri`) — the only value
    // that can be right for a deployment without anyone configuring it. The old hardcoded
    // `http://localhost:8080/...` fallback sent a real customer's browser to their OWN machine.
    let redirect_pin = nonempty(
        row.as_ref()
            .and_then(|r| r.1.get("redirect_uri"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
    )
    .or_else(|| nonempty(std::env::var("GOOGLE_REDIRECT_URI").ok()));

    (
        client_id.unwrap_or_default(),
        client_secret.unwrap_or_default(),
        redirect_pin,
    )
}

/// The redirect URI Google is told to send the user back to.
///
/// Derived from the request that starts the flow (`X-Forwarded-Proto` + `Host`, which nginx sets
/// for the public vhost) so it matches the deployment with no configuration. A tenant pin or the
/// `GOOGLE_REDIRECT_URI` env var wins, because that tenant's Google project has that exact URI
/// registered.
fn resolve_redirect_uri(explicit: Option<String>, headers: &HeaderMap) -> String {
    if let Some(v) = explicit.filter(|v| !v.trim().is_empty()) {
        return v;
    }
    let host = headers
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    let is_local = host.starts_with("localhost")
        || host.starts_with("127.0.0.1")
        || host.starts_with("0.0.0.0")
        || host.starts_with("[::1]");
    // A LOCAL host is the one case that may legitimately be http (and the only one Google accepts
    // http for). Anywhere else the URI must be https: Google refuses `http://` on a public host, so
    // trusting the origin's `X-Forwarded-Proto` was wrong here — Cloudflare terminates TLS and
    // reaches nginx over plain HTTP, so this deployment advertised
    // `http://app.coreswiftcrm.com/...` (proven live 2026-09-22), a value no tenant could register.
    let scheme = if is_local { "http" } else { "https" };
    format!("{}://{}/api/google-calendar/oauth-callback", scheme, host)
}

// ── OAuth `state`: signed, single-purpose, never a bearer token ──────────────────

/// HMAC key for the OAuth `state`, derived from the JWT secret and domain-separated so a `state`
/// can never be replayed as an access token.
fn state_key(secret: &str) -> String {
    format!("{}:google-calendar-oauth-state", secret)
}

/// Sign the consent `state` as `v1.<b64url(json)>.<hex hmac>`.
///
/// The `state` param travels to Google, into the browser history and into Google's logs, so it
/// must never be worth stealing: it carries only what the callback needs (tenant, calendar, the
/// redirect URI used at consent time) plus an issue time. The callback is a browser redirect with
/// NO Authorization header — that is all Google sends — so this signature is the only thing that
/// can prove who started the flow.
fn sign_state(secret: &str, tid: Uuid, calendar_id: Option<Uuid>, redirect_uri: &str) -> String {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let payload = json!({
        "aid": tid,
        "cal": calendar_id,
        "ru": redirect_uri,
        "iat": chrono::Utc::now().timestamp(),
        "n": Uuid::new_v4(),
    })
    .to_string();
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.as_bytes());
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(state_key(secret).as_bytes())
        .expect("HMAC accepts keys of any length");
    mac.update(body.as_bytes());
    format!("v1.{}.{}", body, hex::encode(mac.finalize().into_bytes()))
}

/// Verify a consent `state`, returning `(tenant, calendar, redirect_uri)`.
/// Constant-time signature check; rejects anything older than 30 minutes.
fn verify_state(secret: &str, state: &str) -> Option<(Uuid, Option<Uuid>, String)> {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let rest = state.strip_prefix("v1.")?;
    let (body, sig_hex) = rest.rsplit_once('.')?;
    let sig = hex::decode(sig_hex).ok()?;
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(state_key(secret).as_bytes()).ok()?;
    mac.update(body.as_bytes());
    mac.verify_slice(&sig).ok()?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .ok()?;
    let v: Value = serde_json::from_slice(&raw).ok()?;
    let iat = v.get("iat").and_then(|i| i.as_i64()).unwrap_or(0);
    if chrono::Utc::now().timestamp() - iat > 1800 {
        return None;
    }
    let tid = Uuid::parse_str(v.get("aid")?.as_str()?).ok()?;
    let cal = v
        .get("cal")
        .and_then(|c| c.as_str())
        .and_then(|c| Uuid::parse_str(c).ok());
    let ru = v
        .get("ru")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    Some((tid, cal, ru))
}

/// Percent-encode one query-string value (RFC 3986 unreserved set only).
/// The consent URL used to interpolate `redirect_uri` and `scope` raw.
fn pct(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

// ── Route definitions ────────────────────────────────────────────────────

pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, post};

    // Machine surfaces — GOOGLE calls these, and a browser redirect / push notification carries NO
    // Authorization header. Behind `auth_middleware` they answered 401 to the exact request Google
    // makes, so the consent handshake could never complete (proven live 2026-09-22:
    // GET /api/google-calendar/oauth-callback -> 401 "Authentication required"). The callback is
    // authenticated by its SIGNED `state` instead (see `verify_state`); the plan gate stays on the
    // route that STARTS the flow, which is the only one a user drives.
    let public = axum::Router::new()
        .route("/oauth-callback", get(oauth_callback))
        .route("/webhook", post(webhook_handler));

    let protected = axum::Router::new()
        .route("/connect-url", get(get_connect_url))
        .route("/status", get(calendar_status))
        .route("/sync/:calendar_id", post(sync_calendar))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "google_calendar",
                "Google Calendar sync",
            ),
            crate::features::gate_mw,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    axum::Router::new().merge(public).merge(protected)
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
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (client_id, _client_secret, redirect_pin) = google_oauth_config(&s, tid).await;
    let redirect_uri = resolve_redirect_uri(redirect_pin, &headers);

    if client_id.is_empty() {
        return Err(AppError::Validation(
            "Google Calendar is not configured for this account: add your Google client ID and \
             client secret under Provider Keys (they are stored per tenant), or set \
             GOOGLE_CLIENT_ID / GOOGLE_CLIENT_SECRET on the server."
                .to_string(),
        ));
    }

    // The state holds the calendar_id so we can link it after OAuth completes
    let calendar_id = q.get("calendar_id").cloned().unwrap_or_default();

    // Verify the calendar exists and belongs to this tenant
    if !calendar_id.is_empty() {
        // `query_scalar` decodes exactly ONE column, so the annotation must be the COLUMN type
        // (`Option<Uuid>`). Annotated `Option<(Uuid,)>` sqlx tried to decode the UUID column as a
        // SQL RECORD and reported "mismatched types" on EVERY call — so this route answered 500
        // for any request that named a calendar, which is the only way the Calendar tab can use it
        // (proven live 2026-09-22: connect-url?calendar_id=<real id> -> 500 "Database error").
        let cal: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM booking_calendars WHERE id = $1 AND tenant_id = $2")
                .bind(
                    Uuid::parse_str(&calendar_id)
                        .map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?,
                )
                .bind(tid)
                .fetch_optional(&s.db)
                .await?;
        if cal.is_none() {
            return Err(AppError::NotFound("Calendar not found".to_string()));
        }
    }

    let scopes =
        "https://www.googleapis.com/auth/calendar https://www.googleapis.com/auth/calendar.events";
    let cal = if calendar_id.is_empty() {
        None
    } else {
        Some(
            Uuid::parse_str(&calendar_id)
                .map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?,
        )
    };
    let state = sign_state(&s.config.jwt_secret, tid, cal, &redirect_uri);

    let auth_url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&state={}",
        GOOGLE_AUTH_URL,
        pct(&client_id),
        pct(&redirect_uri),
        pct(scopes),
        pct(&state)
    );

    Ok(Json(json!({
        "connect_url": auth_url,
        "calendar_id": calendar_id,
        // The exact URI that must be registered on the tenant's own Google OAuth client —
        // Google rejects the token exchange if the two differ by one byte. The Calendar tab
        // shows it so a tenant setting up their own client can copy it.
        "redirect_uri": redirect_uri,
    })))
}

/// GET /api/google-calendar/oauth-callback
/// Handles the OAuth2 callback from Google, stores refresh token in booking_calendars.
pub async fn oauth_callback(
    State(s): State<AppState>,
    Query(params): Query<OAuthCallbackParams>,
) -> ApiResult<impl IntoResponse> {
    if let Some(err) = &params.error {
        return Err(AppError::BadRequest(format!("Google OAuth error: {}", err)));
    }

    let code = params
        .code
        .ok_or_else(|| AppError::Validation("Authorization code missing".to_string()))?;

    // No Authorization header exists on a browser redirect, so the SIGNED `state` is the
    // credential: it names the tenant, the calendar and the redirect URI used at consent time.
    let raw_state = params.state.as_deref().ok_or_else(|| {
        AppError::Validation(
            "OAuth state missing — restart the connect flow from the Calendar tab".to_string(),
        )
    })?;
    let (tid, state_calendar, redirect_uri) = verify_state(&s.config.jwt_secret, raw_state)
        .ok_or_else(|| {
            AppError::BadRequest(
                "Invalid or expired OAuth state — restart the connect flow from the Calendar tab"
                    .to_string(),
            )
        })?;
    let (client_id, client_secret, _redirect_pin) = google_oauth_config(&s, tid).await;

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

    // Surface GOOGLE's own sentence (`invalid_client`, `redirect_uri_mismatch`, `invalid_grant`).
    // The first failure in a BYOK OAuth flow is almost always a credential or redirect-URI
    // mismatch, and "Failed to parse token response" hid exactly the text that fixes it.
    let token_status = token_resp.status();
    let token_body = token_resp.text().await.unwrap_or_default();
    if !token_status.is_success() {
        return Err(AppError::BadRequest(format!(
            "Google refused the token exchange (HTTP {}): {}",
            token_status,
            token_body.chars().take(400).collect::<String>()
        )));
    }
    let token_data: GoogleTokenResponse = serde_json::from_str(&token_body)
        .map_err(|e| AppError::BadRequest(format!("Failed to parse token response: {}", e)))?;

    let refresh_token = token_data.refresh_token.ok_or_else(|| {
        AppError::BadRequest(
            "No refresh_token received (Google requires prompt=consent)".to_string(),
        )
    })?;

    // The consent URL may or may not name a booking calendar. With no calendar there is nowhere
    // tenant-scoped to keep the refresh token, so say that plainly instead of implying a link
    // that does not exist (the old branch answered "Refresh token stored" and stored nothing).
    let calendar_id = match state_calendar {
        Some(c) => c.to_string(),
        None => {
            return Ok(Json(json!({
                "message": "Google authorised this account, but no booking calendar was named in the connect flow, so nothing was linked. Start again from the Calendar tab's Connect Google Calendar button.",
                "has_refresh_token": false,
                "linked": false,
            })));
        }
    };

    // Store the refresh token on the booking_calendars record
    // Also create a Google Calendar if this calendar doesn't have one yet
    sqlx::query(
        "UPDATE booking_calendars SET google_refresh_token = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
    )
    .bind(&refresh_token)
    .bind(Uuid::parse_str(&calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
    .bind(tid)
    .execute(&s.db)
    .await?;

    // If the calendar doesn't have a google_calendar_id yet, fetch/create one
    let existing_cal_id: Option<String> = sqlx::query_scalar(
        "SELECT google_calendar_id FROM booking_calendars WHERE id = $1 AND tenant_id = $2",
    )
    .bind(
        Uuid::parse_str(&calendar_id)
            .map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?,
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .flatten();

    if existing_cal_id.is_none() {
        // Create a new Google Calendar for this booking calendar
        let calendar_name: String = sqlx::query_scalar(
            "SELECT name FROM booking_calendars WHERE id = $1 AND tenant_id = $2",
        )
        .bind(
            Uuid::parse_str(&calendar_id)
                .map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?,
        )
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
            .header(
                "Authorization",
                format!("Bearer {}", token_data.access_token),
            )
            .json(&create_payload)
            .send()
            .await
            .map_err(|e| {
                AppError::BadRequest(format!("Failed to create Google Calendar: {}", e))
            })?;

        if create_resp.status().is_success() {
            let created: CalendarItem = create_resp.json().await.map_err(|e| {
                AppError::BadRequest(format!("Failed to parse calendar create response: {}", e))
            })?;

            sqlx::query(
                "UPDATE booking_calendars SET google_calendar_id = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
            )
            .bind(&created.id)
            .bind(Uuid::parse_str(&calendar_id).map_err(|_| AppError::Validation("Invalid calendar_id".to_string()))?)
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
           FROM booking_calendars WHERE id = $1 AND tenant_id = $2"#,
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

    // Get a fresh access token using the refresh token, with THIS tenant's OAuth client
    let access_token = get_access_token(&s, tid, &refresh_token).await?;

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

    for (
        booking_id,
        business_name,
        contact_name,
        contact_email,
        description,
        start_date,
        _slot_pos,
        _status,
        end_date,
    ) in &bookings
    {
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
            .post(format!(
                "{}/calendars/{}/events",
                GOOGLE_CALENDAR_API, google_cal_id
            ))
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
            GOOGLE_CALENDAR_API,
            google_cal_id,
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
                                let event_start = item
                                    .start
                                    .as_ref()
                                    .and_then(|s| s.date_time.as_ref().or(s.date.as_ref()))
                                    .cloned()
                                    .unwrap_or_default();
                                let event_end = item
                                    .end
                                    .as_ref()
                                    .and_then(|e| e.date_time.as_ref().or(e.date.as_ref()))
                                    .cloned()
                                    .unwrap_or_default();
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

/// GET /api/google-calendar/status
///
/// Which of THIS tenant's booking calendars are mirrored into Google Calendar — the reader the
/// Calendar tab needs to be honest, and the reason it exists rather than widening
/// `GET /api/bookings/calendars`: this shape can carry the Google link state WITHOUT ever
/// returning `google_refresh_token`, which is a standing grant on the tenant's Google account and
/// which no surface needs to read back.
pub async fn calendar_status(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query_as::<_, (Uuid, String, String, Option<String>)>(
        "SELECT id, name, slug, google_calendar_id FROM booking_calendars \
         WHERE tenant_id = $1 ORDER BY name",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let (client_id, _, _) = google_oauth_config(&s, tid).await;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|(id, name, slug, gcal)| {
            json!({
                "id": id,
                "name": name,
                "slug": slug,
                "connected": gcal.is_some(),
                "google_calendar_id": gcal,
            })
        })
        .collect();

    Ok(Json(json!({
        "configured": !client_id.is_empty(),
        "count": items.len(),
        "items": items,
    })))
}

// ── Helpers ──────────────────────────────────────────────────────────────
async fn get_access_token(
    s: &AppState,
    tid: Uuid,
    refresh_token: &str,
) -> Result<String, AppError> {
    let (client_id, client_secret, _redirect_uri) = google_oauth_config(s, tid).await;

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
        return Err(AppError::BadRequest(format!(
            "Token refresh error: {}",
            err_text
        )));
    }

    let token_data: GoogleTokenResponse = resp
        .json()
        .await
        .map_err(|e| AppError::BadRequest(format!("Failed to parse refresh response: {}", e)))?;

    Ok(token_data.access_token)
}
