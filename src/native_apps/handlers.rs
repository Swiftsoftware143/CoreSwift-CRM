//! Native App Connectors — API handlers

use axum::{extract::{State, Path, Json, Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use sqlx::Row;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use super::models::*;
use super::connectors;

// ── List all available apps (visible to all authenticated users) ──

pub async fn list_available_apps() -> ApiResult<impl IntoResponse> {
    let apps: Vec<serde_json::Value> = connectors::NATIVE_APPS.iter().map(|a| {
        json!({
            "slug": a.slug,
            "name": a.name,
            "description": a.description,
            "auth_type": a.auth_type,
            "auth_fields": a.auth_fields,
            "access_level": a.access_level,
            "meta": connectors::get_app_meta(a.slug),
        })
    }).collect();
    Ok(Json(json!({"apps": apps})))
}

// ── Connect an app (admin or tenant, depending on access level) ──

pub async fn connect_app(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(r): Json<ConnectAppRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let is_admin = c.role == "account_owner" || c.role == "admin";

    // Look up the app definition
    let app = connectors::NATIVE_APPS.iter().find(|a| a.slug == app_slug)
        .ok_or_else(|| AppError::NotFound(format!("App '{}' not found", app_slug)))?;

    // Check access level
    if app.access_level == "admin" && !is_admin {
        return Err(AppError::Forbidden);
    }

    // Validate required fields
    for field in app.auth_fields {
        let value = r.credentials.get(*field).and_then(|v| v.as_str());
        match value {
            Some(v) if !v.is_empty() => {},
            _ => return Err(AppError::Validation(format!("{} is required", field))),
        }
    }

    // Test the connection
    let (test_ok, test_msg, _latency) = connectors::test_connection(&app_slug, &r.credentials).await;
    if !test_ok {
        return Err(AppError::Validation(format!("Connection test failed: {}", test_msg)));
    }

    // Upsert the connection
    let existing = sqlx::query_scalar::<_, i32>(
        "SELECT COUNT(*) FROM app_connections WHERE tenant_id = $1 AND app_slug = $2"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_one(&s.db)
    .await
    .map_err(|_| AppError::Internal("DB error".into()))?;

    let config = r.config.unwrap_or_else(|| json!({}));

    if existing > 0 {
        // Update existing connection
        sqlx::query(
            "UPDATE app_connections SET credentials = $1, config = $2, status = 'connected', last_test_at = NOW(), last_test_ok = true, error_message = NULL, updated_at = NOW() WHERE tenant_id = $3 AND app_slug = $4"
        )
        .bind(&r.credentials)
        .bind(&config)
        .bind(account_id)
        .bind(&app_slug)
        .execute(&s.db)
        .await?;
    } else {
        // Create new connection
        sqlx::query(
            "INSERT INTO app_connections (id, tenant_id, app_slug, credentials, config, status) VALUES ($1, $2, $3, $4, $5, 'connected')"
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(&app_slug)
        .bind(&r.credentials)
        .bind(&config)
        .execute(&s.db)
        .await?;
    }

    Ok(Json(json!({
        "message": format!("{} connected successfully", app.name),
        "app_slug": app_slug,
        "status": "connected",
        "test_result": test_msg,
    })))
}

// ── Disconnect an app ──

pub async fn disconnect_app(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let r = sqlx::query("DELETE FROM app_connections WHERE tenant_id = $1 AND app_slug = $2")
        .bind(account_id)
        .bind(&app_slug)
        .execute(&s.db)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("No connection found for '{}'", app_slug)));
    }

    Ok(Json(json!({"message": format!("{} disconnected", app_slug)})))
}

// ── Get connection status for an app ──

pub async fn app_status(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let app_meta = connectors::get_app_meta(&app_slug)
        .ok_or_else(|| AppError::NotFound(format!("App '{}' not found", app_slug)))?;

    let conn = sqlx::query_as::<_, (String, Option<bool>, Option<String>, Option<chrono::DateTime<chrono::Utc>>, serde_json::Value, serde_json::Value)>(
        "SELECT status, last_test_ok, error_message, last_test_at, credentials, config FROM app_connections WHERE tenant_id = $1 AND app_slug = $2"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_optional(&s.db)
    .await?;

    match conn {
        Some((status, test_ok, error, last_test, credentials, config)) => {
            Ok(Json(json!({
                "app_slug": app_slug,
                "connected": true,
                "status": status,
                "last_test_ok": test_ok,
                "last_test_at": last_test,
                "error_message": error,
                "config": config,
                "credentials_provided": !credentials.as_object().map(|o| o.is_empty()).unwrap_or(true),
                "meta": app_meta,
            })))
        }
        None => {
            Ok(Json(json!({
                "app_slug": app_slug,
                "connected": false,
                "meta": app_meta,
            })))
        }
    }
}

// ── Test a connection ──

pub async fn test_connection(
    State(_s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(r): Json<ConnectAppRequest>,
) -> ApiResult<impl IntoResponse> {
    let _is_admin = c.role == "account_owner" || c.role == "admin";

    let app = connectors::NATIVE_APPS.iter().find(|a| a.slug == app_slug)
        .ok_or_else(|| AppError::NotFound(format!("App '{}' not found", app_slug)))?;

    let (ok, msg, latency) = connectors::test_connection(&app_slug, &r.credentials).await;

    Ok(Json(json!({
        "success": ok,
        "message": msg,
        "latency_ms": latency,
        "app_name": app.name,
    })))
}

// ── Pull data from an app ──

pub async fn pull_from_app(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(r): Json<PullRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Fetch the connection credentials
    let row = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT credentials FROM app_connections WHERE tenant_id = $1 AND app_slug = $2 AND status = 'connected'"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("No active connection for '{}'", app_slug)))?;

    let sync_id = Uuid::new_v4();

    // Log start
    sqlx::query(
        "INSERT INTO app_sync_logs (id, tenant_id, app_slug, direction, entity_type, status, started_at) VALUES ($1, $2, $3, 'pull', $4, 'running', NOW())"
    )
    .bind(sync_id)
    .bind(account_id)
    .bind(&app_slug)
    .bind(&r.entity_type)
    .execute(&s.db)
    .await?;

    // Execute pull
    let filters = r.filters.unwrap_or_default();
    match connectors::pull_data(&app_slug, &row.0, &r.entity_type, &filters).await {
        Ok(data) => {
            // Mark sync as completed
            let records = data.as_array().map(|a| a.len() as i32).unwrap_or(1);
            sqlx::query(
                "UPDATE app_sync_logs SET status = 'completed', records_processed = $1, records_succeeded = $1, completed_at = NOW() WHERE id = $2"
            )
            .bind(records)
            .bind(sync_id)
            .execute(&s.db)
            .await?;

            Ok(Json(json!({
                "sync_id": sync_id,
                "status": "completed",
                "records": records,
                "data": data,
            })))
        }
        Err(e) => {
            sqlx::query(
                "UPDATE app_sync_logs SET status = 'failed', records_failed = 1, error_log = $1, completed_at = NOW() WHERE id = $2"
            )
            .bind(json!({"error": &e}))
            .bind(sync_id)
            .execute(&s.db)
            .await?;

            Err(AppError::Internal(format!("Pull failed: {}", e)))
        }
    }
}

// ── Push data to an app ──

pub async fn push_to_app(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(r): Json<PushRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT credentials FROM app_connections WHERE tenant_id = $1 AND app_slug = $2 AND status = 'connected'"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("No active connection for '{}'", app_slug)))?;

    let sync_id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO app_sync_logs (id, tenant_id, app_slug, direction, entity_type, status, started_at) VALUES ($1, $2, $3, 'push', $4, 'running', NOW())"
    )
    .bind(sync_id)
    .bind(account_id)
    .bind(&app_slug)
    .bind(&r.entity_type)
    .execute(&s.db)
    .await?;

    match connectors::push_data(&app_slug, &row.0, &r.entity_type, &r.data).await {
        Ok(result) => {
            sqlx::query(
                "UPDATE app_sync_logs SET status = 'completed', records_processed = 1, records_succeeded = 1, completed_at = NOW() WHERE id = $1"
            )
            .bind(sync_id)
            .execute(&s.db)
            .await?;

            Ok(Json(json!({
                "sync_id": sync_id,
                "status": "completed",
                "result": result,
            })))
        }
        Err(e) => {
            sqlx::query(
                "UPDATE app_sync_logs SET status = 'failed', records_failed = 1, error_log = $1, completed_at = NOW() WHERE id = $2"
            )
            .bind(json!({"error": &e}))
            .bind(sync_id)
            .execute(&s.db)
            .await?;

            Err(AppError::Internal(format!("Push failed: {}", e)))
        }
    }
}

// ── Sync history ──

pub async fn sync_history(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        "SELECT id, app_slug, direction, entity_type, records_processed, records_succeeded, records_failed, status, started_at, completed_at FROM app_sync_logs WHERE tenant_id = $1 AND app_slug = $2 ORDER BY started_at DESC LIMIT 50"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_all(&s.db)
    .await?;

    let logs: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.get::<Uuid, _>("id"),
            "app_slug": r.get::<String, _>("app_slug"),
            "direction": r.get::<String, _>("direction"),
            "entity_type": r.get::<String, _>("entity_type"),
            "records_processed": r.get::<i32, _>("records_processed"),
            "records_succeeded": r.get::<i32, _>("records_succeeded"),
            "records_failed": r.get::<i32, _>("records_failed"),
            "status": r.get::<String, _>("status"),
            "started_at": r.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
            "completed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("completed_at"),
        })
    }).collect();

    Ok(Json(json!({"sync_history": logs})))
}

