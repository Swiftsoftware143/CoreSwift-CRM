use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::models::{PrivateEmailPlanFeatures, TenantEmailLimits};
use crate::errors::AppError;

/// Sentinel for "no numeric cap configured".
///
/// The registry stores an unlimited limit as `limit_value = NULL` **with the feature enabled**
/// (`enabled = false` means the plan does not get the feature at all). Every check below compares
/// `count >= max`, so a very large number behaves exactly like SQL's `NULL` would.
const UNLIMITED: i32 = i32::MAX;

/// Fetch any tenant-specific email limit overrides (returns None if none set).
pub async fn get_tenant_limits(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<TenantEmailLimits>, AppError> {
    sqlx::query_as::<_, TenantEmailLimits>("SELECT * FROM tenant_email_limits WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Database)
}

/// Plan features and limits for private email, resolved from the MODULE REGISTRY (CS-20/CS-26).
///
/// Returns `None` when the tenant's plan does not get the private-mailbox module at all.
///
/// The numeric limits used to be read out of `plans.features` JSON. That is why they fired for
/// nobody: `tenant_email_limits` had 0 rows and no plan defined `max_domains` / `max_mailboxes`, so
/// both numbers came back 0 and the "limit reached" branches were unreachable. They are DATA in the
/// registry now (`module_features` where `kind='limit'`), which is the same table the admin panel
/// edits — so a limit can be changed without a redeploy and without a hardcoded table.
///
/// `max_aliases_per_mailbox` and `catch_all_enabled` have no registry feature yet; they still come
/// from the plan JSON exactly as before, so nothing about them changes here.
pub async fn get_plan_features(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<PrivateEmailPlanFeatures>, AppError> {
    let module = crate::module_registry::resolve(pool, tenant_id, "private_email").await?;

    // A tenant with NO active plan row resolves as ALLOWED at module level (deliberate legacy
    // tolerance in module_registry). Private email was plan-gated before this change and stays
    // gated: `no_plan` is not a plan, and treating it as one would hand every plan-less tenant
    // unlimited domains — the opposite of enforcing the sold limits.
    if !module.enabled || module.source == "no_plan" {
        return Ok(None);
    }

    let max_domains = limit_for(pool, tenant_id, "email_domains").await?;
    let max_mailboxes = limit_for(pool, tenant_id, "email_mailboxes").await?;
    let legacy = legacy_alias_features(pool, tenant_id).await?;

    Ok(Some(PrivateEmailPlanFeatures {
        private_email: true,
        max_domains,
        max_mailboxes,
        max_aliases_per_mailbox: legacy.max_aliases_per_mailbox,
        catch_all_enabled: legacy.catch_all_enabled,
    }))
}

/// One numeric limit from the registry: `enabled=false` -> 0 ("not available on your plan"),
/// an enabled feature with no `limit_value` -> unlimited, otherwise the configured number.
async fn limit_for(pool: &PgPool, tenant_id: Uuid, key: &str) -> Result<i32, AppError> {
    let ent = crate::module_registry::resolve(pool, tenant_id, key).await?;
    if !ent.enabled {
        return Ok(0);
    }
    Ok(match ent.limit_value {
        None => UNLIMITED,
        Some(v) if v <= 0.0 => 0,
        Some(v) => v.min(i32::MAX as f64) as i32,
    })
}

/// The pre-registry read of `plans.features`, kept only for the two keys that have no registry
/// feature yet. Absent JSON or an unrecognised shape yields the defaults (0 / false).
async fn legacy_alias_features(
    pool: &PgPool,
    tenant_id: Uuid,
) -> Result<PrivateEmailPlanFeatures, AppError> {
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

    Ok(match row {
        Some((features,)) => {
            serde_json::from_value::<PrivateEmailPlanFeatures>(features).unwrap_or_default()
        }
        None => PrivateEmailPlanFeatures::default(),
    })
}

/// Check if tenant has room for another domain.
pub async fn check_domain_limit(pool: &PgPool, tenant_id: Uuid) -> Result<(), AppError> {
    let features = get_plan_features(pool, tenant_id).await?.ok_or_else(|| {
        AppError::UpgradeRequired(
            "Private email is not available on your current plan. Upgrade to add your own \
                 domain and mailboxes."
                .into(),
        )
    })?;

    // Check for admin override first
    let max_domains = if let Some(limits) = get_tenant_limits(pool, tenant_id).await? {
        limits.max_domains.unwrap_or(features.max_domains)
    } else {
        features.max_domains
    };

    if max_domains == 0 {
        return Err(AppError::UpgradeRequired(
            "Domain provisioning is not available on your plan. Upgrade to add a domain.".into(),
        ));
    }

    let count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM private_email_domains WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(pool)
            .await
            .map_err(AppError::Database)?;

    if max_domains > 0 && count.0 >= max_domains as i64 {
        return Err(AppError::UpgradeRequired(format!(
            "Domain limit reached ({}/{}). Upgrade to add more domains.",
            count.0, max_domains
        )));
    }

    Ok(())
}

/// Check if tenant has room for another mailbox.
pub async fn check_mailbox_limit(pool: &PgPool, tenant_id: Uuid) -> Result<(), AppError> {
    let features = get_plan_features(pool, tenant_id).await?.ok_or_else(|| {
        AppError::UpgradeRequired(
            "Private email is not available on your current plan. Upgrade to add your own \
                 domain and mailboxes."
                .into(),
        )
    })?;

    // Check for admin override first
    let max_mailboxes = if let Some(limits) = get_tenant_limits(pool, tenant_id).await? {
        limits.max_mailboxes.unwrap_or(features.max_mailboxes)
    } else {
        features.max_mailboxes
    };

    if max_mailboxes == 0 {
        return Err(AppError::UpgradeRequired(
            "Mailbox provisioning is not available on your plan. Upgrade to create a mailbox."
                .into(),
        ));
    }

    let count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM private_email_boxes WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    if max_mailboxes > 0 && count.0 >= max_mailboxes as i64 {
        return Err(AppError::UpgradeRequired(format!(
            "Mailbox limit reached ({}/{}). Upgrade to create more mailboxes.",
            count.0, max_mailboxes
        )));
    }

    Ok(())
}

pub async fn can_enable_catch_all(pool: &PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let features = get_plan_features(pool, tenant_id).await?.ok_or_else(|| {
        AppError::UpgradeRequired(
            "Private email is not available on your current plan. Upgrade to enable it.".into(),
        )
    })?;

    Ok(features.catch_all_enabled)
}
