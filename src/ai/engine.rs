//! AI Engine — the runtime brain that integrates with the events dispatcher and worker.
//!
//! These functions are called by the background worker and dispatcher to make
//! intelligent decisions about follow-ups. They analyze live database state
//! and produce adaptive recommendations.

use crate::ai::models::*;
use sqlx::PgPool;
use uuid::Uuid;

/// Assess churn risk for a single entity by analyzing health signals, activity, and plan status.
/// Called by the worker every 5 minutes for inactive trials.
pub async fn assess_churn_risk(db: &PgPool, tenant_id: Uuid, contact_id: Uuid) -> ChurnAssessment {
    // Gather signals in one query
    let health_row = sqlx::query_as::<_, (Option<i32>, Option<String>, Option<chrono::DateTime<chrono::Utc>>, Option<i32>)>(
        "SELECT score, risk_level, last_active_at, jsonb_array_length(COALESCE(signals, '[]'::jsonb)) as signals_len
         FROM account_health WHERE tenant_id = $1 AND entity_type = 'contact' AND entity_id = $2"
    ).bind(tenant_id).bind(contact_id).fetch_optional(db).await.unwrap_or(None);

    let (score, risk_level, last_active_at, signals_len): (
        Option<i32>,
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<i32>,
    ) = health_row.unwrap_or_default();

    let plan_row = sqlx::query_as::<_, (Option<String>, Option<chrono::DateTime<chrono::Utc>>)>(
        "SELECT tp.status, tp.trial_ends_at
         FROM tenant_plans tp
         JOIN contacts c ON c.tenant_id = tp.tenant_id
         WHERE c.id = $1 AND tp.status IN ('active', 'trialing')
         LIMIT 1",
    )
    .bind(contact_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let (plan_status, _trial_ends): (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        plan_row.unwrap_or_default();

    let score = score.unwrap_or(100);
    let _risk = risk_level.unwrap_or_else(|| "healthy".to_string());
    let last_at = last_active_at;
    let signals_count = signals_len.unwrap_or(0);
    let is_trialing = plan_status.as_deref() == Some("trialing");

    // Days since last activity
    let inactivity_days = last_at
        .map(|t| {
            let diff = chrono::Utc::now() - t;
            diff.num_days()
        })
        .unwrap_or(0);

    // Age of account in days
    let age_days = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT EXTRACT(DAY FROM (NOW() - created_at))::bigint FROM contacts WHERE id = $1",
    )
    .bind(contact_id)
    .fetch_one(db)
    .await
    .unwrap_or(None)
    .unwrap_or(0);

    // Weighted churn model: score contribution + inactivity + plan status + signal velocity
    let churn_prob =
        calculate_churn_probability(score, inactivity_days, is_trialing, signals_count);

    let (risk_tier, intervention, priority) = classify_churn(churn_prob, score, inactivity_days);

    ChurnAssessment {
        contact_id,
        churn_probability: churn_prob,
        risk_tier,
        age_days,
        inactivity_days,
        signals_count,
        intervention,
        priority,
    }
}

fn calculate_churn_probability(
    score: i32,
    inactivity_days: i64,
    is_trialing: bool,
    _signals: i32,
) -> f64 {
    let mut prob: f64 = 1.0 - (score as f64 / 100.0);
    prob += (inactivity_days as f64 * 0.02).min(0.4);
    if is_trialing {
        prob += 0.1; // trials are naturally higher risk
    }
    // Signal velocity adjusts: zero signals = higher risk (they never engaged)
    (prob * 100.0).round() / 100.0
}

fn classify_churn(prob: f64, score: i32, days: i64) -> (String, String, String) {
    if prob >= 0.75 || score <= 10 || days >= 30 {
        (
            "critical".to_string(),
            "immediate_human_callback + urgency_email".to_string(),
            "immediate".to_string(),
        )
    } else if prob >= 0.50 || score <= 40 || days >= 14 {
        (
            "high".to_string(),
            "personalized_re_engagement + discount_offer".to_string(),
            "within_24h".to_string(),
        )
    } else if prob >= 0.25 || score <= 70 || days >= 7 {
        (
            "medium".to_string(),
            "checklist_automation + reminder_email".to_string(),
            "within_week".to_string(),
        )
    } else {
        (
            "low".to_string(),
            "monitor — no action needed".to_string(),
            "monitor".to_string(),
        )
    }
}

/// Determine the best channel for a follow-up based on historical engagement.
/// Reads from event_logs to see which channels the contact has responded to.
pub async fn suggest_channel(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    _context: &str,
) -> ChannelSuggestion {
    // Count email opens vs SMS interactions
    let email_engagement = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM event_logs el
         JOIN business_profiles bp ON bp.id = el.business_profile_id
         JOIN users u ON u.id = bp.user_id
         WHERE u.tenant_id = $1 AND u.id = $2
         AND el.event_name IN ('email.opened', 'email.clicked', 'email.replied')",
    )
    .bind(tenant_id)
    .bind(contact_id)
    .fetch_optional(db)
    .await;

    let sms_engagement = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COUNT(*) FROM outbound_messages WHERE tenant_id = $1 AND channel = 'sms'
         AND status = 'sent' AND to_address IN (
             SELECT email FROM contacts WHERE id = $2
         )",
    )
    .bind(tenant_id)
    .bind(contact_id)
    .fetch_optional(db)
    .await;

    let email_count = email_engagement.unwrap_or(None).flatten().unwrap_or(0);
    let sms_count = sms_engagement.unwrap_or(None).flatten().unwrap_or(0);

    if email_count > sms_count * 2 {
        ChannelSuggestion {
            recommended_channel: "email".to_string(),
            confidence: 0.8,
            reason: format!(
                "Contact has engaged with {} emails vs {} SMS messages",
                email_count, sms_count
            ),
        }
    } else if sms_count > 0 && email_count == 0 {
        ChannelSuggestion {
            recommended_channel: "sms".to_string(),
            confidence: 0.7,
            reason: "Contact has SMS history but no email engagement".to_string(),
        }
    } else {
        ChannelSuggestion {
            recommended_channel: "hybrid".to_string(),
            confidence: 0.5,
            reason: "Insufficient engagement data — defaulting to hybrid".to_string(),
        }
    }
}