// ── Admin-only: get global app config ──

pub async fn get_admin_config(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "owner" && c.role != "admin" {
        return Err(AppError::Forbidden);
    }

    let config: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT config FROM app_admin_configs WHERE app_slug = $1"
    )
    .bind(&app_slug)
    .fetch_optional(&s.db)
    .await?;

    Ok(Json(json!({
        "app_slug": app_slug,
        "config": config.unwrap_or_else(|| json!({})),
    })))
}

pub async fn update_admin_config(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(config): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "owner" && c.role != "admin" {
        return Err(AppError::Forbidden);
    }

    sqlx::query(
        "INSERT INTO app_admin_configs (id, app_slug, config) VALUES ($1, $2, $3) ON CONFLICT (app_slug) DO UPDATE SET config = $3, updated_at = NOW()"
    )
    .bind(Uuid::new_v4())
    .bind(&app_slug)
    .bind(&config)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({"message": "Config updated", "app_slug": app_slug})))
}

pub async fn list_admin_configs(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    if c.role != "owner" && c.role != "admin" {
        return Err(AppError::Forbidden);
    }

    let rows = sqlx::query(
        "SELECT app_slug, config, updated_at FROM app_admin_configs ORDER BY app_slug"
    )
    .fetch_all(&s.db)
    .await?;

    let configs: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "app_slug": r.get::<String, _>("app_slug"),
            "config": r.get::<serde_json::Value, _>("config"),
            "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        })
    }).collect();

    Ok(Json(json!({"admin_configs": configs})))
}

