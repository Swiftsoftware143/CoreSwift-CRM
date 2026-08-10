//! Account health evaluation engine.
//!
//! Scores entities on a 0–100 scale based on signals.
//! Triggers interventions when thresholds are crossed.

use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Record a health signal and recalculate risk level.
/// Called automatically by the event bus on login, feature_used, days_inactive, etc.
pub async fn record_signal(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    signal: &str,
    value: i32,
) {
    // Upsert the account health row
    let existing = sqlx::query_as::<_, (i32, String, Option<chrono::DateTime<Utc>>)>(
        "SELECT score, risk_level, last_active_at FROM account_health WHERE tenant_id = $1 AND entity_type = $2 AND entity_id = $3"
    ).bind(tenant_id).bind(entity_type).bind(entity_id).fetch_optional(db).await;

    let (score, _risk, _last_active) = match existing {
        Ok(Some(s)) => s,
        _ => {
            let _ = sqlx::query(
                "INSERT INTO account_health (id, tenant_id, entity_type, entity_id, score, last_active_at) VALUES ($1, $2, $3, $4, 100, NOW())"
            ).bind(Uuid::new_v4()).bind(tenant_id).bind(entity_type).bind(entity_id).execute(db).await;
            (100, "healthy".to_string(), Some(Utc::now()))
        }
    };

    // Calculate new score
    let new_score = match signal {
        "login" | "feature_used" | "api_call" | "payment" => (score + 5).min(100),
        "days_inactive" => (score - (value * 10)).max(0),
        "failed_action" | "error" => (score - 15).max(0),
        "support_ticket" | "complaint" => (score - 10).max(0),
        _ => score,
    };

    let risk = if new_score >= 80 {
        "healthy"
    } else if new_score >= 40 {
        "at_risk"
    } else if new_score > 0 {
        "critical"
    } else {
        "churned"
    };

    let is_activity = signal == "login" || signal == "feature_used" || signal == "api_call";

    let _ = sqlx::query(
        r#"UPDATE account_health SET
            score = $1,
            risk_level = $2,
            last_active_at = CASE WHEN $3 THEN NOW() ELSE last_active_at END,
            signals = COALESCE(signals, '[]'::jsonb) || $4::jsonb,
            updated_at = NOW()
           WHERE tenant_id = $5 AND entity_type = $6 AND entity_id = $7"#,
    )
    .bind(new_score)
    .bind(risk)
    .bind(is_activity)
    .bind(json!([{"signal": signal, "value": value, "ts": Utc::now().to_rfc3339()}]))
    .bind(tenant_id)
    .bind(entity_type)
    .bind(entity_id)
    .execute(db)
    .await
    .ok();

    // Check if intervention needed
    if risk == "critical" || risk == "at_risk" {
        check_interventions(db, tenant_id, entity_type, entity_id, risk).await;
    }
}

/// Check thresholds and trigger interventions for at-risk accounts.
async fn check_interventions(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    risk_level: &str,
) {
    let thresholds = sqlx::query_as::<_, (String, serde_json::Value)>(
        "SELECT intervention_action, intervention_config FROM health_thresholds WHERE tenant_id = $1 AND entity_type = $2 AND risk_level = $3 AND is_active = true LIMIT 1"
    ).bind(tenant_id).bind(entity_type).bind(risk_level).fetch_optional(db).await;

    if let Ok(Some((action, config))) = thresholds {
        tracing::info!(entity=%entity_id, risk=%risk_level, action=%action, "Account health intervention triggered");

        // Schedule intervention via delayed actions
        let _ = sqlx::query(
            r#"INSERT INTO delayed_actions (id, tenant_id, condition_type, condition_config, action_type, action_config, execute_at)
               VALUES ($1, $2, 'timeout', '{}', $3, $4, NOW() + INTERVAL '5 minutes')"#
        )
        .bind(Uuid::new_v4()).bind(tenant_id).bind(&action).bind(&config)
        .execute(db).await;
    }
}
