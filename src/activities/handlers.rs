//! Activities handlers — unified activity feed for the current tenant.

use axum::{
    extract::{State, Extension, Query},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult, validate_pagination};

/// Query parameters for listing activities.
#[derive(Debug, Deserialize)]
pub struct ActivitiesQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
    pub entity_type: Option<String>,
}

/// A single activity item in the feed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ActivityItem {
    pub activity_type: String,
    pub entity_type: Option<String>,
    pub entity_id: Option<Uuid>,
    pub description: Option<String>,
    pub user_name: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// GET /api/activities
///
/// Returns a unified, chronological feed of recent activity for the current tenant.
/// Sources include events, audit logs, and opportunity stage changes.
pub async fn list_activities(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ActivitiesQuery>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    // Build a union of recent activity from events, audit_logs, and stage changes.
    // Filter by entity_type if provided.
    let entity_filter = params.entity_type.clone().unwrap_or_default();

    let activities = if entity_filter.is_empty() {
        sqlx::query_as::<_, ActivityItem>(
            r#"SELECT * FROM (
                -- Events
                SELECT
                    'event' AS activity_type,
                    entity_type,
                    entity_id,
                    event_type AS description,
                    NULL::text AS user_name,
                    created_at AS timestamp
                FROM events
                WHERE tenant_id = $1

                UNION ALL

                -- Audit logs
                SELECT
                    'audit_log' AS activity_type,
                    entity_type,
                    entity_id,
                    SUBSTRING(detail::text, 1, 200) AS description,
                    (SELECT name FROM users WHERE id = audit_logs.user_id) AS user_name,
                    created_at AS timestamp
                FROM audit_logs
                WHERE tenant_id = $1

                UNION ALL

                -- Stage changes
                SELECT
                    'stage_change' AS activity_type,
                    'opportunity' AS entity_type,
                    h.opportunity_id AS entity_id,
                    'Deal moved from ' || COALESCE(fs.name, 'Unknown') || ' to ' || COALESCE(ts.name, 'Unknown') AS description,
                    u.name AS user_name,
                    h.moved_at AS timestamp
                FROM opportunity_stage_history h
                LEFT JOIN pipeline_stages fs ON fs.id = h.from_stage_id
                LEFT JOIN pipeline_stages ts ON ts.id = h.to_stage_id
                LEFT JOIN users u ON u.id = h.moved_by
                WHERE EXISTS (SELECT 1 FROM opportunities o WHERE o.id = h.opportunity_id AND o.tenant_id = $1)
            ) AS combined
            ORDER BY timestamp DESC
            LIMIT $2 OFFSET $3"#
        )
        .bind(account_id)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as::<_, ActivityItem>(
            r#"SELECT * FROM (
                SELECT
                    'event' AS activity_type,
                    entity_type,
                    entity_id,
                    event_type AS description,
                    NULL::text AS user_name,
                    created_at AS timestamp
                FROM events
                WHERE tenant_id = $1 AND entity_type = $2

                UNION ALL

                SELECT
                    'audit_log' AS activity_type,
                    entity_type,
                    entity_id,
                    SUBSTRING(detail::text, 1, 200) AS description,
                    (SELECT name FROM users WHERE id = audit_logs.user_id) AS user_name,
                    created_at AS timestamp
                FROM audit_logs
                WHERE tenant_id = $1 AND entity_type = $2

                UNION ALL

                SELECT
                    'stage_change' AS activity_type,
                    'opportunity' AS entity_type,
                    h.opportunity_id AS entity_id,
                    'Deal moved from ' || COALESCE(fs.name, 'Unknown') || ' to ' || COALESCE(ts.name, 'Unknown') AS description,
                    u.name AS user_name,
                    h.moved_at AS timestamp
                FROM opportunity_stage_history h
                LEFT JOIN pipeline_stages fs ON fs.id = h.from_stage_id
                LEFT JOIN pipeline_stages ts ON ts.id = h.to_stage_id
                LEFT JOIN users u ON u.id = h.moved_by
                WHERE EXISTS (SELECT 1 FROM opportunities o WHERE o.id = h.opportunity_id AND o.tenant_id = $1)
                  AND $2 = 'opportunity'
            ) AS combined
            ORDER BY timestamp DESC
            LIMIT $3 OFFSET $4"#
        )
        .bind(account_id)
        .bind(&entity_filter)
        .bind(per_page)
        .bind(offset)
        .fetch_all(&state.db)
        .await?
    };

    Ok(Json(json!({
        "activities": activities,
        "page": page,
        "per_page": per_page,
    })))
}
