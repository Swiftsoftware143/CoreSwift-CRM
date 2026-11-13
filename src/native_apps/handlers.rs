//! Native App Connectors — API handlers

use super::connectors;
use super::credentials;
use super::models::*;
use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, State},
    response::IntoResponse,
};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;

// ── List all available apps (visible to all authenticated users) ──

pub async fn list_available_apps() -> ApiResult<impl IntoResponse> {
    let apps: Vec<serde_json::Value> = connectors::NATIVE_APPS
        .iter()
        .map(|a| {
            json!({
                "slug": a.slug,
                "name": a.name,
                "description": a.description,
                "auth_type": a.auth_type,
                "auth_fields": a.auth_fields,
                "access_level": a.access_level,
                "meta": connectors::get_app_meta(a.slug),
            })
        })
        .collect();
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
    // Three role vocabularies exist ('owner' is what signup writes, 'agency_admin'
    // is the platform operator) — see auth::middleware::is_account_admin.
    let is_admin = crate::auth::middleware::is_account_admin(&c.role);

    // Look up the app definition
    let app = connectors::NATIVE_APPS
        .iter()
        .find(|a| a.slug == app_slug)
        .ok_or_else(|| AppError::NotFound(format!("App '{}' not found", app_slug)))?;

    // Access level — the ONE rule, shared with the automation door via
    // `connectors::connect_refusal` (kanban t_0db782c6). This door can prove an admin PERSON
    // (the signed-in role), so an account admin may connect an admin-only app here while the
    // same tenant's automation token may not: `webhook::actions::route_action`'s
    // `native.connect` arm passes `is_person_admin = false` because an `automation_webhooks`
    // row names a tenant, never a user. The reason the webhook side is refused rather than this
    // side widened is written on `connect_refusal`.
    if connectors::connect_refusal(app, is_admin).is_some() {
        return Err(AppError::Forbidden);
    }

    // Validate required fields
    for field in app.auth_fields {
        let value = r.credentials.get(*field).and_then(|v| v.as_str());
        match value {
            Some(v) if !v.is_empty() => {}
            _ => return Err(AppError::Validation(format!("{} is required", field))),
        }
    }

    // Test the connection — with the caller's own plaintext, before anything is stored.
    let (test_ok, test_msg, _latency) =
        connectors::test_connection(&app_slug, &r.credentials).await;
    if !test_ok {
        return Err(AppError::Validation(format!(
            "Connection test failed: {}",
            test_msg
        )));
    }

    // SEAL the secret fields before they touch the column (kanban t_a1ada5ef): a sister app's API
    // key is a durable credential, so it is stored in the app-wide `enc:v1:` envelope, never in the
    // clear. This fails closed when the master key is missing — no plaintext fallback, ever.
    // `base_url` and any other config stay readable (the panel renders them).
    let credentials = credentials::seal_for_storage(account_id, &r.credentials)?;

    // Upsert the connection
    // COUNT(*) is bigint; decoding it into i32 fails at runtime ("mismatched
    // types") which the old `.map_err(|_| Internal("DB error"))` hid behind a
    // generic 500 -- i.e. connecting any app was impossible.
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM app_connections WHERE tenant_id = $1 AND app_slug = $2",
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_one(&s.db)
    .await
    .map_err(|_| AppError::Internal("DB error".into()))?;

    let config = r.config.unwrap_or_else(|| json!({}));

    if existing > 0 {
        // Update existing connection. `test_ok` is bound (instead of a literal
        // `true`) so the row records the verdict this request actually got —
        // the early return above makes it true, this keeps the two arms honest.
        sqlx::query(
            "UPDATE app_connections SET credentials = $1, config = $2, status = 'connected', last_test_at = NOW(), last_test_ok = $3, error_message = NULL, updated_at = NOW() WHERE tenant_id = $4 AND app_slug = $5"
        )
        .bind(&credentials)
        .bind(&config)
        .bind(test_ok)
        .bind(account_id)
        .bind(&app_slug)
        .execute(&s.db)
        .await?;
    } else {
        // Create new connection — and record the test this request just ran.
        // The UPDATE arm always did; the INSERT arm used to write only
        // (id, tenant_id, app_slug, credentials, config, status), so a
        // first-ever connection showed as "never tested" in the Integration
        // Center until the same app was connected a second time (t_4d2776a4).
        sqlx::query(
            "INSERT INTO app_connections (id, tenant_id, app_slug, credentials, config, status, last_test_at, last_test_ok, error_message) VALUES ($1, $2, $3, $4, $5, 'connected', NOW(), $6, NULL)"
        )
        .bind(Uuid::new_v4())
        .bind(account_id)
        .bind(&app_slug)
        .bind(&credentials)
        .bind(&config)
        .bind(test_ok)
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
        return Err(AppError::NotFound(format!(
            "No connection found for '{}'",
            app_slug
        )));
    }

    Ok(Json(
        json!({"message": format!("{} disconnected", app_slug)}),
    ))
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
        Some((status, test_ok, error, last_test, credentials, config)) => Ok(Json(json!({
            "app_slug": app_slug,
            "connected": true,
            "status": status,
            "last_test_ok": test_ok,
            "last_test_at": last_test,
            "error_message": error,
            "config": config,
            "credentials_provided": credentials::has_secret(&credentials),
            "meta": app_meta,
        }))),
        None => Ok(Json(json!({
            "app_slug": app_slug,
            "connected": false,
            "meta": app_meta,
        }))),
    }
}

