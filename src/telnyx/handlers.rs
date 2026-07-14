//! Telnyx integration handlers.
//!
//! Ported from MissedCall Respondr with adaptations for CoreSwift's:
//! - Claims model (sub=user_id, aid=tenant_id)
//! - sqlx 0.8 (uses PgPool directly)
//! - AppState (uses `state.db` instead of `state.pool`)
//! - Existing communications/providers.rs for outbound SMS
//! - Existing billing system for credit management

use axum::{
    extract::{Extension, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult};
use crate::AppState;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// TelnyxConfig — global, single-row config for the platform-wide Telnyx account.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TelnyxConfig {
    pub id: Uuid,
    pub api_key: String,
    pub profile_id: Option<String>,
    pub messaging_profile_id: Option<String>,
    pub webhook_secret: Option<String>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// TelnyxNumber — a phone number purchased via Telnyx, scoped to a tenant.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TelnyxNumber {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub phone_number: String,
    pub friendly_name: Option<String>,
    pub provider: String,
    pub capabilities: Value,
    pub is_active: bool,
    pub telnyx_connection_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// OutboundSms — record of an outbound SMS sent via Telnyx.
#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct OutboundSms {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub from_number: String,
    pub to_number: String,
    pub body: String,
    pub status: String,
    pub telnyx_message_id: Option<String>,
    pub error_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// Request / response types

#[derive(Debug, Deserialize)]
pub struct SendSmsRequest {
    pub from: String,
    pub to: String,
    pub body: String,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxConfigUpdate {
    pub api_key: String,
    pub profile_id: Option<String>,
    pub messaging_profile_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PurchaseNumberRequest {
    pub number: String,
    #[serde(default)]
    pub friendly_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AvailableNumbersQuery {
    pub filter: Option<String>,
    pub limit: Option<i64>,
}

// Telnyx webhook types

#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookPayload {
    pub data: Option<TelnyxWebhookData>,
    pub meta: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookData {
    pub event_type: Option<String>,
    pub id: Option<String>,
    pub occurred_at: Option<String>,
    pub payload: Option<TelnyxWebhookEventPayload>,
}

#[derive(Debug, Deserialize)]
pub struct TelnyxWebhookEventPayload {
    pub call_control_id: Option<String>,
    pub connection_id: Option<String>,
    pub call_leg_id: Option<String>,
    pub call_session_id: Option<String>,
    pub client_state: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub direction: Option<String>,
    pub state: Option<String>,
    pub start_time: Option<String>,
    pub sip_source_ip: Option<String>,
    #[serde(default)]
    pub digits: Option<String>,
    #[serde(default)]
    pub text: Option<String>,            // SMS body
    #[serde(default)]
    pub messaging_profile_id: Option<String>,
    #[serde(default)]
    pub message_type: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Mask an API key for logging / display (keep first 3 + last 3 chars).
fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return format!("{}...", &key[..3.min(key.len())]);
    }
    let prefix = &key[..3];
    let suffix = &key[key.len()-3..];
    format!("{}...{}", prefix, suffix)
}

/// Fetch the global Telnyx config row.
async fn get_global_config(db: &sqlx::PgPool) -> Result<Option<TelnyxConfig>, AppError> {
    let config = sqlx::query_as::<_, TelnyxConfig>(
        "SELECT * FROM telnyx_config WHERE is_active = true LIMIT 1"
    )
    .fetch_optional(db)
    .await?;
    Ok(config)
}

/// Check if tenant has their own Telnyx key (BYOK) via provider_keys table.
async fn tenant_has_own_telnyx(db: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let count = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM provider_keys WHERE tenant_id = $1 AND provider = 'telnyx' AND is_active = true"
    )
    .bind(tenant_id)
    .fetch_one(db)
    .await?;
    Ok(count > 0)
}

/// Get the tenant's Telnyx API key: prefer BYOK, fall back to global config.
async fn resolve_telnyx_api_key(db: &sqlx::PgPool, tenant_id: Uuid) -> Result<String, AppError> {
    // Try BYOK first
    let byok_key: Option<String> = sqlx::query_scalar(
        "SELECT api_key FROM provider_keys WHERE tenant_id = $1 AND provider = 'telnyx' AND is_active = true"
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?;

    if let Some(key) = byok_key {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    // Fall back to global config
    let global = get_global_config(db).await?
        .ok_or_else(|| AppError::Internal("Telnyx not configured".into()))?;

    if global.api_key.is_empty() {
        return Err(AppError::Internal("Telnyx API key is empty in global config".into()));
    }

    Ok(global.api_key)
}

/// Call the Telnyx REST API
async fn telnyx_api_request(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<(reqwest::StatusCode, Value), AppError> {
    let api_key = resolve_telnyx_api_key(db, tenant_id).await?;
    let url = format!("https://api.telnyx.com/v2{}", path);
    let client = reqwest::Client::new();

    let mut req = client
        .request(method.clone(), &url)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json");

    if let Some(b) = body {
        req = req.json(&b);
    }

    let resp = req.send().await
        .map_err(|e| AppError::Internal(format!("Telnyx API error: {}", e)))?;

    let status = resp.status();
    let resp_body: Value = resp.json().await
        .map_err(|e| AppError::Internal(format!("Failed to parse Telnyx response: {}", e)))?;

    Ok((status, resp_body))
}

/// Deduct one credit from the tenant's plan balance
async fn deduct_credit(db: &sqlx::PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let result = sqlx::query_scalar::<_, Option<i32>>(
        "UPDATE tenant_plans
         SET credit_balance = GREATEST(COALESCE(credit_balance, 0) - 1, 0),
             lifetime_credits = COALESCE(lifetime_credits, 0) + 1,
             updated_at = NOW()
         WHERE tenant_id = $1
         RETURNING credit_balance"
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?;

    match result {
        Some(Some(balance)) => Ok(balance > 0),
        _ => Ok(false), // no plan row — no credits
    }
}

/// Resolve tenant_id from a called phone number (Telnyx inbound calls/SMS)
async fn tenant_id_for_number(db: &sqlx::PgPool, called_number: &str) -> Result<Uuid, AppError> {
    let normalized = if called_number.starts_with('+') {
        called_number.to_string()
    } else {
        format!("+{}", called_number)
    };

    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT tenant_id FROM telnyx_numbers WHERE phone_number = $1 AND is_active = true LIMIT 1"
    )
    .bind(&normalized)
    .fetch_optional(db)
    .await?
    .flatten();

    tenant_id.ok_or_else(|| AppError::NotFound(format!("No tenant found for number: {}", normalized)))
}

// ---------------------------------------------------------------------------
// 1. POST /api/telnyx/send-sms — Send an outbound SMS
// ---------------------------------------------------------------------------
pub async fn send_sms(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<SendSmsRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Validate
    if req.to.is_empty() || req.body.is_empty() {
        return Err(AppError::Validation("to and body are required".into()));
    }

    // Normalize from number
    let from = if req.from.starts_with('+') {
        req.from.clone()
    } else {
        format!("+{}", req.from)
    };

    // Verify tenant owns the from number
    let owns_number = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT id FROM telnyx_numbers WHERE tenant_id = $1 AND phone_number = $2 AND is_active = true"
    )
    .bind(tenant_id)
    .bind(&from)
    .fetch_optional(&state.db)
    .await?;

    if owns_number.is_none() {
        return Err(AppError::Validation(format!(
            "Number {} is not assigned to your account", from
        )));
    }

    // Resolve API key and send
    let api_key = resolve_telnyx_api_key(&state.db, tenant_id).await?;
    let messaging_profile_id = if tenant_has_own_telnyx(&state.db, tenant_id).await? {
        None // BYOK — Telnyx manages via API key
    } else {
        // Use global config's messaging profile
        get_global_config(&state.db).await?
            .and_then(|c| c.messaging_profile_id)
    };

    let mut payload = json!({
        "from": from,
        "to": req.to,
        "text": req.body,
    });

    if let Some(profile_id) = messaging_profile_id {
        payload["messaging_profile_id"] = json!(profile_id);
    }

    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.telnyx.com/v2/messages")
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Telnyx send error: {}", e)))?;

    let status = resp.status();
    let resp_body: Value = resp.json().await
        .map_err(|e| AppError::Internal(format!("Failed to parse Telnyx response: {}", e)))?;

    if !status.is_success() && status != 202 {
        let err_msg = resp_body
            .pointer("/errors/0/detail")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown Telnyx error");
        return Err(AppError::Internal(format!("Telnyx SMS failed: {}", err_msg)));
    }

    // Extract message ID from Telnyx response
    let telnyx_msg_id = resp_body
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Record the outbound SMS
    let msg_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status, sent_at)
         VALUES ($1, $2, 'sms', $3, NULL, $4, 'sent', NOW())"
    )
    .bind(msg_id)
    .bind(tenant_id)
    .bind(&req.to)
    .bind(&req.body)
    .execute(&state.db)
    .await?;

    // Also record in outbound_sms table for Telnyx-specific tracking
    let sms_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outbound_sms (id, tenant_id, from_number, to_number, body, status, telnyx_message_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"
    )
    .bind(sms_id)
    .bind(tenant_id)
    .bind(&from)
    .bind(&req.to)
    .bind(&req.body)
    .bind("sent")
    .bind(&telnyx_msg_id)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::OK, Json(json!({
        "message_id": sms_id,
        "telnyx_message_id": telnyx_msg_id,
        "status": "sent",
        "from": from,
        "to": req.to,
    }))))
}

