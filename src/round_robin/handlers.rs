use axum::{
    extract::{Path, State, Extension},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use super::models::*;
use super::engine;

// ── Teams CRUD ───────────────────────────────────────────────────────────

pub async fn list_teams(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let teams = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE tenant_id = $1 ORDER BY name"
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!(teams)))
}

pub async fn create_team(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CreateTeamRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let team = sqlx::query_as::<_, RoundRobinTeam>(
        r#"INSERT INTO round_robin_teams(id, tenant_id, name, description, strategy, scope_type, scope_id)
           VALUES($1, $2, $3, $4, $5, $6, $7) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(tid)
    .bind(&body.name)
    .bind(&body.description)
    .bind(body.strategy.as_deref().unwrap_or("round_robin"))
    .bind(body.scope_type.as_deref().unwrap_or("global"))
    .bind(body.scope_id)
    .fetch_one(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(team))))
}

pub async fn get_team(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;
    Ok(Json(json!(team)))
}

pub async fn update_team(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let existing = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    // Build dynamic update
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or(&existing.name);
    let description = body.get("description").and_then(|v| v.as_str());
    let strategy = body.get("strategy").and_then(|v| v.as_str()).unwrap_or(&existing.strategy);
    let scope_type = body.get("scope_type").and_then(|v| v.as_str()).unwrap_or(&existing.scope_type);
    let scope_id = body.get("scope_id").and_then(|v| v.as_str()).and_then(|v| Uuid::parse_str(v).ok()).or(existing.scope_id);
    let is_active = body.get("is_active").and_then(|v| v.as_bool()).unwrap_or(existing.is_active);

    let team = sqlx::query_as::<_, RoundRobinTeam>(
        r#"UPDATE round_robin_teams SET name = $1, description = $2, strategy = $3,
           scope_type = $4, scope_id = $5, is_active = $6, updated_at = NOW()
           WHERE id = $7 AND tenant_id = $8 RETURNING *"#
    )
    .bind(name)
    .bind(description)
    .bind(strategy)
    .bind(scope_type)
    .bind(scope_id)
    .bind(is_active)
    .bind(id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;
    Ok(Json(json!(team)))
}

pub async fn delete_team(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let result = sqlx::query(
        "DELETE FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(id)
    .bind(tid)
    .execute(&s.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Team not found".into()));
    }
    Ok(Json(json!({"success": true})))
}

// ── Members ──────────────────────────────────────────────────────────────

pub async fn list_members(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(team_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Verify team belongs to tenant
    let _team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(team_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    let members = sqlx::query_as::<_, RoundRobinMember>(
        "SELECT * FROM round_robin_members WHERE team_id = $1 ORDER BY created_at"
    )
    .bind(team_id)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!(members)))
}

pub async fn add_member(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(team_id): Path<Uuid>,
    Json(body): Json<AddMemberRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Verify team belongs to tenant
    let _team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(team_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    let member = sqlx::query_as::<_, RoundRobinMember>(
        r#"INSERT INTO round_robin_members(id, team_id, user_id, weight, max_concurrent_bookings)
           VALUES($1, $2, $3, $4, $5)
           ON CONFLICT(team_id, user_id) DO UPDATE SET is_active = true, weight = EXCLUDED.weight, max_concurrent_bookings = EXCLUDED.max_concurrent_bookings
           RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(body.user_id)
    .bind(body.weight.unwrap_or(1))
    .bind(body.max_concurrent_bookings.unwrap_or(10))
    .fetch_one(&s.db)
    .await?;
    Ok((StatusCode::CREATED, Json(json!(member))))
}

pub async fn remove_member(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path((team_id, member_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Verify team belongs to tenant
    let _team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(team_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    let result = sqlx::query(
        "DELETE FROM round_robin_members WHERE id = $1 AND team_id = $2"
    )
    .bind(member_id)
    .bind(team_id)
    .execute(&s.db)
    .await?;
    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Member not found".into()));
    }
    Ok(Json(json!({"success": true})))
}

// ── Assignments ──────────────────────────────────────────────────────────

pub async fn list_assignments(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(team_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Verify team belongs to tenant
    let _team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND tenant_id = $2"
    )
    .bind(team_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    let assignments = sqlx::query_as::<_, RoundRobinAssignment>(
        "SELECT * FROM round_robin_assignments WHERE team_id = $1 ORDER BY assigned_at DESC"
    )
    .bind(team_id)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!(assignments)))
}

pub async fn trigger_assignment(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    let _tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let team_id = body.get("team_id").and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or_else(|| AppError::Validation("team_id required".into()))?;
    let booking_id = body.get("booking_id").and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok());
    let contact_id = body.get("contact_id").and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok());

    let assignment = engine::assign_lead(&s.db, team_id, booking_id, contact_id).await?;
    Ok((StatusCode::CREATED, Json(json!(assignment))))
}