// ── Test a connection ──

pub async fn test_connection(
    State(_s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(r): Json<ConnectAppRequest>,
) -> ApiResult<impl IntoResponse> {
    let _is_admin = crate::auth::middleware::is_account_admin(&c.role);

    let app = connectors::NATIVE_APPS
        .iter()
        .find(|a| a.slug == app_slug)
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
    ensure_declared(&app_slug, "pull", &r.entity_type)?;

    // Fetch the connection credentials
    let row = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT credentials FROM app_connections WHERE tenant_id = $1 AND app_slug = $2 AND status = 'connected'"
    )
    .bind(account_id)
    .bind(&app_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("No active connection for '{}'", app_slug)))?;

    // Open the sealed credential before the connector sees it (kanban t_a1ada5ef). A stored value
    // this deployment cannot open is a 422 here — never a connector call with an empty or envelope
    // credential.
    let credentials = credentials::open_for_use(account_id, &row.0)?;

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
    match connectors::pull_data(&app_slug, &credentials, &r.entity_type, &filters).await {
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

/// Refuse an entity the connector does not declare, as caller input (422).
///
/// The connector's OWN declaration (`get_meta().entities`) is the contract: the shipped sync
/// modal builds its dropdown from exactly this list. A name outside it can only arrive from a
/// hand-written API call, and `push_data`/`pull_data` answer such a name with an honest
/// `<App> does not support entity type: <x>` — which the two callers below wrapped in
/// `AppError::Internal`, so the caller saw a bare `500 Internal server error` instead
/// (measured live 2026-10-02, kanban t_8b81b1dd). Undeclared is now the same 422 refusal that
/// the connector's own message describes, and the declaration is load-bearing on the write path.
///
/// Every connector's `push_entity`/`pull_entity` match arms are exactly its declared lists
/// (checked across all five native connectors that remain: adaswift/workflowswift/funnelswift/
/// missedcall_responder/multi_directory), so this refuses nothing that used to work.
fn ensure_declared(app_slug: &str, direction: &str, entity_type: &str) -> ApiResult<()> {
    let declared: Vec<String> = connectors::get_app_meta(app_slug)
        .and_then(|m| m.get("entities").cloned())
        .and_then(|e| e.get(direction).cloned())
        .and_then(|l| l.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();

    if declared.iter().any(|d| d.as_str() == entity_type) {
        return Ok(());
    }
    Err(AppError::Validation(format!(
        "'{}' does not support {} entity type '{}' (declared: {})",
        app_slug,
        direction,
        entity_type,
        if declared.is_empty() {
            "none".to_string()
        } else {
            declared.join(", ")
        }
    )))
}

pub async fn push_to_app(
    State(_s): State<AppState>,
    Extension(_c): Extension<Claims>,
    Path(app_slug): Path<String>,
    Json(_r): Json<PushRequest>,
) -> Result<axum::Json<serde_json::Value>, AppError> {
    // ONE-WAY BY DESIGN (David, 2026-11-13): *"data only flows from the sister apps. CoreSwift is the
    // last destination. It never sends data through its own API — it only receives."*
    //
    // Contacts arrive on /api/external/contacts (and on the mapped intake); the ONLY thing CoreSwift
    // sends out is the portfolio-company broadcast in src/portfolio/sync.rs, which is a control
    // message, not contact data.
    //
    // Refused rather than deleted so the rule is visible and reversible; the connector modules stay
    // for the PULL direction (reading a sibling's data in is still this CRM receiving).
    let _ = app_slug;
    Err::<axum::Json<serde_json::Value>, AppError>(AppError::BadRequest(
        "CoreSwift does not send contacts out to sister apps — data flows one way, into this CRM. \
         Have the app push to /api/external/contacts with this account's key instead."
            .into(),
    ))
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

    let logs: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
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
        })
        .collect();

    Ok(Json(json!({"sync_history": logs})))
}

