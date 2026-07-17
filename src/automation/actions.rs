use sqlx::PgPool;
use uuid::Uuid;
use crate::errors::AppError;
use super::models::AutomationRule;

pub async fn execute_action(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    match rule.action_type.as_str() {
        "AddTag" => exec_add_tag(db, rule, tenant_id, entity_type, entity_id).await,
        "RemoveTag" => exec_remove_tag(db, rule, entity_type, entity_id).await,
        "MovePipeline" => exec_move_pipeline(db, rule, tenant_id, entity_id).await,
        "AddToList" => exec_add_to_list(db, rule, tenant_id, entity_id).await,
        "RemoveFromList" => exec_remove_from_list(db, rule, entity_id).await,
        "Webhook" => { tracing::info!("Webhook action (stub)"); Ok(()) },
        "NotifyUser" => { tracing::info!("Notify action (stub)"); Ok(()) },
        _ => Ok(()),
    }
}

async fn exec_add_tag(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let tid_str = rule.action_config.get("tag_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing tag_id".into()))?;
    let tag_id = Uuid::parse_str(tid_str).map_err(|_| AppError::Validation("Invalid tag_id".into()))?;
    let exists: bool = sqlx::query_scalar("SELECT COUNT(*) FROM tag_assignments WHERE tag_id=$1 AND entity_type=$2::entity_type AND entity_id=$3 AND tenant_id=$4").bind(tag_id).bind(entity_type).bind(entity_id).bind(tenant_id).fetch_one(db).await.unwrap_or(0) > 0;
    if !exists { sqlx::query("INSERT INTO tag_assignments(id,tag_id,entity_type,entity_id,tenant_id) VALUES($1,$2,$3::entity_type,$4,$5)").bind(Uuid::new_v4()).bind(tag_id).bind(entity_type).bind(entity_id).bind(tenant_id).execute(db).await?; }
    Ok(())
}

async fn exec_remove_tag(db: &PgPool, rule: &AutomationRule, entity_type: &str, entity_id: Uuid) -> Result<(), AppError> {
    let tid_str = rule.action_config.get("tag_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing tag_id".into()))?;
    let tag_id = Uuid::parse_str(tid_str).map_err(|_| AppError::Validation("Invalid tag_id".into()))?;
    sqlx::query("DELETE FROM tag_assignments WHERE tag_id=$1 AND entity_type=$2::entity_type AND entity_id=$3").bind(tag_id).bind(entity_type).bind(entity_id).execute(db).await?;
    Ok(())
}

async fn exec_move_pipeline(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_id: Uuid) -> Result<(), AppError> {
    let sid_str = rule.action_config.get("stage_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing stage_id".into()))?;
    let stage_id = Uuid::parse_str(sid_str).map_err(|_| AppError::Validation("Invalid stage_id".into()))?;
    let r = sqlx::query("UPDATE opportunities SET stage_id=$1, updated_at=NOW() WHERE id=$2 AND tenant_id=$3").bind(stage_id).bind(entity_id).bind(tenant_id).execute(db).await?;
    if r.rows_affected() > 0 { sqlx::query("INSERT INTO stage_history(id,opportunity_id,to_stage_id) VALUES($1,$2,$3)").bind(Uuid::new_v4()).bind(entity_id).bind(stage_id).execute(db).await?; }
    Ok(())
}

async fn exec_add_to_list(db: &PgPool, rule: &AutomationRule, tenant_id: Uuid, entity_id: Uuid) -> Result<(), AppError> {
    let lid_str = rule.action_config.get("list_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing list_id".into()))?;
    let list_id = Uuid::parse_str(lid_str).map_err(|_| AppError::Validation("Invalid list_id".into()))?;
    sqlx::query("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,false) ON CONFLICT DO NOTHING")
        .bind(Uuid::new_v4()).bind(list_id).bind(entity_id).bind(tenant_id).execute(db).await?;
    Ok(())
}

async fn exec_remove_from_list(db: &PgPool, rule: &AutomationRule, entity_id: Uuid) -> Result<(), AppError> {
    let lid_str = rule.action_config.get("list_id").and_then(|v| v.as_str()).ok_or(AppError::Validation("Missing list_id".into()))?;
    let list_id = Uuid::parse_str(lid_str).map_err(|_| AppError::Validation("Invalid list_id".into()))?;
    sqlx::query("DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2").bind(list_id).bind(entity_id).execute(db).await?;
    Ok(())
}
