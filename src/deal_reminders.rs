use axum::{
    extract::{State, Path, Json, Extension, Query},
    http::StatusCode,
    response::IntoResponse,
    Router, middleware,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult, validate_pagination};

/// Build the router for deal reminders endpoints
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(list_reminders))
        .route("/", axum::routing::post(create_reminder))
        .route("/{id}/dismiss", axum::routing::put(dismiss_reminder))
        .route("/{id}", axum::routing::get(get_reminder))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}

/// DealReminder database model
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DealReminder {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub opportunity_id: Uuid,
    pub user_id: Uuid,
    pub remind_at: DateTime<Utc>,
    pub reminder_type: String,
    pub note: Option<String>,
    pub is_dismissed: bool,
    pub dismissed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request to create a new deal reminder
#[derive(Debug, Deserialize)]
pub struct CreateReminderRequest {
    pub opportunity_id: Uuid,
    pub remind_at: DateTime<Utc>,
    pub note: Option<String>,
}

/// Query parameters for listing reminders
#[derive(Debug, Deserialize)]
pub struct ListRemindersParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub opportunity_id: Option<Uuid>,
    pub is_dismissed: Option<bool>,
}

/// GET /api/deal-reminders
/// List reminders for the authenticated tenant, with optional filters
pub async fn list_reminders(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ListRemindersParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    // Count total (for pagination)
    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deal_reminders WHERE tenant_id = $1"
    )
    .bind(account_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    // Fetch reminders with optional filters
    let reminders = if let Some(opp_id) = params.opportunity_id {
        if let Some(dismissed) = params.is_dismissed {
            sqlx::query_as::<_, DealReminder>(
                "SELECT * FROM deal_reminders WHERE tenant_id = $1 AND opportunity_id = $2 AND is_dismissed = $3 ORDER BY remind_at ASC LIMIT $4 OFFSET $5"
            )
            .bind(account_id).bind(opp_id).bind(dismissed).bind(per_page).bind(offset)
            .fetch_all(&state.db).await?
        } else {
            sqlx::query_as::<_, DealReminder>(
                "SELECT * FROM deal_reminders WHERE tenant_id = $1 AND opportunity_id = $2 ORDER BY remind_at ASC LIMIT $3 OFFSET $4"
            )
            .bind(account_id).bind(opp_id).bind(per_page).bind(offset)
            .fetch_all(&state.db).await?
        }
    } else if let Some(dismissed) = params.is_dismissed {
        sqlx::query_as::<_, DealReminder>(
            "SELECT * FROM deal_reminders WHERE tenant_id = $1 AND is_dismissed = $2 ORDER BY remind_at ASC LIMIT $3 OFFSET $4"
        )
        .bind(account_id).bind(dismissed).bind(per_page).bind(offset)
        .fetch_all(&state.db).await?
    } else {
        sqlx::query_as::<_, DealReminder>(
            "SELECT * FROM deal_reminders WHERE tenant_id = $1 ORDER BY remind_at ASC LIMIT $2 OFFSET $3"
        )
        .bind(account_id).bind(per_page).bind(offset)
        .fetch_all(&state.db).await?
    };

    // Count overdue (not dismissed, remind_at < now)
    let overdue_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deal_reminders WHERE tenant_id = $1 AND is_dismissed = false AND remind_at < NOW()"
    )
    .bind(account_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "reminders": reminders,
        "overdue_count": overdue_count,
        "page": page,
        "per_page": per_page,
        "total": total
    })))
}

/// POST /api/deal-reminders
/// Create a new reminder for an opportunity
pub async fn create_reminder(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateReminderRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    // Verify the opportunity exists and belongs to the tenant
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM opportunities WHERE id = $1 AND tenant_id = $2)"
    )
    .bind(req.opportunity_id).bind(account_id)
    .fetch_one(&state.db).await?;

    if !exists {
        return Err(AppError::NotFound(format!("Opportunity {} not found", req.opportunity_id)));
    }

    let reminder = sqlx::query_as::<_, DealReminder>(
        r#"INSERT INTO deal_reminders (id, tenant_id, opportunity_id, user_id, remind_at, note)
           VALUES ($1, $2, $3, $4, $5, $6) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(req.opportunity_id)
    .bind(user_id)
    .bind(req.remind_at)
    .bind(&req.note)
    .fetch_one(&state.db).await?;

    Ok((StatusCode::CREATED, Json(json!(reminder))))
}

/// GET /api/deal-reminders/{id}
/// Get a single reminder by ID
pub async fn get_reminder(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let reminder = sqlx::query_as::<_, DealReminder>(
        "SELECT * FROM deal_reminders WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id).bind(account_id)
    .fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Reminder {} not found", id)))?;

    Ok(Json(json!(reminder)))
}

/// PUT /api/deal-reminders/{id}/dismiss
/// Dismiss a reminder (mark as dismissed)
pub async fn dismiss_reminder(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let reminder = sqlx::query_as::<_, DealReminder>(
        r#"UPDATE deal_reminders SET is_dismissed = true, dismissed_at = NOW(), updated_at = NOW()
           WHERE id = $1 AND tenant_id = $2 RETURNING *"#
    )
    .bind(id).bind(account_id)
    .fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Reminder {} not found", id)))?;

    Ok(Json(json!(reminder)))
}
