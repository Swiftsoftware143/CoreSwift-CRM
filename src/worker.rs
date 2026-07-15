//! Cron-based background worker for the Flawless Follow-up system.
//!
//! Runs every 60 seconds evaluating:
//! 1. Delayed "If-Not-Then" actions that are due
//! 2. Inactive trial accounts that need health penalties
//! 3. Follow-up queue items ready to execute
//!
//! This replaces n8n / Node.js cron — all baked into the single Rust binary.

use sqlx::PgPool;
use uuid::Uuid;
use tokio_cron_scheduler::{Job, JobScheduler};

// Import AI engine for intelligent decision-making
use crate::ai::engine;
use crate::communications::providers;

/// Start the background worker scheduler.
/// Call this once during server startup.
pub async fn start_worker(db: PgPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let sched = JobScheduler::new().await?;

    // Job 1: Every 60 seconds — evaluate pending delayed actions
    let db1 = db.clone();
    let job1 = Job::new_async("0/60 * * * * *", move |_uuid, _lock| {
        let db = db1.clone();
        Box::pin(async move {
            evaluate_pending_delayed_actions(&db).await;
        })
    })?;
    sched.add(job1).await?;

    // Job 2: Every 5 minutes — check for inactive trials and mark health
    let db2 = db.clone();
    let job2 = Job::new_async("0 */5 * * * *", move |_uuid, _lock| {
        let db = db2.clone();
        Box::pin(async move {
            check_inactive_trials(&db).await;
        })
    })?;
    sched.add(job2).await?;

    // Job 3: Every hour — run health score recalculation for all active tenants
    let db3 = db.clone();
    let job3 = Job::new_async("0 0 * * * *", move |_uuid, _lock| {
        let db = db3.clone();
        Box::pin(async move {
            recalculate_health_scores(&db).await;
        })
    })?;
    sched.add(job3).await?;

    // Job 4: Every 5 minutes — check for abandoned directory sign-ups (Case A)
    let db4 = db.clone();
    let job4 = Job::new_async("0 */5 * * * *", move |_uuid, _lock| {
        let db = db4.clone();
        Box::pin(async move {
            check_abandoned_directory_signups(&db).await;
        })
    })?;
    sched.add(job4).await?;

    // Job 5: Every 30 seconds — process queued outbound messages (welcome emails etc.)
    let db5 = db.clone();
    let job5 = Job::new_async("0/30 * * * * *", move |_uuid, _lock| {
        let db = db5.clone();
        Box::pin(async move {
            deliver_queued_messages(&db).await;
        })
    })?;
    sched.add(job5).await?;

    sched.start().await?;
    tracing::info!("Flawless Follow-up background worker started (5 cron jobs)");

    Ok(())
}


/// Evaluate all pending delayed actions that are past their execute_at time.
/// This is the core "If-Not-Then" engine — checks conditions and fires actions.
async fn evaluate_pending_delayed_actions(db: &PgPool) {
    let pending = match sqlx::query_as::<_, (Uuid,)>(
        "SELECT id FROM delayed_actions WHERE executed = false AND cancelled = false AND execute_at <= NOW() ORDER BY execute_at ASC LIMIT 100"
    ).fetch_all(db).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch pending delayed actions");
            return;
        }
    };

    if pending.is_empty() {
        return;
    }

    tracing::info!("Evaluating {} pending delayed actions", pending.len());

    for (id,) in &pending {
        crate::events::dispatcher::evaluate_delayed_action(db, *id).await;
    }
}

