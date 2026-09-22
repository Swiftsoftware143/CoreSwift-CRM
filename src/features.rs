//! Feature limit enforcement — reads limits from plans table (JSONB features + dedicated columns).
//!
//! Plan-level BOOLEAN gating no longer lives here. It is data-driven: `crate::module_registry`
//! resolves a tenant's entitlement from the `modules` / `module_features` / `plan_modules` /
//! `plan_module_features` tables that the ADMIN assigns through `/api/admin/plans/:slug/*`.
//!
//! Removed in this change (they were the hardcoding David asked to delete):
//!   * the catalogue as a Rust const (the `FeatureDef` list)
//!   * the alias bridge table next to it, whose own comment warned that drift made the gate
//!     "silently stop enforcing". The four alias pairs are now `modules.legacy_feature_key`
//!     DATA, folded into the seed by migration 072.
//!   * `feature_registry_json` — the admin UI now reads the catalogue from the database via
//!     `GET /api/admin/modules`.
use crate::errors::AppError;
use crate::module_registry;
use sqlx::PgPool;
use uuid::Uuid;

#[allow(dead_code)]
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

#[allow(dead_code)]
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

// ─────────────────────────────────────────────────────────────────────────────
// Per-plan BOOLEAN feature gating.
//
// Resolution order (most specific wins), all of it DATA now — see `module_registry::resolve`:
//   1. tenant_plans.feature_overrides->>'<key>'          per-tenant override
//   2. the tenant's active plan's assignment rows        plan_modules / plan_module_features
//   3. DENY                                              (fail-closed)
//
// It used to be a JSONB lookup on `plans.features` with `unset => ALLOWED`, bridged by a
// hardcoded alias table.
// An unset key being allowed meant a module could ship ungated and a renamed key could silently stop
// enforcing; the registry replaced both. Seeding `plan_modules` from the same `plans.features` data
// (migration 072) is what kept that switch behaviour-preserving.
// ─────────────────────────────────────────────────────────────────────────────

pub async fn enforce_feature_flag(
    db: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    let ent = module_registry::resolve(db, tenant_id, feature_key).await?;
    if ent.enabled {
        return Ok(());
    }
    match ent.source {
        // No active plan row at all: legacy tolerance, kept so the 81 tenants without one do not
        // lose every module in a single deploy. Reported, not hidden.
        "no_plan" => Ok(()),
        _ => Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access it.",
            label
        ))),
    }
}

/// Middleware state for `gate_mw`: which module protects which router.
#[derive(Clone)]
pub struct FeatureGate {
    pub db: PgPool,
    pub key: &'static str,
    pub label: &'static str,
}

impl FeatureGate {
    pub fn new(db: PgPool, key: &'static str, label: &'static str) -> Self {
        Self { db, key, label }
    }
}

/// Enforce a module's plan flag on every request that reaches it.
///
/// MUST be layered INSIDE the auth middleware (i.e. listed before it, since the
/// last `.layer()` applied is the outermost) so `Claims` is already in the request
/// extensions. A request with no claims is passed through untouched — unauthenticated
/// routes are not this layer's business, and the auth layer will reject them anyway.
pub async fn gate_mw(
    axum::extract::State(gate): axum::extract::State<FeatureGate>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, AppError> {
    if let Some(claims) = req.extensions().get::<crate::auth::models::Claims>() {
        if let Ok(tenant_id) = Uuid::parse_str(&claims.aid) {
            enforce_feature_flag(&gate.db, tenant_id, gate.key, gate.label).await?;
        }
    }
    Ok(next.run(req).await)
}