// ---------------------------------------------------------------------------
// 2. POST /api/telnyx/webhook — Inbound call webhook receiver (public)
// ---------------------------------------------------------------------------
pub async fn webhook(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let event_type = body
        .pointer("/data/event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let call_control_id = body
        .pointer("/data/payload/call_control_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let from_number = body
        .pointer("/data/payload/from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let to_number = body
        .pointer("/data/payload/to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    tracing::info!(
        "Telnyx voice webhook: event={}, from={:?}, to={:?}, call_ctrl={:?}",
        event_type, from_number, to_number, call_control_id
    );

    // Only process inbound calls
    if event_type != "call_received" && event_type != "call_initiated" {
        return Ok(Json(json!({ "commands": [] })));
    }

    let called = to_number.clone().unwrap_or_default();
    let normalized_called = if called.starts_with('+') {
        called.clone()
    } else {
        format!("+{}", called)
    };

    let tenant_id = match tenant_id_for_number(&state.db, &normalized_called).await {
        Ok(id) => id,
        Err(_) => {
            tracing::warn!("Telnyx webhook: no tenant for number {}", normalized_called);
            return Ok(Json(json!({"commands": [{"type": "hangup"}]})));
        }
    };

    // Check BYOK — if using own key, skip credit deduction
    let byok = tenant_has_own_telnyx(&state.db, tenant_id).await?;
    if !byok {
        let has_credits = deduct_credit(&state.db, tenant_id).await?;
        if !has_credits {
            tracing::warn!("Tenant {} insufficient credits for inbound call", tenant_id);
            return Ok(Json(json!({"commands": [{"type": "hangup"}]})));
        }
    }

    let caller = from_number.unwrap_or_else(|| "unknown".to_string());
    let normalized_caller = if caller.starts_with('+') {
        caller
    } else {
        format!("+{}", caller)
    };

    // Record inbound call
    let call_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    sqlx::query(
        "INSERT INTO inbound_calls (id, tenant_id, caller_number, called_number, call_time, disposition)
         VALUES ($1, $2, $3, $4, $5, 'missed')"
    )
    .bind(call_id)
    .bind(tenant_id)
    .bind(&normalized_caller)
    .bind(&normalized_called)
    .bind(now)
    .execute(&state.db)
    .await?;

    // Record call log
    sqlx::query(
        "INSERT INTO call_logs (id, tenant_id, caller_number, called_number, duration, disposition, cost, recorded)
         VALUES ($1, $2, $3, $4, NULL, 'missed', $5, false)"
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&normalized_caller)
    .bind(&normalized_called)
    .bind(if byok { None } else { Some(1.0_f64) })
    .execute(&state.db)
    .await?;

    tracing::info!("Processed Telnyx inbound call for tenant {}: call_id={}", tenant_id, call_id);

    // Return answer + gather commands (same as MissedCall Respondr)
    Ok(Json(json!({
        "commands": [
            {"type": "answer"},
            {
                "type": "record_start",
                "options": {"format": "wav", "play_beep": false}
            },
            {
                "type": "gather_using_audio",
                "options": {
                    "invalid_audio_url": "default",
                    "inter_digit_timeout_ms": 2000,
                    "max_digits": 1,
                    "timeout_millis": 10000
                }
            }
        ]
    })))
}

// ---------------------------------------------------------------------------
// 3. POST /api/telnyx/sms-webhook — Inbound SMS webhook receiver (public)
// ---------------------------------------------------------------------------
pub async fn sms_webhook(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> ApiResult<impl IntoResponse> {
    let event_type = body
        .pointer("/data/event_type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    tracing::info!("Telnyx SMS webhook received: event_type={}", event_type);

    // Only process inbound SMS messages
    if event_type != "message.received" {
        return Ok(Json(json!({"status": "ack"})));
    }

    let from_number = body
        .pointer("/data/payload/from")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let to_number = body
        .pointer("/data/payload/to")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let text = body
        .pointer("/data/payload/text")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();

    let telnyx_msg_id = body
        .pointer("/data/id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    tracing::info!(
        "Inbound SMS: from={:?}, to={:?}, body={}",
        from_number, to_number, text
    );

    let called = to_number.clone().unwrap_or_default();
    let normalized_called = if called.starts_with('+') {
        called
    } else {
        format!("+{}", called)
    };

    // Resolve tenant from the "to" number (the number the SMS was sent to)
    match tenant_id_for_number(&state.db, &normalized_called).await {
        Ok(tenant_id) => {
            let normalized_from = from_number.as_ref().map(|s| {
                if s.starts_with('+') { s.clone() } else { format!("+{}", s) }
            }).unwrap_or_else(|| "unknown".to_string());

            // Record inbound SMS as an event
            sqlx::query(
                "INSERT INTO events (id, tenant_id, source, event_type, entity_type, payload)
                 VALUES ($1, $2, 'telnyx', 'sms.received', 'message', $3)"
            )
            .bind(Uuid::new_v4())
            .bind(tenant_id)
            .bind(json!({
                "from": normalized_from,
                "to": normalized_called,
                "text": text,
                "telnyx_message_id": telnyx_msg_id,
                "direction": "inbound",
            }))
            .execute(&state.db)
            .await?;

            tracing::info!("Inbound SMS recorded for tenant {}: from={}", tenant_id, normalized_from);
        }
        Err(e) => {
            tracing::warn!("No tenant found for SMS destination {}: {}", normalized_called, e);
        }
    }

    Ok(Json(json!({"status": "received"})))
}

// ---------------------------------------------------------------------------
// 4. GET /api/telnyx/numbers — List purchased numbers for current tenant
// ---------------------------------------------------------------------------
pub async fn list_numbers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let numbers = sqlx::query_as::<_, TelnyxNumber>(
        "SELECT id, tenant_id, phone_number, friendly_name, provider, capabilities, is_active, telnyx_connection_id, created_at, updated_at
         FROM telnyx_numbers
         WHERE tenant_id = $1
         ORDER BY phone_number ASC"
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "count": numbers.len(), "items": numbers })))
}

// ---------------------------------------------------------------------------
// 5. POST /api/telnyx/numbers — Purchase/assign a number
// ---------------------------------------------------------------------------
pub async fn purchase_number(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<PurchaseNumberRequest>,
) -> Result<(StatusCode, Json<Value>), AppError> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let number = if req.number.starts_with('+') {
        req.number.clone()
    } else {
        format!("+{}", req.number)
    };

    // Check if already assigned and active
    let existing_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM telnyx_numbers WHERE phone_number = $1 AND is_active = true"
    )
    .bind(&number)
    .fetch_optional(&state.db)
    .await?
    .flatten();

    if let Some(existing_id) = existing_id {
        let current_tenant: Option<Uuid> = sqlx::query_scalar(
            "SELECT tenant_id FROM telnyx_numbers WHERE id = $1"
        )
        .bind(existing_id)
        .fetch_one(&state.db)
        .await?;

        if current_tenant == Some(tenant_id) {
            return Err(AppError::Duplicate("Number already assigned to your account".into()));
        }

        // Reassign
        sqlx::query(
            "UPDATE telnyx_numbers SET tenant_id = $1, updated_at = NOW() WHERE id = $2"
        )
        .bind(tenant_id)
        .bind(existing_id)
        .execute(&state.db)
        .await?;

        return Ok((StatusCode::OK, Json(json!({
            "id": existing_id,
            "number": number,
            "assigned": true,
            "reassigned": true
        }))));
    }

    // If not BYOK, purchase via Telnyx API
    let byok = tenant_has_own_telnyx(&state.db, tenant_id).await?;
    if !byok {
        let global = get_global_config(&state.db).await?
            .ok_or_else(|| AppError::Internal("Telnyx not configured by admin".into()))?;

        let client = reqwest::Client::new();
        let mut purchase_payload = json!({
            "phone_number": number,
        });
        if let Some(ref conn_id) = global.profile_id {
            purchase_payload["connection_id"] = json!(conn_id);
        }

        let resp = client
            .post("https://api.telnyx.com/v2/phone_numbers")
            .header("Authorization", format!("Bearer {}", global.api_key))
            .json(&purchase_payload)
            .send()
            .await
            .map_err(|e| AppError::Internal(format!("Telnyx purchase error: {}", e)))?;

        let resp_status = resp.status();
        let resp_body: Value = resp.json().await
            .map_err(|e| AppError::Internal(format!("Failed to parse Telnyx purchase response: {}", e)))?;

        if !resp_status.is_success() {
            return Err(AppError::Internal(format!(
                "Telnyx purchase failed ({}): {}",
                resp_status,
                resp_body
            )));
        }
    }

    // Insert locally
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO telnyx_numbers (id, tenant_id, phone_number, friendly_name, provider, is_active)
         VALUES ($1, $2, $3, $4, 'telnyx', true)"
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&number)
    .bind(&req.friendly_name)
    .execute(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!({
        "id": id,
        "number": number,
        "assigned": true,
        "reassigned": false
    }))))
}

