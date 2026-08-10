use sqlx::PgPool;
use uuid::Uuid;

pub async fn fire_tag_trigger(
    db: &PgPool,
    tenant_id: Uuid,
    entity_type: &str,
    entity_id: Uuid,
    tag_id: Uuid,
    trigger_type: &str,
) {
    tracing::debug!("Tag trigger: type={trigger_type}, entity={entity_type}/{entity_id}, tag={tag_id}, tenant={tenant_id}");
    if let Err(e) = crate::automation::engine::evaluate_tag_triggers(
        db,
        tenant_id,
        entity_type,
        entity_id,
        tag_id,
        trigger_type,
    )
    .await
    {
        tracing::error!("Tag trigger eval error: {e:?}");
    }
}
