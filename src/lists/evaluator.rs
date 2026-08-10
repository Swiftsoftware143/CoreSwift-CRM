use super::models::{List, ListRule};
use crate::errors::AppError;
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn evaluate_dynamic_list(
    db: &PgPool,
    list: &List,
) -> Result<serde_json::Value, AppError> {
    let rules: Vec<ListRule> = if let Some(ref r) = list.rules {
        serde_json::from_value(r.clone()).unwrap_or_default()
    } else {
        Vec::new()
    };
    if rules.is_empty() {
        return Ok(json!({"message":"No rules","added":0,"removed":0}));
    }

    let mut matched: Vec<Uuid> = Vec::new();
    for rule in &rules {
        match rule.field.as_str() {
            "tag" => {
                let tn = rule.value.as_str().unwrap_or("");
                let ids = sqlx::query_scalar::<_,Uuid>("SELECT DISTINCT ta.entity_id FROM tag_assignments ta JOIN tags t ON t.id=ta.tag_id WHERE ta.tenant_id=$1 AND ta.entity_type='contact' AND t.name=$2")
                    .bind(list.tenant_id).bind(tn).fetch_all(db).await?;
                matched.extend(ids);
            }
            "score" => {
                if rule.operator == "gte" {
                    let ms = rule.value.as_i64().unwrap_or(0) as i32;
                    matched.extend(
                        sqlx::query_scalar::<_, Uuid>(
                            "SELECT contact_id FROM scores WHERE tenant_id=$1 AND total_score>=$2",
                        )
                        .bind(list.tenant_id)
                        .bind(ms)
                        .fetch_all(db)
                        .await?,
                    );
                } else if rule.operator == "lte" {
                    let ms = rule.value.as_i64().unwrap_or(0) as i32;
                    matched.extend(
                        sqlx::query_scalar::<_, Uuid>(
                            "SELECT contact_id FROM scores WHERE tenant_id=$1 AND total_score<=$2",
                        )
                        .bind(list.tenant_id)
                        .bind(ms)
                        .fetch_all(db)
                        .await?,
                    );
                } else if rule.operator == "equals" {
                    let cat = rule.value.as_str().unwrap_or("cold");
                    matched.extend(sqlx::query_scalar::<_,Uuid>("SELECT contact_id FROM scores WHERE tenant_id=$1 AND category=$2::score_category").bind(list.tenant_id).bind(cat).fetch_all(db).await?);
                }
            }
            _ => tracing::warn!("Unknown rule field: {}", rule.field),
        }
    }
    matched.sort();
    matched.dedup();

    let existing: Vec<Uuid> =
        sqlx::query_scalar("SELECT contact_id FROM list_members WHERE list_id=$1 AND tenant_id=$2")
            .bind(list.id)
            .bind(list.tenant_id)
            .fetch_all(db)
            .await?;

    let to_add: Vec<Uuid> = matched
        .iter()
        .filter(|id| !existing.contains(id))
        .copied()
        .collect();
    let to_remove: Vec<Uuid> = existing
        .iter()
        .filter(|id| !matched.contains(id))
        .copied()
        .collect();

    for cid in &to_add {
        let _ = sqlx::query("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,false) ON CONFLICT DO NOTHING")
            .bind(Uuid::new_v4()).bind(list.id).bind(cid).bind(list.tenant_id).execute(db).await;
    }
    for cid in &to_remove {
        let _ = sqlx::query(
            "DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2 AND tenant_id=$3",
        )
        .bind(list.id)
        .bind(cid)
        .bind(list.tenant_id)
        .execute(db)
        .await;
    }

    Ok(
        json!({"message":"Evaluated","total_matched":matched.len(),"added":to_add.len(),"removed":to_remove.len()}),
    )
}