// ---------------------------------------------------------------------------
// 6. DELETE /api/telnyx/numbers/:id — Release/unassign a number
// ---------------------------------------------------------------------------
pub async fn delete_number(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let number = sqlx::query_as::<_, TelnyxNumber>(
        "SELECT * FROM telnyx_numbers WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Number not found or not owned by account".into()))?;

    // If not BYOK, release via Telnyx API
    let byok = tenant_has_own_telnyx(&state.db, tenant_id).await?;
    if !byok {
        if let Ok(Some(_)) = get_global_config(&state.db).await {
            // Don't block on Telnyx release failure — soft-delete locally regardless
            let _ = telnyx_api_request(&state.db, tenant_id, reqwest::Method::DELETE,
                &format!("/phone_numbers/{}", number.phone_number.trim_start_matches('+')), None).await;
        }
    }

    // Soft-delete
    sqlx::query(
        "UPDATE telnyx_numbers SET is_active = false, updated_at = NOW() WHERE id = $1"
    )
    .bind(id)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "deleted": true,
        "id": id,
        "number": number.phone_number
    })))
}

// ---------------------------------------------------------------------------
// 7. GET /api/telnyx/available — Search available numbers from Telnyx
// ---------------------------------------------------------------------------
pub async fn search_available_numbers(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<AvailableNumbersQuery>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let filter = query.filter.unwrap_or_else(|| "555".to_string());
    let limit = query.limit.unwrap_or(10).min(50);

    let (status, body) = telnyx_api_request(
        &state.db,
        tenant_id,
        reqwest::Method::GET,
        &format!("/available_phone_numbers?filter[number][starts_with]={}&page[size]={}", filter, limit),
        None,
    ).await?;

    if !status.is_success() {
        tracing::warn!("Telnyx available numbers search failed: {} {:?}", status, body);
    }

    let numbers = body
        .pointer("/data")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().map(|item| {
                json!({
                    "phone_number": item.pointer("/attributes/phone_number").and_then(|v| v.as_str()),
                    "region": item.pointer("/attributes/region_information").and_then(|r| r.as_array()).and_then(|ri| ri.first()).and_then(|r| r.pointer("/region_name")).and_then(|v| v.as_str()),
                    "rate_center": item.pointer("/attributes/rate_center").and_then(|v| v.as_str()),
                    "cost": item.pointer("/attributes/cost_information/monthly_cost").and_then(|v| v.as_str()),
                    "features": item.pointer("/attributes/features").and_then(|v| v.as_array()),
                })
            }).collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(Json(json!({
        "count": numbers.len(),
        "items": numbers,
    })))
}

