use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::models::PrivateEmailPlanFeatures;
use crate::errors::AppError;

/// Check if private email is enabled for a tenant's plan,
/// returning the feature limits. Returns None if feature is disabled.
pub async fn get_plan_features(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<PrivateEmailPlanFeatures>, AppError> {
    let row = sqlx::query_as::<_, (Value,)>(
        r#"
        SELECT p.features
        FROM tenant_plans tp
        JOIN plans p ON p.id = tp.plan_id
        WHERE tp.tenant_id = $1 AND tp.status = 'active'
        LIMIT 1
        "#,
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some((features,)) => {
            let features: PrivateEmailPlanFeatures =
                serde_json::from_value(features).unwrap_or_default();
            if features.private_email {
                Ok(Some(features))
            } else {
                Ok(None)
            }
        }
        None => Ok(None),
    }
}

/// Check if tenant has room for another domain.
pub async fn check_domain_limit(pool: &PgPool, tenant_id: Uuid) -> Result<(), AppError> {
    let features = get_plan_features(pool, tenant_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("Private Email not available on your plan".into()))?;

    if features.max_domains == 0 {
        return Err(AppError::BadRequest("Domain provisioning not available on your plan".into()));
    }

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM private_email_domains WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    if features.max_domains > 0 && count.0 >= features.max_domains as i64 {
        return Err(AppError::BadRequest(format!(
            "Domain limit reached ({}/{})",
            count.0, features.max_domains
        )));
    }

    Ok(())
}

/// Check if tenant has room for another mailbox.
pub async fn check_mailbox_limit(pool: &PgPool, tenant_id: Uuid) -> Result<(), AppError> {
    let features = get_plan_features(pool, tenant_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("Private Email not available on your plan".into()))?;

    if features.max_mailboxes == 0 {
        return Err(AppError::BadRequest("Mailbox provisioning not available on your plan".into()));
    }

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM private_email_boxes WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    if features.max_mailboxes > 0 && count.0 >= features.max_mailboxes as i64 {
        return Err(AppError::BadRequest(format!(
            "Mailbox limit reached ({}/{})",
            count.0, features.max_mailboxes
        )));
    }

    Ok(())
}

pub async fn can_enable_catch_all(pool: &PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let features = get_plan_features(pool, tenant_id)
        .await?
        .ok_or_else(|| AppError::BadRequest("Private Email not available on your plan".into()))?;

    Ok(features.catch_all_enabled)
}