// ── Admin-only: get global app config ──

/// GLOBAL (platform-wide) app configuration spans every tenant, so it is gated on platform-admin
/// authority resolved from the database by token subject (`crate::auth::platform_admin`) — see that
/// module for why no `Claims.role` string can carry it (39 of 47 production users held a role this
/// gate used to accept).

pub async fn get_admin_config(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(app_slug): Path<String>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    let config: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT config FROM app_admin_configs WHERE app_slug = $1")
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
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    sqlx::query(
        "INSERT INTO app_admin_configs (id, app_slug, config) VALUES ($1, $2, $3) ON CONFLICT (app_slug) DO UPDATE SET config = $3, updated_at = NOW()"
    )
    .bind(Uuid::new_v4())
    .bind(&app_slug)
    .bind(&config)
    .execute(&s.db)
    .await?;

    Ok(Json(
        json!({"message": "Config updated", "app_slug": app_slug}),
    ))
}

pub async fn list_admin_configs(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    let rows =
        sqlx::query("SELECT app_slug, config, updated_at FROM app_admin_configs ORDER BY app_slug")
            .fetch_all(&s.db)
            .await?;

    let configs: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "app_slug": r.get::<String, _>("app_slug"),
                "config": r.get::<serde_json::Value, _>("config"),
                "updated_at": r.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
            })
        })
        .collect();

    Ok(Json(json!({"admin_configs": configs})))
}

// ── Ada campaign triggers ── RETIRED 2026-10-02 (kanban t_434b240b)
//
// `ada_campaign_triggers`, its three routes (`/apps/ada-campaigns`) and the
// `AdaCampaignTrigger` / `AdaCampaignRequest` types were removed. Measured live: the table held
// 0 rows, no code path read `trigger_on`, and driving `contact_created` plus a tag assign produced
// no outbound call — a trigger could never fire a campaign. The four affiliate event names it still
// accepted had no emitter at all (the affiliate module was deleted by t_3d81b041 / migration 108).
// `migrations/109_retire_ada_campaign_triggers.sql` drops the table. The manual Integration Center
// push is the only remaining path that talks to AdaSwift — and its push entity is now `client`
// (`trigger_campaign` went with the canonical paths, kanban t_8b81b1dd).
