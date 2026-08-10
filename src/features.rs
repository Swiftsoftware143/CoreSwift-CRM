//! Feature limit enforcement — reads limits from plans table (JSONB features + dedicated columns).
use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn enforce_feature_limit(
    db: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    // Get plan slug from tenant_plans
    let plan_slug: Option<String> = sqlx::query_scalar(
        "SELECT p.slug FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id WHERE tp.tenant_id = $1 AND tp.status = 'active'"
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?
    .flatten();

    let slug = match plan_slug {
        Some(s) => s,
        None => return Ok(()),
    };

    // Check dedicated columns first
    if feature_key == "max_industries" || feature_key == "industries" {
        let limit: Option<i64> =
            sqlx::query_scalar("SELECT max_industries FROM plans WHERE slug = $1")
                .bind(&slug)
                .fetch_optional(db)
                .await?
                .flatten();

        if let Some(limit) = limit {
            if limit == -1 {
                return Ok(());
            }
            if limit == 0 {
                return Err(AppError::UpgradeRequired(format!(
                    "{} is not available on your current plan.",
                    label
                )));
            }
            let usage: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM industries WHERE tenant_id = $1")
                    .bind(tenant_id)
                    .fetch_one(db)
                    .await?;
            if usage >= limit {
                return Err(AppError::UpgradeRequired(format!(
                    "{} limit reached ({}/{}). Upgrade to increase your limit.",
                    label, usage, limit
                )));
            }
            return Ok(());
        }
    }

    // Check JSONB features column for other limits
    let json_key = match feature_key {
        "max_users" | "users" | "team_members" => "max_users",
        "pipelines" => "pipelines",
        "integrations" | "max_integrations" => "integrations",
        "max_contacts" | "contacts" | "leads" => "max_contacts",
        _ => return Ok(()),
    };

    let limit: Option<i64> =
        sqlx::query_scalar("SELECT (features->>$2)::bigint FROM plans WHERE slug = $1")
            .bind(&slug)
            .bind(json_key)
            .fetch_optional(db)
            .await?
            .flatten();

    match limit {
        None | Some(-1) => Ok(()),
        Some(0) => Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan.",
            label
        ))),
        Some(limit) => {
            let usage = count_usage(db, tenant_id, feature_key).await?;
            if usage >= limit {
                Err(AppError::UpgradeRequired(format!(
                    "{} limit reached ({}/{}). Upgrade to increase your limit.",
                    label, usage, limit
                )))
            } else {
                Ok(())
            }
        }
    }
}

pub async fn get_usage_json(db: &PgPool, tenant_id: Uuid) -> serde_json::Value {
    let contacts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    let industries: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM industries WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
    let pipelines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pipelines WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0);
    let users: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true")
            .bind(tenant_id)
            .fetch_one(db)
            .await
            .unwrap_or(0);
    serde_json::json!({
        "contacts": contacts,
        "industries": industries,
        "pipelines": pipelines,
        "users": users
    })
}

async fn count_usage(db: &PgPool, tenant_id: Uuid, feature_key: &str) -> Result<i64, AppError> {
    match feature_key {
        "max_contacts" | "contacts" | "leads" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM contacts WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await?),
        "max_users" | "users" | "team_members" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true",
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await?),
        "pipelines" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM pipelines WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await?),
        "integrations" | "max_integrations" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM integration_targets WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await?),
        "max_industries" | "industries" => Ok(sqlx::query_scalar(
            "SELECT COUNT(*) FROM industries WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_one(db)
        .await?),
        _ => Ok(0),
    }
}
