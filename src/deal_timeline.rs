use axum::{
    extract::{State, Path, Extension, Query},
    response::IntoResponse,
    Json, Router, middleware,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult, validate_pagination};

/// Build the router for deal timeline endpoints
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/{deal_id}/timeline", axum::routing::get(get_timeline))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}

/// A single timeline event for a deal/opportunity
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct TimelineEvent {
    pub event_type: String,
    pub description: String,
    pub field: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub user_id: Option<Uuid>,
    pub user_name: Option<String>,
}

/// Query parameters for timeline
#[derive(Debug, serde::Deserialize)]
pub struct TimelineParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// GET /api/v1/deals/{deal_id}/timeline
/// Returns a chronological timeline of events for a specific opportunity.
/// Combines: created event, stage changes (from opportunity_stage_history),
/// value changes (from audit_logs), and tag assignments.
pub async fn get_timeline(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(deal_id): Path<Uuid>,
    Query(params): Query<TimelineParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    // Verify the opportunity exists and belongs to this tenant
    let opp_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM opportunities WHERE id = $1 AND tenant_id = $2)"
    )
    .bind(deal_id).bind(account_id)
    .fetch_one(&state.db).await?;

    if !opp_exists {
        return Err(AppError::NotFound(format!("Deal {} not found", deal_id)));
    }

    // We'll build the timeline with a UNION ALL of multiple event sources.
    // Use a CTE approach with row_number for pagination.
    let events = sqlx::query_as::<_, TimelineEvent>(
        r#"SELECT * FROM (
            -- 1. Deal created event
            SELECT
                'created' AS event_type,
                'Deal created' AS description,
                NULL::text AS field,
                NULL::text AS old_value,
                NULL::text AS new_value,
                created_at AS timestamp,
                NULL::uuid AS user_id,
                NULL::text AS user_name
            FROM opportunities WHERE id = $1 AND tenant_id = $2

            UNION ALL

            -- 2. Stage change events (from opportunity_stage_history + pipeline_stages names)
            SELECT
                'stage_change' AS event_type,
                CASE
                    WHEN h.from_stage_id IS NULL THEN 'Deal entered stage: ' || COALESCE(ts.name, 'Unknown')
                    ELSE 'Deal moved from ' || COALESCE(fs.name, 'Unknown') || ' to ' || COALESCE(ts.name, 'Unknown')
                END AS description,
                'pipeline_stage' AS field,
                fs.name AS old_value,
                ts.name AS new_value,
                h.moved_at AS timestamp,
                h.moved_by AS user_id,
                u.name AS user_name
            FROM opportunity_stage_history h
            LEFT JOIN pipeline_stages fs ON fs.id = h.from_stage_id
            LEFT JOIN pipeline_stages ts ON ts.id = h.to_stage_id
            LEFT JOIN users u ON u.id = h.moved_by
            WHERE h.opportunity_id = $1

            UNION ALL

            -- 3. Value changes (from audit_logs)
            SELECT
                'value_change' AS event_type,
                'Deal value changed' AS description,
                'value' AS field,
                (changes->>'old_value')::text AS old_value,
                (changes->>'new_value')::text AS new_value,
                created_at AS timestamp,
                user_id,
                NULL::text AS user_name
            FROM audit_logs
            WHERE entity_type = 'opportunity'
              AND entity_id = $1
              AND action = 'opportunity.updated'
              AND changes ? 'value'

            UNION ALL

            -- 4. Tag assignment events
            SELECT
                'tag_assigned' AS event_type,
                'Tag assigned' AS description,
                'tags' AS field,
                NULL::text AS old_value,
                t.name AS new_value,
                ta.assigned_at AS timestamp,
                ta.assigned_by AS user_id,
                u2.name AS user_name
            FROM tag_assignments ta
            JOIN tags t ON t.id = ta.tag_id
            LEFT JOIN users u2 ON u2.id = ta.assigned_by
            WHERE ta.entity_type = 'opportunity'
              AND ta.entity_id = $1
        ) AS timeline
        ORDER BY timestamp DESC
        LIMIT $3 OFFSET $4"#
    )
    .bind(deal_id)
    .bind(account_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db).await?;

    // Count total events
    let total: i64 = sqlx::query_scalar(
        r#"SELECT COUNT(*) FROM (
            SELECT created_at FROM opportunities WHERE id = $1 AND tenant_id = $2
            UNION ALL
            SELECT moved_at FROM opportunity_stage_history WHERE opportunity_id = $1
            UNION ALL
            SELECT created_at FROM audit_logs WHERE entity_type = 'opportunity' AND entity_id = $1 AND action = 'opportunity.updated' AND changes ? 'value'
            UNION ALL
            SELECT assigned_at FROM tag_assignments WHERE entity_type = 'opportunity' AND entity_id = $1
        ) AS all_events"#
    )
    .bind(deal_id)
    .bind(account_id)
    .fetch_one(&state.db)
    .await
    .unwrap_or(0);

    Ok(Json(json!({
        "events": events,
        "page": page,
        "per_page": per_page,
        "total": total
    })))
}
