//! Account Health Trial Monitor & Churn Prevention Handlers.
//!
//! Provides:
//!   POST /api/account-health/check  — Manual trigger to run health checks
//!   POST /api/account-health/milestone — Record a usage milestone
//!   GET  /api/account-health/status/:profile_id — Get health for a business profile
//!
//! These complement the existing /api/monitoring/health endpoints by focusing on
//! trial-specific detection, milestone tracking, and churn prevention actions.

use axum::{extract::{State, Path, Json}, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;
use chrono::Utc;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use super::models::AccountHealth;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Request body for POST /api/account-health/check
#[derive(Debug, Deserialize)]
pub struct HealthCheckRequest {
    /// Optional tenant_id — if omitted, scans all tenants
    pub tenant_id: Option<Uuid>,
}

/// Response for POST /api/account-health/check
#[derive(Debug, Serialize)]
pub struct HealthCheckResponse {
    pub checked: i64,
    pub at_risk: i64,
    pub churn_flagged: i64,
    pub re_engagement_sent: i64,
}

/// Request body for POST /api/account-health/milestone
#[derive(Debug, Deserialize)]
pub struct MilestoneRequest {
    pub business_profile_id: Uuid,
    pub milestone_type: String,
    pub metadata: Option<serde_json::Value>,
}

/// Response for GET /api/account-health/status/:profile_id
#[derive(Debug, Serialize)]
pub struct ProfileHealthStatus {
    pub profile_id: Uuid,
    pub health: Option<AccountHealth>,
    pub recent_events: Vec<serde_json::Value>,
    pub days_remaining_in_trial: Option<i64>,
    pub trial_expired: bool,
}

// ---------------------------------------------------------------------------
// POST /api/account-health/check
// ---------------------------------------------------------------------------

/// Manually trigger a full health check across all at-risk accounts.
/// Three scans:
///   1. SaaS users registered via event_logs with no activity in 24h
///   2. Trial accounts (tenant_plans.status = 'trialing') within 3 days of expiration
///   3. business_profiles with saas unit, trial-like states, and no recent activity
///
/// For each at-risk account, creates a health signal, and schedules churn-prevention
/// actions (delayed_actions / followup_queue entries).
pub async fn run_health_check(
    State(s): State<AppState>,
    Json(r): Json<HealthCheckRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_filter = r.tenant_id;
    let mut stats = HealthCheckResponse {
        checked: 0,
        at_risk: 0,
        churn_flagged: 0,
        re_engagement_sent: 0,
    };

    // --- Scan 1: SaaS users with no activity in 24h (from event_logs) ---
    let no_activity_users = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        r#"
        SELECT bp.id AS profile_id, bp.user_id, u.tenant_id
        FROM business_profiles bp
        JOIN users u ON bp.user_id = u.id
        WHERE bp.unit = 'saas'
          AND bp.current_state IN ('active', 'pending_onboarding')
          AND NOT EXISTS (
            SELECT 1 FROM event_logs el
            WHERE el.business_profile_id = bp.id
              AND el.created_at > NOW() - INTERVAL '24 hours'
          )
          AND ($1::uuid IS NULL OR u.tenant_id = $1)
        LIMIT 100
        "#
    )
    .bind(tenant_filter)
    .fetch_all(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to scan no-activity users: {}", e)))?;

    stats.checked += no_activity_users.len() as i64;

    for (profile_id, user_id, tenant_id) in &no_activity_users {
        stats.at_risk += 1;

        // Record health signal for inactivity
        crate::monitoring::engine::record_signal(
            &s.db,
            *tenant_id,
            "contact",
            *user_id,
            "days_inactive",
            1,
        ).await;

        // Log the event
        let _ = sqlx::query(
            r#"INSERT INTO event_logs (id, business_profile_id, event_name, metadata, created_at)
               VALUES ($1, $2, 'churn_check.inactive_24h', $3, NOW())"#
        )
        .bind(Uuid::new_v4())
        .bind(profile_id)
        .bind(json!({"detected_by": "account_health_check", "inactive_hours": 24}))
        .execute(&s.db)
        .await;

        // Schedule a re-engagement action via delayed_actions
        let _ = sqlx::query(
            r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
               VALUES ($1, $2, 'timeout', '{}'::jsonb, 'send_email',
                       $3::jsonb,
                       NOW() + INTERVAL '15 minutes')"#
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(json!({
            "to_entity": "contact",
            "entity_id": user_id,
            "template_type": "ai_recommended",
            "stage_title": "Welcome Back — Let's Finish Setting Up",
            "reason": "no_activity_24h"
        }))
        .execute(&s.db)
        .await;

        stats.re_engagement_sent += 1;
    }

    // --- Scan 2: Trial accounts within 3 days of expiration ---
    let expiring_trials = sqlx::query_as::<_, (Uuid, Uuid, Option<chrono::DateTime<Utc>>)>(
        r#"
        SELECT tp.tenant_id, c.id AS contact_id, tp.trial_ends_at
        FROM tenant_plans tp
        JOIN contacts c ON c.tenant_id = tp.tenant_id
        WHERE tp.status = 'trialing'
          AND tp.trial_ends_at IS NOT NULL
          AND tp.trial_ends_at > NOW()
          AND tp.trial_ends_at <= NOW() + INTERVAL '3 days'
          AND ($1::uuid IS NULL OR tp.tenant_id = $1)
        LIMIT 100
        "#
    )
    .bind(tenant_filter)
    .fetch_all(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to scan expiring trials: {}", e)))?;

    stats.checked += expiring_trials.len() as i64;

    for (tenant_id, contact_id, trial_ends_at) in &expiring_trials {
        stats.at_risk += 1;

        let days_left = trial_ends_at
            .map(|t| (t - Utc::now()).num_days().max(0))
            .unwrap_or(0);

        // Record a health signal that their trial is ending
        crate::monitoring::engine::record_signal(
            &s.db,
            *tenant_id,
            "contact",
            *contact_id,
            "days_inactive",
            0, // just signals the event, no score penalty
        ).await;

        // Flag as churn-risky
        stats.churn_flagged += 1;

        // Schedule trial-ending followup
        let _ = sqlx::query(
            r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
               VALUES ($1, $2, 'timeout', '{}'::jsonb, 'send_email',
                       $3::jsonb,
                       $4)"#
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(json!({
            "to_entity": "contact",
            "entity_id": contact_id,
            "template_type": "trial_ending",
            "days_remaining": days_left,
            "stage_title": format!("Your trial ends in {} days — convert now", days_left)
        }))
        .bind(trial_ends_at.unwrap_or_else(Utc::now))
        .execute(&s.db)
        .await;

        stats.re_engagement_sent += 1;
    }

    // --- Scan 3: business_profiles with trial states and old activity ---
    let stale_profiles = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        r#"
        SELECT bp.id, bp.user_id, u.tenant_id
        FROM business_profiles bp
        JOIN users u ON bp.user_id = u.id
        WHERE bp.unit = 'saas'
          AND bp.current_state IN ('lead_captured', 'pending_onboarding', 'active')
          AND bp.subscription_active = false
          AND bp.last_activity_at < NOW() - INTERVAL '3 days'
          AND ($1::uuid IS NULL OR u.tenant_id = $1)
        LIMIT 100
        "#
    )
    .bind(tenant_filter)
    .fetch_all(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to scan stale profiles: {}", e)))?;

    stats.checked += stale_profiles.len() as i64;

    for (profile_id, user_id, tenant_id) in &stale_profiles {
        stats.at_risk += 1;

        crate::monitoring::engine::record_signal(
            &s.db,
            *tenant_id,
            "contact",
            *user_id,
            "days_inactive",
            2,
        ).await;

        let _ = sqlx::query(
            r#"INSERT INTO event_logs (id, business_profile_id, event_name, metadata, created_at)
               VALUES ($1, $2, 'churn_check.stale_profile', $3, NOW())"#
        )
        .bind(Uuid::new_v4())
        .bind(profile_id)
        .bind(json!({"detected_by": "account_health_check", "inactive_days": 3}))
        .execute(&s.db)
        .await;

        let _ = sqlx::query(
            r#"INSERT INTO followup_queue (id, business_profile_id, scheduled_for, channel, template_slug)
               VALUES ($1, $2, NOW() + INTERVAL '1 hour', 'email', 'trial_reactivation')"#
        )
        .bind(Uuid::new_v4())
        .bind(profile_id)
        .execute(&s.db)
        .await;

        stats.re_engagement_sent += 1;
    }

    tracing::info!(
        checked = stats.checked,
        at_risk = stats.at_risk,
        churn_flagged = stats.churn_flagged,
        re_engagement_sent = stats.re_engagement_sent,
        "Account health check complete"
    );

    Ok((StatusCode::OK, Json(json!(stats))))
}

// ---------------------------------------------------------------------------
// POST /api/account-health/milestone
// ---------------------------------------------------------------------------

/// Record a usage milestone for a business profile.
/// Milestone types:
///   first_automation  — first automated rule triggered
///   first_contact     — first contact created
///   first_pipeline    — first pipeline stage reached
///   first_campaign    — first campaign sent
/// If `first_automation`, triggers a congratulatory delayed_action.
pub async fn record_milestone(
    State(s): State<AppState>,
    Json(r): Json<MilestoneRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validate milestone_type
    let valid_types = ["first_automation", "first_contact", "first_pipeline", "first_campaign"];
    if !valid_types.contains(&r.milestone_type.as_str()) {
        return Err(AppError::Validation(format!(
            "milestone_type must be one of: {:?}",
            valid_types
        )));
    }

    // Look up the tenant_id for this profile
    let tenant_id: Uuid = sqlx::query_scalar(
        r#"
        SELECT u.tenant_id
        FROM business_profiles bp
        JOIN users u ON bp.user_id = u.id
        WHERE bp.id = $1
        "#
    )
    .bind(r.business_profile_id)
    .fetch_one(&s.db)
    .await
    .map_err(|_| AppError::NotFound("Business profile not found".to_string()))?;

    // Record the milestone in event_logs
    let event_id = Uuid::new_v4();
    let _ = sqlx::query(
        r#"INSERT INTO event_logs (id, business_profile_id, event_name, metadata, created_at)
           VALUES ($1, $2, $3, $4, NOW())"#
    )
    .bind(event_id)
    .bind(r.business_profile_id)
    .bind(format!("milestone.{}", r.milestone_type))
    .bind(json!({
        "milestone_type": r.milestone_type,
        "metadata": r.metadata.unwrap_or(json!({}))
    }))
    .execute(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to record milestone: {}", e)))?;

    // Record a health signal (positive)
    let user_id = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM business_profiles WHERE id = $1"
    )
    .bind(r.business_profile_id)
    .fetch_one(&s.db)
    .await?;

    crate::monitoring::engine::record_signal(
        &s.db,
        tenant_id,
        "contact",
        user_id,
        "feature_used",
        10, // big positive signal for reaching a milestone
    ).await;

    // If first_automation, trigger congratulatory followup
    if r.milestone_type == "first_automation" {
        let _ = sqlx::query(
            r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
               VALUES ($1, $2, 'timeout', '{}'::jsonb, 'send_email',
                       $3::jsonb,
                       NOW() + INTERVAL '1 hour')"#
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(json!({
            "to_entity": "contact",
            "entity_id": user_id,
            "template_type": "milestone_congratulations",
            "stage_title": "Congratulations on your first automation!",
            "milestone": "first_automation"
        }))
        .execute(&s.db)
        .await;

        tracing::info!(
            profile = %r.business_profile_id,
            milestone = %r.milestone_type,
            "First automation milestone — congratulations followup scheduled"
        );
    }

    Ok((StatusCode::CREATED, Json(json!({
        "message": "Milestone recorded",
        "event_id": event_id,
        "milestone_type": r.milestone_type,
        "business_profile_id": r.business_profile_id
    }))))
}

// ---------------------------------------------------------------------------
// GET /api/account-health/status/:profile_id
// ---------------------------------------------------------------------------

/// Get the full health status for a business profile.
/// Returns:
///   - The account_health record (if any)
///   - Recent event_logs entries
///   - Days remaining in trial (calculated from tenant_plans)
pub async fn profile_health_status(
    State(s): State<AppState>,
    Path(profile_id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    // Verify profile exists
    let profile = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT id, user_id FROM business_profiles WHERE id = $1"
    )
    .bind(profile_id)
    .fetch_optional(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Database error: {}", e)))?
    .ok_or_else(|| AppError::NotFound("Business profile not found".to_string()))?;

    let (_pid, user_id) = profile;

    // Get tenant_id from user
    let tenant_id = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT tenant_id FROM users WHERE id = $1"
    )
    .bind(user_id)
    .fetch_optional(&s.db)
    .await?
    .flatten()
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    // Fetch account health record
    let health = sqlx::query_as::<_, AccountHealth>(
        r#"SELECT * FROM account_health
           WHERE tenant_id = $1 AND entity_type = 'contact' AND entity_id = $2
           LIMIT 1"#
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(&s.db)
    .await?;

    // Fetch recent event_logs for this profile (last 20)
    let recent_events: Vec<serde_json::Value> = sqlx::query_as::<_, (String, serde_json::Value, chrono::DateTime<Utc>)>(
        r#"SELECT event_name, COALESCE(metadata, '{}'::jsonb), created_at
           FROM event_logs
           WHERE business_profile_id = $1
           ORDER BY created_at DESC
           LIMIT 20"#
    )
    .bind(profile_id)
    .fetch_all(&s.db)
    .await
    .map_err(|e| AppError::Internal(format!("Failed to fetch events: {}", e)))?
    .into_iter()
    .map(|(name, meta, ts)| {
        json!({
            "event_name": name,
            "metadata": meta,
            "created_at": ts.to_rfc3339()
        })
    })
    .collect();

    // Calculate trial days remaining from tenant_plans
    let trial_info = sqlx::query_as::<_, (Option<chrono::DateTime<Utc>>,)>( // tuple of one for proper pattern match
        r#"SELECT trial_ends_at
           FROM tenant_plans
           WHERE tenant_id = $1 AND status = 'trialing'
           LIMIT 1"#
    )
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await?;

    let (days_remaining, trial_expired) = match trial_info {
        Some((Some(ends_at),)) => {
            let remaining = (ends_at - Utc::now()).num_days().max(0);
            let expired = Utc::now() > ends_at;
            (Some(remaining), expired)
        }
        _ => (None, false),
    };

    let status = ProfileHealthStatus {
        profile_id,
        health,
        recent_events,
        days_remaining_in_trial: days_remaining,
        trial_expired,
    };

    Ok((StatusCode::OK, Json(json!(status))))
}
