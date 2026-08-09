//! Background purge task for email data retention.
//!
//! Deletes outbound_messages and email-related events older than the
//! tenant-specific retention window. Default retention is 365 days;
//! agency admins can override per tenant (min 30 days, max 3650 days).

use sqlx::PgPool;
use uuid::Uuid;

use super::models::PurgeSummaryResponse;

/// Purge email data for all tenants (or a single tenant) based on their
/// retention policy. Each tenant's `retention_days` in `tenant_email_limits`
/// determines the cutoff; falls back to 365 days if no override exists.
pub async fn purge_expired_emails(
    pool: &PgPool,
    target_tenant: Option<Uuid>,
) -> PurgeSummaryResponse {
    let mut summary = PurgeSummaryResponse::new();

    // ── 1. Determine which tenants to process ──
    let tenants: Vec<Uuid> = match get_purge_targets(pool, target_tenant).await {
        Ok(t) => {
            summary.tenants_checked = t.len() as i64;
            t
        }
        Err(e) => {
            summary.errors.push(format!("Failed to list tenants: {}", e));
            return summary;
        }
    };

    if tenants.is_empty() {
        return summary;
    }

    // ── 2. For each tenant, run the purge ──
    for tenant_id in &tenants {
        match purge_single_tenant(pool, *tenant_id).await {
            Ok((msgs, evts)) => {
                if msgs > 0 || evts > 0 {
                    summary.tenants_purged += 1;
                    summary.messages_deleted += msgs;
                    summary.events_deleted += evts;
                    tracing::info!(
                        tenant = %tenant_id,
                        messages = msgs,
                        events = evts,
                        "Purged expired email data"
                    );

                    // Update last_purged_at
                    let _ = sqlx::query(
                        r#"
                        UPDATE tenant_email_limits
                        SET last_purged_at = NOW()
                        WHERE tenant_id = $1 AND EXISTS (
                            SELECT 1 FROM tenant_email_limits WHERE tenant_id = $1
                        )
                        "#,
                    )
                    .bind(tenant_id)
                    .execute(pool)
                    .await;
                }
            }
            Err(e) => {
                let msg = format!("Tenant {} purge error: {}", tenant_id, e);
                tracing::warn!("{}", msg);
                summary.errors.push(msg);
            }
        }
    }

    summary
}

/// Returns the list of tenant UUIDs to purge.
/// If `target_tenant` is provided, returns just that one.
async fn get_purge_targets(
    pool: &PgPool,
    target_tenant: Option<Uuid>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    if let Some(tid) = target_tenant {
        return Ok(vec![tid]);
    }

    // All tenants that have outbound_messages
    let rows: Vec<(Uuid,)> = sqlx::query_as(
        "SELECT DISTINCT tenant_id FROM outbound_messages",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(id,)| id).collect())
}

/// Purge expired email records for a single tenant.
/// Returns (messages_deleted, events_deleted).
async fn purge_single_tenant(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<(i64, i64), sqlx::Error> {
    // Determine retention window for this tenant
    let retention_days = get_retention_days(pool, tenant_id).await;

    // Delete expired outbound_messages
    let msg_result = sqlx::query(
        r#"
        DELETE FROM outbound_messages
        WHERE tenant_id = $1
          AND created_at < NOW() - ($2 || ' days')::INTERVAL
        "#,
    )
    .bind(tenant_id)
    .bind(retention_days.to_string())
    .execute(pool)
    .await?;
    let messages_deleted = msg_result.rows_affected() as i64;

    // Delete expired email-related events (source = 'private_email')
    let evt_result = sqlx::query(
        r#"
        DELETE FROM events
        WHERE tenant_id = $1
          AND source = 'private_email'
          AND created_at < NOW() - ($2 || ' days')::INTERVAL
        "#,
    )
    .bind(tenant_id)
    .bind(retention_days.to_string())
    .execute(pool)
    .await?;
    let events_deleted = evt_result.rows_affected() as i64;

    Ok((messages_deleted, events_deleted))
}

/// Look up the retention window for a tenant.
/// Falls back to 365 days when no override is set.
async fn get_retention_days(pool: &PgPool, tenant_id: Uuid) -> i32 {
    let row: Option<(Option<i32>,)> = sqlx::query_as(
        "SELECT retention_days FROM tenant_email_limits WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    row.and_then(|(days,)| days).unwrap_or(365)
}