// ── Ada Campaign Triggers (replaces Mailgun for welcome emails) ──

pub async fn create_ada_campaign_trigger(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<AdaCampaignRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    if r.name.is_empty() || r.ada_campaign_id.is_empty() {
        return Err(AppError::Validation("Name and ada_campaign_id are required".into()));
    }

    let valid_triggers = ["user_created", "contact_created", "account_activated", "scan_complete", "referral_confirmed", "commission_earned", "payout_processed", "affiliate_activated"];
    if !valid_triggers.contains(&r.trigger_on.as_str()) {
        return Err(AppError::Validation(format!("Invalid trigger. Must be one of: {:?}", valid_triggers)));
    }

    let row = sqlx::query(
        r#"INSERT INTO ada_campaign_triggers (id, tenant_id, name, trigger_on, ada_campaign_id, schedule_delay_minutes, active)
           VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(&r.name)
    .bind(&r.trigger_on)
    .bind(&r.ada_campaign_id)
    .bind(r.schedule_delay_minutes.unwrap_or(0))
    .bind(r.active.unwrap_or(true))
    .fetch_one(&s.db)
    .await?;

    let trigger = json!({
        "id": row.get::<Uuid, _>("id"),
        "tenant_id": row.get::<Uuid, _>("tenant_id"),
        "name": row.get::<String, _>("name"),
        "trigger_on": row.get::<String, _>("trigger_on"),
        "ada_campaign_id": row.get::<String, _>("ada_campaign_id"),
        "schedule_delay_minutes": row.get::<i32, _>("schedule_delay_minutes"),
        "active": row.get::<bool, _>("active"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    });

    Ok((StatusCode::CREATED, Json(json!({"trigger": trigger}))))
}

pub async fn list_ada_campaign_triggers(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let rows = sqlx::query(
        "SELECT * FROM ada_campaign_triggers WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(account_id)
    .fetch_all(&s.db)
    .await?;

    let triggers: Vec<serde_json::Value> = rows.iter().map(|r| {
        json!({
            "id": r.get::<Uuid, _>("id"),
            "tenant_id": r.get::<Uuid, _>("tenant_id"),
            "name": r.get::<String, _>("name"),
            "trigger_on": r.get::<String, _>("trigger_on"),
            "ada_campaign_id": r.get::<String, _>("ada_campaign_id"),
            "schedule_delay_minutes": r.get::<i32, _>("schedule_delay_minutes"),
            "active": r.get::<bool, _>("active"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
        })
    }).collect();

    Ok(Json(json!({"triggers": triggers})))
}

pub async fn delete_ada_campaign_trigger(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let r = sqlx::query("DELETE FROM ada_campaign_triggers WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(account_id)
        .execute(&s.db)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Trigger not found".into()));
    }

    Ok(Json(json!({"message": "Trigger deleted"})))
}
