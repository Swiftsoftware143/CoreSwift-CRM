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

// `enforce_feature_limit()` + `count_usage()` used to live here. They carried the last hardcoded
// limit catalogue in this file — a `max_industries` arm that read `plans` through `tenants.plan_id`
// (NULL for every one of the 118 live tenants, so the "limit" was silently the literal fallback 1 and
// would not have moved when the admin changed it) plus jsonb arms for max_users / pipelines /
// integrations / max_contacts. Both were dead (`#[allow(dead_code)]`, zero callers): boolean gating
// went to `module_registry`, and the industry ceiling is now resolved there too, as the admin-assignable
// `limit_max_industries` feature (`crate::industries::handlers::industry_limit`). Deleted rather than
// left as a second source of truth that silently stops enforcing (kanban t_0986ba98).
//
// `get_usage_json` below is still the live reader of the legacy numeric keys; its `industries` number
// now comes from `industries::handlers::active_count` — the SAME expression the industry gate checks.

// The usage numbers below are the SHARED expressions: `get_usage_json` renders them and every
// numeric gate (`enforce_usage_limit`) checks them, so the number a customer sees and the number
// the gate enforces cannot drift (same discipline as `industries::handlers::active_count`).
//
// `#[allow(dead_code)]` is not used: each one has a live caller (the guard or the usage JSON).
pub async fn count_contacts(db: &PgPool, tenant_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

pub async fn count_pipelines(db: &PgPool, tenant_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM pipelines WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

pub async fn count_integrations(db: &PgPool, tenant_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM integrations WHERE tenant_id = $1")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

/// Active users of the workspace — `users.is_active`, the same expression the plan's
/// `limit_max_users` ceiling is written against (an inactive member does not consume a seat).
pub async fn count_active_users(db: &PgPool, tenant_id: Uuid) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND is_active = true")
        .bind(tenant_id)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

pub async fn get_usage_json(db: &PgPool, tenant_id: Uuid) -> serde_json::Value {
    let contacts = count_contacts(db, tenant_id).await;
    // The SAME expression the industry gate checks (`industries::handlers::active_count`): the
    // tenant's ACTIVE industry tabs, read through the `industries` view. Counting every row — the
    // shape this had while the module was unmounted — would count deactivated tabs too, so the number
    // would not move when a user deactivated one (kanban t_0986ba98).
    let industries = crate::industries::handlers::active_count(db, tenant_id)
        .await
        .unwrap_or(0);
    let pipelines = count_pipelines(db, tenant_id).await;
    let users = count_active_users(db, tenant_id).await;
    serde_json::json!({
        "contacts": contacts,
        "industries": industries,
        "pipelines": pipelines,
        "users": users
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// NUMERIC CEILINGS (the `limits` module).
//
// A boolean flag is enforced by reading the plan key; a numeric limit is enforced by comparing
// USAGE against it — so each wired key needs both halves in ONE place: the ceiling resolved from
// the registry row the admin's Features & Plans panel edits, and the usage count above. The guard
// is called ONLY on the route that ADDS the counted row: a usage ceiling copied onto a
// GET/PATCH/DELETE wedges the tenant at its limit (on FunnelSwift every list/edit/delete 402d), and
// a usage limit can never be checked on a read.
//
// House semantics — identical to `support_widgets::create_widget` and
// `private_email::feature_gate::limit_for`:
//   * the plan assignment says `enabled = false` -> ceiling 0 -> "not available on your plan";
//   * a NEGATIVE assignment is the documented unlimited sentinel -> no ceiling;
//   * an enabled limit with no number has no ceiling;
//   * a tenant with NO active plan row keeps its modules (`no_plan`) -> no ceiling, rather than
//     this module inventing one the admin never assigned.
// ─────────────────────────────────────────────────────────────────────────────

/// The effective integer ceiling of a `kind = 'limit'` registry feature, or `None` for "no ceiling".
pub async fn usage_ceiling(
    db: &PgPool,
    tenant_id: Uuid,
    key: &str,
) -> Result<Option<i64>, AppError> {
    let ent = module_registry::resolve(db, tenant_id, key).await?;
    if !ent.enabled {
        return Ok(Some(0));
    }
    Ok(match ent.limit_value {
        None => None,
        Some(v) if v < 0.0 => None,
        Some(v) => Some(v.floor() as i64),
    })
}

/// The 402 body for a reached ceiling — one wording for every limit so the panel guide and the
/// API agree, and an upsell (402), not a malformed-request (400).
pub fn limit_reached_error(label: &str, plural: &str, usage: i64, limit: i64) -> AppError {
    AppError::UpgradeRequired(format!(
        "{} limit reached ({}/{}). Upgrade your plan for more {}.",
        label, usage, limit, plural
    ))
}

/// Deny with **402** when `usage` has reached the tenant's ceiling for `key`.
///
/// Returns the effective ceiling (`None` = no ceiling) so a BULK path (CSV import) can keep
/// enforcing the same number row by row instead of asking for it a second time.
pub async fn enforce_usage_limit(
    db: &PgPool,
    tenant_id: Uuid,
    key: &str,
    label: &str,
    plural: &str,
    usage: i64,
) -> Result<Option<i64>, AppError> {
    let Some(limit) = usage_ceiling(db, tenant_id, key).await? else {
        return Ok(None);
    };
    if usage >= limit {
        return Err(limit_reached_error(label, plural, usage, limit));
    }
    Ok(Some(limit))
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
