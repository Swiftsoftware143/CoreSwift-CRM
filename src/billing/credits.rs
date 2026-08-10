//! Credit consumption engine — tracks every billable action and deducts credits.
//!
//! Every automated action, AI call, and communication burns credits.
//! Tenants are checked against their plan's allowance before execution.
//!
//! Credit costs (per action):
//!
//! ┌──────────────────────────────────┬────────┬──────────────┐
//! │ Action                           │ Credits│ Scope        │
//! ├──────────────────────────────────┼────────┼──────────────┤
//! │ Contact import / create          │      1 │ per contact  │
//! │ Automation rule execution        │      2 │ per trigger  │
//! │ Email sent (automated)           │      3 │ per message  │
//! │ SMS sent (automated)             │      5 │ per message  │
//! │ Webhook callout                  │      1 │ per request  │
//! │ Checklist stage transition       │      2 │ per stage    │
//! │ Health evaluation / signal       │      1 │ per signal   │
//! │ AI churn assessment              │      5 │ per assess   │
//! │ AI message composition           │     10 │ per message  │
//! │ AI lead prioritization           │      3 │ per batch    │
//! │ AI win prediction                │      5 │ per predict  │
//! │ Campaign recommendation          │      5 │ per request  │
//! │ API call (integration)           │      1 │ per call     │
//! │ Prepopulation scan               │      3 │ per scan     │
//! ├──────────────────────────────────┼────────┼──────────────┤
//! │ Free plan allowance              │    200 │ per month    │
//! │ Starter plan allowance           │  2,000 │ per month    │
//! │ Professional plan allowance      │ 10,000 │ per month    │
//! │ Enterprise plan allowance        │ 50,000 │ per month    │
//! └──────────────────────────────────┴────────┴──────────────┘

use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

/// Map of action types to credit cost
fn credit_costs() -> HashMap<&'static str, i32> {
    let mut m = HashMap::new();
    m.insert("contact.created", 1);
    m.insert("automation.executed", 2);
    m.insert("email.sent", 3);
    m.insert("sms.sent", 5);
    m.insert("webhook.called", 1);
    m.insert("checklist.stage", 2);
    m.insert("health.signal", 1);
    m.insert("ai.churn_assessment", 5);
    m.insert("ai.compose_message", 10);
    m.insert("ai.prioritize", 3);
    m.insert("ai.predict", 5);
    m.insert("ai.campaign", 5);
    m.insert("integration.api_call", 1);
    m.insert("prepopulation.scan", 3);
    m
}

/// Check if a tenant has enough credits for an action.
/// Returns (has_credits, remaining, cost).
pub async fn check_credits(
    db: &PgPool,
    tenant_id: Uuid,
    action_type: &str,
) -> Result<(bool, i32, i32), String> {
    let cost = credit_costs().get(action_type).copied().unwrap_or(1);
    let remaining = get_credits_remaining(db, tenant_id).await;
    Ok((remaining >= cost, remaining, cost))
}

/// Get current monthly credit allowance and remaining for a tenant.
pub async fn get_credits_remaining(db: &PgPool, tenant_id: Uuid) -> i32 {
    // Get plan's monthly credit allowance
    let allowance = sqlx::query_scalar::<_, Option<i32>>(
        "SELECT COALESCE(p.monthly_credits, 0) FROM plans p
         JOIN tenant_plans tp ON tp.plan_id = p.id
         WHERE tp.tenant_id = $1 AND tp.status IN ('active', 'trialing')
         LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_one(db)
    .await
    .unwrap_or(None)
    .unwrap_or(0);

    if allowance == 0 {
        return 0; // No plan or no credits
    }

    // Get consumed credits this period
    let consumed = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT COALESCE(SUM(ABS(credits)), 0) FROM credit_transactions
         WHERE tenant_id = $1 AND credits < 0
           AND created_at >= (SELECT current_period_starts_at FROM tenant_plans WHERE tenant_id = $1 AND status IN ('active', 'trialing') LIMIT 1)"
    ).bind(tenant_id).fetch_one(db).await.unwrap_or(None).unwrap_or(0);

    (allowance - consumed as i32).max(0)
}

/// Consume credits for a specific action. Logs the transaction.
/// Returns remaining credits after consumption.
pub async fn consume_credits(
    db: &PgPool,
    tenant_id: Uuid,
    action_type: &str,
    description: &str,
    entity_type: Option<&str>,
    entity_id: Option<Uuid>,
) -> i32 {
    let cost = credit_costs().get(action_type).copied().unwrap_or(1);
    let remaining = get_credits_remaining(db, tenant_id).await;

    if remaining < cost {
        tracing::warn!(tenant = %tenant_id, action = %action_type, remaining = %remaining, cost = %cost, "Insufficient credits");
        return remaining;
    }

    let _ = sqlx::query(
        r#"INSERT INTO credit_transactions (id, tenant_id, action_type, credits, description, entity_type, entity_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7)"#
    )
    .bind(Uuid::new_v4()).bind(tenant_id).bind(action_type)
    .bind(-cost).bind(description).bind(entity_type).bind(entity_id)
    .execute(db).await;

    let new_remaining = remaining - cost;
    tracing::info!(tenant = %tenant_id, action = %action_type, cost = %cost, remaining = %new_remaining, "Credits consumed");
    new_remaining
}

/// Get credit usage summary for the current billing period.
pub async fn get_credit_summary(db: &PgPool, tenant_id: Uuid) -> serde_json::Value {
    let remaining = get_credits_remaining(db, tenant_id).await;

    let (allowance, period_start, period_end) = sqlx::query_as::<
        _,
        (
            Option<i32>,
            Option<chrono::DateTime<chrono::Utc>>,
            Option<chrono::DateTime<chrono::Utc>>,
        ),
    >(
        "SELECT p.monthly_credits, tp.current_period_starts_at, tp.current_period_ends_at
         FROM plans p JOIN tenant_plans tp ON tp.plan_id = p.id
         WHERE tp.tenant_id = $1 AND tp.status IN ('active', 'trialing') LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_one(db)
    .await
    .unwrap_or((None, None, None));

    let consumed = allowance.unwrap_or(0) - remaining;

    // Get breakdown by action type
    let breakdown = sqlx::query_as::<_, (String, i64)>(
        "SELECT action_type, SUM(ABS(credits)) as total
         FROM credit_transactions
         WHERE tenant_id = $1 AND credits < 0
           AND created_at >= COALESCE($2, '1970-01-01'::timestamptz)
         GROUP BY action_type ORDER BY total DESC",
    )
    .bind(tenant_id)
    .bind(period_start)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    serde_json::json!({
        "allowance": allowance.unwrap_or(0),
        "consumed": consumed,
        "remaining": remaining,
        "period_start": period_start,
        "period_end": period_end,
        "breakdown": breakdown.into_iter().map(|(action, total)| {
            serde_json::json!({"action": action, "credits": total})
        }).collect::<Vec<_>>()
    })
}