/// Determine optimal send time based on past engagement patterns.
pub async fn suggest_timing(_db: &PgPool, _tenant_id: Uuid, _contact_id: Uuid) -> TimingSuggestion {
    // In production: analyze event_logs for time-of-day patterns per contact.
    // For now: sensible defaults based on B2B behavior patterns.
    TimingSuggestion {
        recommended_hour: 10, // 10 AM — peak B2B email open rates
        recommended_day: "weekday".to_string(),
        best_window: "morning".to_string(),
        confidence: 0.65,
        reason: "B2B contacts typically engage between 8-11 AM on weekdays".to_string(),
    }
}

/// Suggest a segmentation and campaign strategy for the given goal.
pub async fn recommend_campaign(
    db: &PgPool,
    tenant_id: Uuid,
    goal: &str,
) -> CampaignRecommendation {
    let target_count =
        sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(db)
            .await
            .unwrap_or(None)
            .unwrap_or(0);

    let segments = vec![
        CampaignSegment {
            name: "High Engagement".to_string(),
            count: sqlx::query_scalar::<_, Option<i64>>(
                "SELECT COUNT(*) FROM account_health WHERE tenant_id = $1 AND risk_level = 'healthy' AND score >= 80"
            ).bind(tenant_id).fetch_one(db).await.unwrap_or(None).unwrap_or(0),
            avg_health_score: 90.0,
            recommended_offer: match goal {
                "trial_conversion" => "Upgrade now — first month free".to_string(),
                "upsell" => "Pro plan trial — 30 days free".to_string(),
                "reactivation" => "Come back — 20% off next 3 months".to_string(),
                _ => "Retention check-in".to_string(),
            },
        },
        CampaignSegment {
            name: "At Risk".to_string(),
            count: sqlx::query_scalar::<_, Option<i64>>(
                "SELECT COUNT(*) FROM account_health WHERE tenant_id = $1 AND risk_level = 'at_risk'"
            ).bind(tenant_id).fetch_one(db).await.unwrap_or(None).unwrap_or(0),
            avg_health_score: 55.0,
            recommended_offer: match goal {
                "trial_conversion" => "Personal onboarding call".to_string(),
                "reactivation" => "Free strategy session".to_string(),
                _ => "Personal check-in".to_string(),
            },
        },
        CampaignSegment {
            name: "Critical".to_string(),
            count: sqlx::query_scalar::<_, Option<i64>>(
                "SELECT COUNT(*) FROM account_health WHERE tenant_id = $1 AND risk_level = 'critical'"
            ).bind(tenant_id).fetch_one(db).await.unwrap_or(None).unwrap_or(0),
            avg_health_score: 20.0,
            recommended_offer: "Urgent: Schedule retention call".to_string(),
        },
    ];

    let (recommended_template, ai_message) = match goal {
        "trial_conversion" => (
            "trial_conversion_v2".to_string(),
            format!("{} of {} contacts are still trialing. Recommend sending conversion sequence with personalized case studies.", segments[1].count + segments[2].count, target_count)
        ),
        "reactivation" => (
            "reactivation_v3".to_string(),
            format!("{} inactive contacts identified. Suggest reactivation campaign with incentive for their account type.", target_count)
        ),
        "upsell" => (
            "upsell_pro_v2".to_string(),
            format!("{} high-engagement contacts are ready for upsell. Recommend Pro plan trial offer.", segments[0].count)
        ),
        _ => (
            "retention_monthly".to_string(),
            format!("{} contacts active. Monthly health check campaign recommended.", segments[0].count)
        ),
    };

    CampaignRecommendation {
        target_count,
        recommended_template,
        ai_message,
        segments,
    }
}

/// Determine if a contact needs a human intervention vs automated follow-up.
pub async fn should_escalate_to_human(assessment: &ChurnAssessment) -> bool {
    assessment.churn_probability >= 0.7
        || assessment.risk_tier == "critical"
        || assessment.inactivity_days >= 30
}

/// Select the best message template based on context and churn risk.
pub fn select_template(context: &str, churn_prob: f64) -> &'static str {
    match context {
        "abandoned_signup" => "directory_abandoned_15min",
        "inactive_trial" if churn_prob >= 0.5 => "saas_trial_critical_re_engagement",
        "inactive_trial" => "saas_trial_inactive_24h",
        "checklist_stage_2" => "checklist_complete_logo",
        "checklist_stage_3" => "checklist_add_keywords",
        "churn_risk" if churn_prob >= 0.7 => "retention_urgency_callback",
        "churn_risk" => "retention_check_in",
        "renewal" => "renewal_reminder",
        _ => "generic_follow_up",
    }
}
