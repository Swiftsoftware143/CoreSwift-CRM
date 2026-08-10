use sqlx::PgPool;
use uuid::Uuid;

pub async fn dispatch_webhook(
    _db: &PgPool,
    tenant_id: Uuid,
    event: &str,
    _payload: &serde_json::Value,
) {
    tracing::debug!(
        "Dispatching webhook event '{}' for tenant {}",
        event,
        tenant_id
    );
}
