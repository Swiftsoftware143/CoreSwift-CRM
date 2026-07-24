use sqlx::PgPool;
use std::collections::HashMap;
use std::time::Duration;
use uuid::Uuid;
use super::models::{Score, ScoreRule, ScoringThreshold, ScoringWebhook, score_category};

/// Calculate score for a contact based on an event type.
/// Applies all matching active rules and records history.
pub async fn calculate_score(db: &PgPool, tenant_id: Uuid, contact_id: Uuid, event_type: &str) -> Result<Score, crate::errors::AppError> {
    let rules = sqlx::query_as::<_, ScoreRule>(
        "SELECT * FROM score_rules WHERE tenant_id=$1 AND event_type=$2 AND is_active=true"
    )
    .bind(tenant_id)
    .bind(event_type)
    .fetch_all(db)
    .await?;

    let mut score = sqlx::query_as::<_, Score>(
        "SELECT * FROM contact_scores WHERE tenant_id=$1 AND contact_id=$2"
    )
    .bind(tenant_id)
    .bind(contact_id)
    .fetch_optional(db)
    .await?;

    let score_id = if let Some(ref s) = score {
        s.id
    } else {
        let ns = sqlx::query_as::<_, Score>(
            "INSERT INTO contact_scores(id,tenant_id,contact_id,total_score,category,updated_at) VALUES($1,$2,$3,0,'interested',NOW()) RETURNING *"
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(contact_id)
        .fetch_one(db)
        .await?;
        score = Some(ns.clone());
        ns.id
    };

    let current_score = score.as_ref().map(|s| s.total_score).unwrap_or(0);
    let mut total_points = 0i32;

    for rule in &rules {
        let pts = if rule.direction == "subtract" { -rule.points } else { rule.points };
        total_points += pts;
        let previous = (current_score + total_points - pts).max(0);
        let new_score_val = (current_score + total_points).max(0);

        sqlx::query(
            "INSERT INTO score_history(id,score_id,contact_id,rule_id,tenant_id,points,previous_score,new_score,event_type) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)"
        )
        .bind(Uuid::new_v4())
        .bind(score_id)
        .bind(contact_id)
        .bind(rule.id)
        .bind(tenant_id)
        .bind(pts)
        .bind(previous)
        .bind(new_score_val)
        .bind(event_type)
        .execute(db)
        .await?;
    }

    let final_score = (current_score + total_points).max(0);
    let category = score_category(final_score);
    let old_cat = score.map(|s| s.category).unwrap_or_else(|| "interested".to_string());

    let updated = sqlx::query_as::<_, Score>(
        "UPDATE contact_scores SET total_score=$1, category=$2, last_event_type=$3, last_event_at=NOW(), updated_at=NOW() WHERE id=$4 RETURNING *"
    )
    .bind(final_score)
    .bind(category)
    .bind(event_type)
    .bind(score_id)
    .fetch_one(db)
    .await?;

    if old_cat != category {
        crate::automation::engine::fire_score_trigger(db, tenant_id, contact_id, final_score, category).await;
        let _ = apply_thresholds(db, tenant_id, contact_id, final_score, category).await;
        let _ = fire_scoring_webhooks(db, tenant_id, contact_id, final_score, category).await;
    }

    Ok(updated)
}

/// Ensure a score record exists for a contact, creating one if needed.
pub async fn apply_thresholds(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    total_score: i32,
    _category: &str,
) -> Result<(), crate::errors::AppError> {
    let thresholds = sqlx::query_as::<_, ScoringThreshold>(
        "SELECT * FROM scoring_thresholds
         WHERE tenant_id = $1 AND is_active = true
         AND min_score <= $2 AND (max_score IS NULL OR max_score >= $2)"
    )
    .bind(tenant_id)
    .bind(total_score)
    .fetch_all(db)
    .await?;

    for t in &thresholds {
        match t.action.as_str() {
            "move_stage" => {
                let opp = sqlx::query_as::<_, (Uuid,)>(
                    "SELECT id FROM opportunities WHERE tenant_id = $1 AND contact_id = $2 AND pipeline_id = $3 LIMIT 1"
                )
                .bind(tenant_id)
                .bind(contact_id)
                .bind(t.pipeline_id)
                .fetch_optional(db)
                .await?;

                if let Some((opp_id,)) = opp {
                    sqlx::query(
                        "UPDATE opportunities SET stage_id = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
                    )
                    .bind(t.target_stage_id)
                    .bind(opp_id)
                    .bind(tenant_id)
                    .execute(db)
                    .await?;
                }
            }
            "assign_tag" => {
                if let Some(tag_name) = t.action_config.get("tag_name").and_then(|v| v.as_str()) {
                    if let Ok(Some((tag_id,))) = sqlx::query_as::<_, (Uuid,)>(
                        "SELECT id FROM tags WHERE tenant_id = $1 AND name = $2 LIMIT 1"
                    )
                    .bind(tenant_id)
                    .bind(tag_name)
                    .fetch_optional(db)
                    .await
                    {
                        let _ = sqlx::query(
                            "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id) VALUES ($1, $2, 'contact', $3, $4) ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"
                        )
                        .bind(Uuid::new_v4())
                        .bind(tag_id)
                        .bind(contact_id)
                        .bind(tenant_id)
                        .execute(db)
                        .await;
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Fire configured webhooks when a score threshold is crossed.
pub async fn fire_scoring_webhooks(
    db: &PgPool,
    tenant_id: Uuid,
    contact_id: Uuid,
    total_score: i32,
    _category: &str,
) {
    let webhooks = match sqlx::query_as::<_, ScoringWebhook>(
        "SELECT * FROM scoring_webhooks
         WHERE tenant_id = $1 AND is_active = true
         AND min_score <= $2 AND (max_score IS NULL OR max_score >= $2)"
    )
    .bind(tenant_id)
    .bind(total_score)
    .fetch_all(db)
    .await
    {
        Ok(w) => w,
        Err(_) => return,
    };

    if webhooks.is_empty() {
        return;
    }

    // Look up contact info for the payload
    let contact_info: Option<(String, Option<String>)> = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT CONCAT(first_name, ' ', last_name), email FROM contacts WHERE id = $1 AND tenant_id = $2"
    )
    .bind(contact_id)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .unwrap_or(None);

    let (contact_name, contact_email) = contact_info.unwrap_or_else(|| ("Unknown".into(), None));

    for wh in &webhooks {
        let payload = serde_json::json!({
            "event": "score_threshold_crossed",
            "tenant_id": tenant_id,
            "contact_id": contact_id,
            "email": contact_email,
            "name": contact_name,
            "score": total_score,
            "category": _category,
            "timestamp": chrono::Utc::now(),
        });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();

        let hdrs: HashMap<String, String> =
            serde_json::from_value(wh.headers.clone()).unwrap_or_default();

        let mut req = client.post(&wh.url).json(&payload);
        for (k, v) in &hdrs {
            req = req.header(k.as_str(), v.as_str());
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let _ = sqlx::query(
                    "UPDATE scoring_webhooks SET last_fired_at = NOW(), failure_count = 0 WHERE id = $1"
                )
                .bind(wh.id)
                .execute(db)
                .await;
            }
            _ => {
                let _ = sqlx::query(
                    "UPDATE scoring_webhooks SET failure_count = failure_count + 1 WHERE id = $1"
                )
                .bind(wh.id)
                .execute(db)
                .await;
            }
        }
    }
}

pub async fn ensure_score_record(db: &PgPool, tenant_id: Uuid, contact_id: Uuid) -> Result<Score, crate::errors::AppError> {
    Ok(match sqlx::query_as::<_, Score>("SELECT * FROM contact_scores WHERE tenant_id=$1 AND contact_id=$2")
        .bind(tenant_id)
        .bind(contact_id)
        .fetch_optional(db)
        .await?
    {
        Some(s) => s,
        None => sqlx::query_as::<_, Score>(
            "INSERT INTO contact_scores(id,tenant_id,contact_id,total_score,category,updated_at) VALUES($1,$2,$3,0,'interested',NOW()) RETURNING *"
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(contact_id)
        .fetch_one(db)
        .await?,
    })
}