/// Check for inactive trial accounts and record health signals.
/// Mirrors Case A/C from Steve Rosenberg's schema:
///   SELECT ... WHERE current_state = 'active' AND subscription_active = FALSE
///     AND last_activity_at < NOW() - INTERVAL '24 hours'
///
/// Queries business_profiles table directly (your exact schema query).
async fn check_inactive_trials(db: &PgPool) {
    // Query 1: Native CRM Swift tables (already existing)
    let native = match sqlx::query_as::<_, (Uuid, Uuid)>(
        r#"SELECT tp.tenant_id, c.id as contact_id
           FROM tenant_plans tp
           JOIN contacts c ON c.tenant_id = tp.tenant_id
           LEFT JOIN account_health ah ON ah.entity_id = c.id AND ah.entity_type = 'contact'
           WHERE tp.status = 'trialing'
             AND (ah.last_active_at IS NULL OR ah.last_active_at < NOW() - INTERVAL '24 hours')
             AND (ah.risk_level IS NULL OR ah.risk_level = 'healthy' OR ah.risk_level = 'at_risk')
           LIMIT 50"#
    ).fetch_all(db).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check inactive trials");
            return;
        }
    };

    // Query 2: business_profiles table (your exact schema query)
    let profiles = match sqlx::query_as::<_, (Uuid, Uuid, String, Option<String>, String)>(
        r#"SELECT bp.id, u.id as user_id, u.email, u.phone, bp.business_name
           FROM business_profiles bp
           JOIN users u ON bp.user_id = u.id
           WHERE bp.unit = 'saas'
             AND bp.current_state = 'active'
             AND bp.subscription_active = FALSE
             AND bp.last_activity_at < NOW() - INTERVAL '24 hours'
           LIMIT 50"#
    ).fetch_all(db).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check inactive trials via business_profiles");
            vec![]
        }
    };

    // Process native records
    for (tenant_id, contact_id) in &native {
        tracing::info!(tenant = %tenant_id, contact = %contact_id, "Inactive trial detected (native)");
        crate::monitoring::engine::record_signal(
            db,
            *tenant_id,
            "contact",
            *contact_id,
            "days_inactive",
            1,
        ).await;
    }

    // Process business_profiles records — fetch tenant_id from users table
    for (_bp_id, user_id, email, _phone, business_name) in &profiles {
        if let Some(tenant_id) = sqlx::query_scalar::<_, Option<Uuid>>(
            "SELECT tenant_id FROM users WHERE id = $1"
        ).bind(user_id).fetch_one(db).await.unwrap_or(None) {
            tracing::info!(user = %user_id, business = %business_name, "Inactive trial detected (business_profiles)");

            // AI-powered churn assessment
            let assessment = engine::assess_churn_risk(db, tenant_id, *user_id).await;

            // Record health signal
            crate::monitoring::engine::record_signal(
                db,
                tenant_id,
                "contact",
                *user_id,
                "days_inactive",
                1,
            ).await;

            // AI-selected template based on churn risk
            let template_slug = engine::select_template("inactive_trial", assessment.churn_probability);

            // AI-suggested channel
            let channel_suggestion = engine::suggest_channel(db, tenant_id, *user_id, "inactive_trial").await;
            let channel = &channel_suggestion.recommended_channel;

            tracing::info!(user = %user_id, churn = %assessment.churn_probability, template = %template_slug, channel = %channel, "AI-selected follow-up strategy");

            // Queue a follow-up in followup_queue (your schema) with AI-selected template
            let _ = sqlx::query(
                r#"INSERT INTO followup_queue (id, business_profile_id, scheduled_for, channel, template_slug)
                   VALUES ($1, $2, NOW(), $3, $4)"#
            )
            .bind(Uuid::new_v4()).bind(user_id).bind(channel).bind(template_slug)
            .execute(db).await;

            // If critical risk, escalate to human intervention
            if engine::should_escalate_to_human(&assessment).await {
                tracing::warn!(user = %user_id, churn = %assessment.churn_probability, "AI escalation: contact needs human callback");
                let _ = sqlx::query(
                    r#"INSERT INTO notifications (id, tenant_id, user_id, message)
                       VALUES ($1, $2, $3, $4)"#
                )
                .bind(Uuid::new_v4()).bind(tenant_id).bind(user_id)
                .bind(format!("CRITICAL: {} at {:.0}% churn risk. Business: {}. Intervention: {}",
                    email, assessment.churn_probability * 100.0, business_name, assessment.intervention))
                .execute(db).await;
            }

            // Also queue via delayed_actions (native) with AI-selected template
            let _ = sqlx::query(
                r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
                   VALUES ($1, $2, 'timeout', '{}'::jsonb, 'send_email',
                           $3::jsonb || jsonb_build_object('template_slug', $4),
                           NOW())"#
            )
            .bind(Uuid::new_v4()).bind(tenant_id)
            .bind(serde_json::json!({
                "to_entity": "contact",
                "entity_id": user_id,
                "template_type": "ai_recommended",
                "stage_title": "Reactivate Your Trial",
                "business_name": business_name,
                "email": email,
                "churn_probability": assessment.churn_probability
            }))
            .bind(template_slug)
            .execute(db).await;
        }
    }
}

/// Recalculate health scores based on last_activity for all active entities.
async fn recalculate_health_scores(db: &PgPool) {
    // Find contacts who haven't been active in 7 days across active tenants
    let old = match sqlx::query_as::<_, (Uuid, Uuid, i32)>(
        r#"SELECT ah.tenant_id, ah.entity_id,
                  EXTRACT(DAY FROM (NOW() - ah.last_active_at))::int as inactive_days
           FROM account_health ah
           JOIN tenant_plans tp ON tp.tenant_id = ah.tenant_id
           WHERE tp.status IN ('active', 'trialing')
             AND ah.risk_level != 'churned'
             AND ah.last_active_at < NOW() - INTERVAL '7 days'
           LIMIT 100"#
    ).fetch_all(db).await {
        Ok(o) => o,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to scan old health records");
            return;
        }
    };

    for (tid, eid, days) in &old {
        crate::monitoring::engine::record_signal(db, *tid, "contact", *eid, "days_inactive", *days).await;
    }

    if !old.is_empty() {
        tracing::info!("Health recalculation complete: {} accounts penalized", old.len());
    }
}

