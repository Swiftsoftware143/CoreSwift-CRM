//! Public webhook handler — OpenClaw, n8n, and CheatLayer all hit this endpoint
//!
//! POST /api/webhook/{token}/{action}
//! No auth header needed — the token identifies the tenant.
//! Body: { "params": {...}, "data": {...} }

use axum::{
    extract::{State, Path, Json, Request},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use super::models::*;
use super::actions;

/// POST /api/webhook/{token}/{action}
pub async fn handle_webhook(
    State(s): State<AppState>,
    Path((token, action)): Path<(String, String)>,
    request: Request,
) -> impl IntoResponse {
    let start = std::time::Instant::now();

    // Look up the webhook by token
    let webhook = match sqlx::query_as::<_, AutomationWebhook>(
        "SELECT * FROM automation_webhooks WHERE webhook_token = $1 AND is_active = true"
    )
    .bind(&token)
    .fetch_optional(&s.db)
    .await
    {
        Ok(Some(w)) => w,
        Ok(None) => {
            return (StatusCode::UNAUTHORIZED, Json(json!({
                "success": false, "error": "Invalid or inactive webhook token"
            }))).into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
                "success": false, "error": format!("DB error: {}", e)
            }))).into_response();
        }
    };

    // Check allowed actions
    if !webhook.allowed_actions.contains(&action) {
        return (StatusCode::FORBIDDEN, Json(json!({
            "success": false,
            "error": format!("Action '{}' not allowed for this webhook", action),
            "allowed": webhook.allowed_actions,
        }))).into_response();
    }

    // Parse body
    let (params, data) = match extract_body(request).await {
        Ok((p, d)) => (p, d),
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({
                "success": false, "error": e
            }))).into_response();
        }
    };

    // Route the action
    match actions::route_action(&s.db, webhook.tenant_id, &action, params.as_ref(), data.as_ref()).await {
        Ok((status, response_data)) => {
            // Update last_used_at
            let _ = sqlx::query("UPDATE automation_webhooks SET last_used_at = NOW() WHERE id = $1")
                .bind(webhook.id).execute(&s.db).await;

            // Log
            let _ = sqlx::query(
                "INSERT INTO automation_webhook_logs (id, webhook_id, action, request_body, response_status, created_at) VALUES ($1, $2, $3, $4, $5, NOW())"
            )
            .bind(Uuid::new_v4())
            .bind(webhook.id)
            .bind(&action)
            .bind(json!({"params": params, "data": data}))
            .bind(status)
            .execute(&s.db).await;

            (StatusCode::from_u16(status as u16).unwrap_or(StatusCode::OK), Json(json!({
                "success": true,
                "action": action,
                "data": response_data,
                "elapsed_ms": start.elapsed().as_millis() as i64,
            }))).into_response()
        }
        Err(e) => {
            // Log failure
            let _ = sqlx::query(
                "INSERT INTO automation_webhook_logs (id, webhook_id, action, request_body, response_status, response_body, created_at) VALUES ($1, $2, $3, $4, $5, $6, NOW())"
            )
            .bind(Uuid::new_v4())
            .bind(webhook.id)
            .bind(&action)
            .bind(json!({"params": params, "data": data}))
            .bind(400)
            .bind(&e)
            .execute(&s.db).await;

            (StatusCode::BAD_REQUEST, Json(json!({
                "success": false,
                "action": action,
                "error": e,
                "elapsed_ms": start.elapsed().as_millis() as i64,
            }))).into_response()
        }
    }
}

/// Extract JSON body from the request
async fn extract_body(request: Request) -> Result<(Option<serde_json::Value>, Option<serde_json::Value>), String> {
    let body_bytes = axum::body::to_bytes(request.into_body(), 1024 * 1024)
        .await
        .map_err(|e| format!("Failed to read body: {}", e))?;

    if body_bytes.is_empty() {
        return Ok((None, None));
    }

    let body: WebhookRequestBody = serde_json::from_slice(&body_bytes)
        .map_err(|e| format!("Invalid JSON: {}", e))?;

    Ok((body.params, body.data))
}
