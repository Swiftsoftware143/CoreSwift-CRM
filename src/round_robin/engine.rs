use sqlx::PgPool;
use uuid::Uuid;
use crate::errors::AppError;
use super::models::*;

/// Assign a booking/contact to the next available team member using the configured strategy.
pub async fn assign_lead(
    db: &PgPool,
    team_id: Uuid,
    booking_id: Option<Uuid>,
    contact_id: Option<Uuid>,
) -> Result<RoundRobinAssignment, AppError> {
    let team = sqlx::query_as::<_, RoundRobinTeam>(
        "SELECT * FROM round_robin_teams WHERE id = $1 AND is_active = true"
    )
    .bind(team_id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("Team not found".into()))?;

    match team.strategy.as_str() {
        "round_robin" => assign_round_robin(db, &team, booking_id, contact_id).await,
        "least_loaded" => assign_least_loaded(db, &team, booking_id, contact_id).await,
        _ => Err(AppError::Validation("Unknown strategy".into())),
    }
}

/// Find the active round-robin team scoped to a specific calendar.
pub async fn find_team_for_calendar(
    db: &PgPool,
    calendar_id: Uuid,
) -> Option<Uuid> {
    // Check for a team specifically scoped to this calendar first
    let result = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM round_robin_teams
         WHERE scope_type = 'calendar' AND scope_id = $1 AND is_active = true
         LIMIT 1"
    )
    .bind(calendar_id)
    .fetch_optional(db)
    .await
    .ok()?;

    if result.is_some() {
        return result;
    }

    // Fall back to a global team for this calendar's tenant
    let cal_tenant = sqlx::query_scalar::<_, Uuid>(
        "SELECT tenant_id FROM booking_calendars WHERE id = $1"
    )
    .bind(calendar_id)
    .fetch_optional(db)
    .await
    .ok()??;

    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM round_robin_teams
         WHERE tenant_id = $1 AND scope_type = 'global' AND is_active = true
         LIMIT 1"
    )
    .bind(cal_tenant)
    .fetch_optional(db)
    .await
    .ok()?
}

async fn assign_round_robin(
    db: &PgPool,
    team: &RoundRobinTeam,
    booking_id: Option<Uuid>,
    contact_id: Option<Uuid>,
) -> Result<RoundRobinAssignment, AppError> {
    // Find the member with the fewest recent assignments (round-robin)
    let member = sqlx::query_as::<_, RoundRobinMember>(
        r#"SELECT rm.* FROM round_robin_members rm
           LEFT JOIN round_robin_assignments ra ON ra.member_id = rm.id AND ra.status = 'pending'
           WHERE rm.team_id = $1 AND rm.is_active = true
           GROUP BY rm.id
           ORDER BY COUNT(ra.id) ASC
           LIMIT 1"#
    )
    .bind(team.id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("No active team members".into()))?;

    let assignment = sqlx::query_as::<_, RoundRobinAssignment>(
        "INSERT INTO round_robin_assignments(id, team_id, member_id, booking_id, contact_id)
         VALUES($1, $2, $3, $4, $5) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(team.id)
    .bind(member.id)
    .bind(booking_id)
    .bind(contact_id)
    .fetch_one(db)
    .await?;

    Ok(assignment)
}

async fn assign_least_loaded(
    db: &PgPool,
    team: &RoundRobinTeam,
    booking_id: Option<Uuid>,
    contact_id: Option<Uuid>,
) -> Result<RoundRobinAssignment, AppError> {
    let member = sqlx::query_as::<_, RoundRobinMember>(
        r#"SELECT rm.* FROM round_robin_members rm
           LEFT JOIN round_robin_assignments ra ON ra.member_id = rm.id AND ra.status IN ('pending', 'active')
           WHERE rm.team_id = $1 AND rm.is_active = true
           GROUP BY rm.id
           HAVING COUNT(ra.id) < rm.max_concurrent_bookings
           ORDER BY COUNT(ra.id) ASC
           LIMIT 1"#
    )
    .bind(team.id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound("No available team members (all at capacity)".into()))?;

    let assignment = sqlx::query_as::<_, RoundRobinAssignment>(
        "INSERT INTO round_robin_assignments(id, team_id, member_id, booking_id, contact_id)
         VALUES($1, $2, $3, $4, $5) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(team.id)
    .bind(member.id)
    .bind(booking_id)
    .bind(contact_id)
    .fetch_one(db)
    .await?;

    Ok(assignment)
}
