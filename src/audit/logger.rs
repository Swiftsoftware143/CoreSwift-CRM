use sqlx::PgPool;
use uuid::Uuid;

/// Log an audit event to the database.
/// This is the centralized audit logger that all modules should call.
/// Fails silently (logs a warning) — audit should never break the main flow.
#[allow(clippy::too_many_arguments)]
pub async fn log_event(
    db: &PgPool,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    changes: Option<serde_json::Value>,
    ip_address: Option<&str>,
) {
    let result = sqlx::query(
        r#"INSERT INTO audit_logs (id, tenant_id, user_id, action, entity_type, entity_id, changes, ip_address)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(user_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(changes)
    .bind(ip_address)
    .execute(db)
    .await;

    if let Err(e) = result {
        tracing::warn!(error = %e, "Failed to write audit log");
    }
}

/// The same audit row, written INSIDE the caller's transaction.
///
/// [`log_event`] fails silently on purpose — an audit line is a side effect, never the point of the
/// request, so a broken trail must not fail a save. A DESTRUCTIVE action is the opposite case: a
/// deletion with no record of who performed it cannot be accounted for afterwards, so this variant
/// RETURNS the error and the caller rolls its whole transaction back — the row is not removed unless
/// the trail lands with it.
#[allow(clippy::too_many_arguments)]
pub async fn log_event_tx(
    conn: &mut sqlx::PgConnection,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    action: &str,
    entity_type: &str,
    entity_id: Option<Uuid>,
    changes: Option<serde_json::Value>,
    ip_address: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"INSERT INTO audit_logs (id, tenant_id, user_id, action, entity_type, entity_id, changes, ip_address)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(user_id)
    .bind(action)
    .bind(entity_type)
    .bind(entity_id)
    .bind(changes)
    .bind(ip_address)
    .execute(conn)
    .await
    .map(|_| ())
}