/// Case A: Directory Abandoned Sign-up (15-Minute Window)
/// A user paid for a directory listing but walked away before completing their profile.
/// Looks for profiles stuck in pending_onboarding with no new profile updates in 15 minutes.
/// Dedup guard: skip if we already queued a follow-up in the last hour.
/// Matches the exact query:
///   SELECT bp.id, u.email, u.phone, u.first_name, bp.business_name
///   FROM business_profiles bp
///   JOIN users u ON bp.user_id = u.id
///   WHERE bp.unit = 'directory'
///    AND bp.current_state = 'pending_onboarding'
///    AND bp.last_activity_at < NOW() - INTERVAL '15 minutes'
///    AND NOT EXISTS (
///      SELECT 1 FROM followup_queue fq
///      WHERE fq.business_profile_id = bp.id
///      AND fq.created_at > NOW() - INTERVAL '1 hour'
///    );
async fn check_abandoned_directory_signups(db: &PgPool) {
    let abandoned = match sqlx::query_as::<_, (Uuid, Uuid, Uuid, String, Option<String>, Option<String>, String)>(
        r#"SELECT bp.id, u.id as user_id, u.tenant_id, u.email, u.phone, u.first_name, bp.business_name
           FROM business_profiles bp
           JOIN users u ON bp.user_id = u.id
           WHERE bp.unit = 'directory'
             AND bp.current_state = 'pending_onboarding'
             AND bp.last_activity_at < NOW() - INTERVAL '15 minutes'
             AND NOT EXISTS (
               SELECT 1 FROM followup_queue fq
               WHERE fq.business_profile_id = bp.id
               AND fq.created_at > NOW() - INTERVAL '1 hour'
             )
           LIMIT 50"#
    ).fetch_all(db).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check abandoned directory signups");
            return;
        }
    };

    if abandoned.is_empty() {
        return;
    }

    tracing::info!(count = %abandoned.len(), "Directory abandoned sign-ups detected");

    for (bp_id, _user_id, tenant_id, email, phone, first_name, business_name) in &abandoned {
        // AI-suggested channel based on their engagement history (if any)
        let channel_suggestion = engine::suggest_channel(db, *tenant_id, *_user_id, "abandoned_signup").await;
        let channel = &channel_suggestion.recommended_channel;
        let template_slug = "directory_abandoned_15min";

        tracing::info!(business = %business_name, channel = %channel, "AI-recommended channel for abandoned signup");

        // Queue a follow-up action with AI-recommended channel
        let _ = sqlx::query(
            r#"INSERT INTO followup_queue (id, business_profile_id, scheduled_for, channel, template_slug)
               VALUES ($1, $2, NOW(), $3, $4)"#
        )
        .bind(Uuid::new_v4()).bind(bp_id).bind(channel).bind(template_slug)
        .execute(db).await;

        // Also insert into delayed_actions (native) with AI-recommended channel
        let _ = sqlx::query(
            r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
               VALUES ($1, $2, 'timeout', '{}'::jsonb, 'send_email',
                       $3::jsonb || jsonb_build_object('template_slug', $4),
                       NOW())"#
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(serde_json::json!({
            "to_email": email,
            "to_phone": phone,
            "first_name": first_name,
            "business_name": business_name,
            "template_type": "checklist",
            "stage_title": "Complete Your Directory Profile",
            "ai_recommended_channel": channel
        }))
        .bind(template_slug)
        .execute(db).await;
    }
}

/// Process queued email messages from outbound_messages table.
/// Polls for 'queued' messages with channel='email', sends via configured provider.
async fn deliver_queued_messages(db: &PgPool) {
    let messages = match sqlx::query_as::<_, (Uuid, Uuid, String, String, Option<String>, String)>(
        r#"SELECT id, tenant_id, to_address, body, subject, channel
           FROM outbound_messages
           WHERE status = 'queued' AND channel = 'email'
           ORDER BY created_at ASC
           LIMIT 10"#
    ).fetch_all(db).await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to fetch queued messages");
            return;
        }
    };

    if messages.is_empty() {
        return;
    }

    tracing::info!("Processing {} queued messages", messages.len());

    for (msg_id, tenant_id, to_address, body, subject, _channel) in &messages {
        // Mark as sending
        let _ = sqlx::query(
            "UPDATE outbound_messages SET status = 'sending' WHERE id = $1"
        ).bind(msg_id).execute(db).await;

        // Load delivery config from tenant settings
        let cfg = crate::communications::providers::load_delivery_config(
            db, *msg_id, *tenant_id, "email", to_address,
            subject.clone(), body,
        ).await;

        // Deliver
        let (ok, err) = crate::communications::providers::deliver(&cfg).await;

        if ok {
            let _ = sqlx::query(
                "UPDATE outbound_messages SET status = 'sent', sent_at = NOW() WHERE id = $1"
            ).bind(msg_id).execute(db).await;
            tracing::info!(msg = %msg_id, "Email delivered successfully");
        } else {
            let err_msg = err.unwrap_or_else(|| "Unknown error".to_string());
            let _ = sqlx::query(
                "UPDATE outbound_messages SET status = 'failed', error_message = $2 WHERE id = $1"
            ).bind(msg_id).bind(&err_msg).execute(db).await;
            tracing::warn!(msg = %msg_id, error = %err_msg, "Email delivery failed");
        }
    }
}