// ---------------------------------------------------------------------------
// 8. GET /api/telnyx/config — Get Telnyx API config
// ---------------------------------------------------------------------------
pub async fn get_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Check BYOK first
    let byok_key: Option<String> = sqlx::query_scalar(
        "SELECT api_key FROM provider_keys WHERE tenant_id = $1 AND provider = 'telnyx' AND is_active = true"
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?;

    if let Some(ref key) = byok_key {
        return Ok(Json(json!({
            "mode": "byok",
            "api_key_masked": mask_key(key),
            "api_key_set": !key.is_empty(),
        })));
    }

    // Global config
    let global = get_global_config(&state.db).await?;
    match global {
        Some(c) => Ok(Json(json!({
            "mode": "global",
            "api_key_set": !c.api_key.is_empty(),
            "api_key_masked": if c.api_key.is_empty() { None } else { Some(mask_key(&c.api_key)) },
            "messaging_profile_id": c.messaging_profile_id,
            "profile_id": c.profile_id,
        }))),
        None => Ok(Json(json!({
            "mode": "none",
            "api_key_set": false,
        }))),
    }
}

// ---------------------------------------------------------------------------
// 9. PUT /api/telnyx/config — Update Telnyx API config
// ---------------------------------------------------------------------------
pub async fn update_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<TelnyxConfigUpdate>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Upsert into provider_keys for this tenant (BYOK mode)
    let api_key_clone = req.api_key.clone();
    sqlx::query(
        "INSERT INTO provider_keys (tenant_id, provider, api_key, metadata, is_active, scope)
         VALUES ($1, 'telnyx', $2, $3, true, 'tenant')
         ON CONFLICT (tenant_id, provider)
         DO UPDATE SET api_key = EXCLUDED.api_key,
                       metadata = CASE WHEN EXCLUDED.metadata = '{}'::jsonb THEN provider_keys.metadata ELSE EXCLUDED.metadata END,
                       is_active = true,
                       updated_at = NOW()"
    )
    .bind(tenant_id)
    .bind(&req.api_key)
    .bind(json!({
        "profile_id": req.profile_id,
        "messaging_profile_id": req.messaging_profile_id,
    }))
    .execute(&state.db)
    .await?;

    tracing::info!("Telnyx config updated for tenant {} (BYOK)", tenant_id);

    Ok(Json(json!({
        "message": "Telnyx configuration saved",
        "mode": "byok",
        "api_key_masked": mask_key(&api_key_clone),
    })))
}
