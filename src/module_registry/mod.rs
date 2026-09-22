//! Data-driven MODULE & FEATURE registry (CS-25/CS-26).
//!
//! Single source of truth for "what this tenant is entitled to". Everything that used to be a Rust
//! constant lives in four tables now:
//!
//! * `modules`              — one row per tool (Campaigns, Private mailbox, SMS/Telnyx, …)
//! * `module_features`      — one row per capability of that tool (boolean toggles, numeric limits)
//! * `plan_modules`         — the ADMIN's module → plan assignment
//! * `plan_module_features` — the ADMIN's feature → plan assignment (with `limit_value` for limits)
//!
//! Resolution order, most specific first:
//!   1. `tenant_plans.feature_overrides->>'<key>'`   an explicit per-tenant override
//!   2. the tenant's ACTIVE plan: `plan_modules.enabled`      (when `<key>` names a module)
//!      or                        `plan_module_features`      (when it names a feature)
//!   3. **deny**
//!
//! Step 3 is the whole point: the previous implementation fail-OPENed on an unknown key, so a new
//! module could ship ungated and a renamed key silently stopped enforcing. Unknown now denies.
//!
//! ONE residual exception, deliberate and measured: a tenant with **no active `tenant_plans` row**
//! resolves as ALLOWED (`source = "no_plan"`). Today that path allows everything, and 81 tenants (10
//! of them with users) have no plan row — turning it into a deny would strip every module from them
//! in a single deploy. New signups DO get a free-plan row (`auth::handlers`), so this is legacy
//! tolerance, not a design choice; it is reported so the admin can assign those tenants a plan and
//! shrink it to zero.

use crate::errors::AppError;
use serde_json::Value as Json;
use sqlx::PgPool;
use uuid::Uuid;

pub mod handlers;

/// The outcome of one entitlement lookup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Entitlement {
    pub key: String,
    pub enabled: bool,
    pub limit_value: Option<f64>,
    /// Where the answer came from: `tenant_override` | `plan_module` | `plan_feature` | `unassigned`
    /// | `no_plan`.
    pub source: &'static str,
}

/// Read a boolean / limit out of a `feature_overrides` blob. Accepts a bare boolean
/// (`{"tickets": false}`) and an object (`{"email_domains": {"enabled": true, "limit_value": 3}}`).
fn override_value(ov: &Json, key: &str) -> Option<(bool, Option<f64>)> {
    let v = ov.get(key)?;
    if let Some(b) = v.as_bool() {
        return Some((b, None));
    }
    let enabled = v.get("enabled").and_then(|e| e.as_bool())?;
    let limit = v.get("limit_value").and_then(|l| l.as_f64());
    Some((enabled, limit))
}

type ResolveRow = (
    Json,         // feature_overrides
    bool,         // module exists in the catalogue
    Option<bool>, // plan_modules.enabled
    bool,         // feature exists in the catalogue
    Option<bool>, // plan_module_features.enabled
    Option<f64>,  // plan_module_features.limit_value
);

/// Resolve one key (module key or feature key) for one tenant.
pub async fn resolve(db: &PgPool, tenant_id: Uuid, key: &str) -> Result<Entitlement, AppError> {
    let row: Option<ResolveRow> = sqlx::query_as(
        r#"SELECT COALESCE(tp.feature_overrides, '{}'::jsonb),
                  (m.id IS NOT NULL),
                  pm.enabled,
                  (f.id IS NOT NULL),
                  pf.enabled,
                  pf.limit_value::float8
             FROM tenant_plans tp
             LEFT JOIN modules m  ON m.key = $2
             LEFT JOIN plan_modules pm ON pm.plan_id = tp.plan_id AND pm.module_id = m.id
             LEFT JOIN module_features f ON f.key = $2
             LEFT JOIN plan_module_features pf
                    ON pf.plan_id = tp.plan_id AND pf.module_feature_id = f.id
            WHERE tp.tenant_id = $1 AND tp.status = 'active'
            LIMIT 1"#,
    )
    .bind(tenant_id)
    .bind(key)
    .fetch_optional(db)
    .await?;

    let Some((
        overrides,
        module_exists,
        module_enabled,
        feature_exists,
        feature_enabled,
        feature_limit,
    )) = row
    else {
        return Ok(Entitlement {
            key: key.to_string(),
            enabled: true,
            limit_value: None,
            source: "no_plan",
        });
    };

    if let Some((enabled, limit)) = override_value(&overrides, key) {
        return Ok(Entitlement {
            key: key.to_string(),
            enabled,
            limit_value: limit,
            source: "tenant_override",
        });
    }

    if module_exists {
        return Ok(Entitlement {
            key: key.to_string(),
            enabled: module_enabled.unwrap_or(false),
            limit_value: None,
            source: "plan_module",
        });
    }

    if feature_exists {
        return Ok(Entitlement {
            key: key.to_string(),
            enabled: feature_enabled.unwrap_or(false),
            limit_value: feature_limit,
            source: "plan_feature",
        });
    }

    // Unknown key: fail closed. A module that ships without being assigned to a plan is denied
    // rather than silently open (the old behaviour that let the gate "silently stop enforcing").
    Ok(Entitlement {
        key: key.to_string(),
        enabled: false,
        limit_value: None,
        source: "unassigned",
    })
}

/// The effective value for every catalogue key, for one tenant. Used by the admin entitlements view.
pub async fn resolve_all(db: &PgPool, tenant_id: Uuid) -> Result<Vec<Entitlement>, AppError> {
    let keys: Vec<(String,)> = sqlx::query_as(
        "SELECT key FROM modules WHERE is_active
         UNION ALL SELECT key FROM module_features WHERE is_active
         ORDER BY 1",
    )
    .fetch_all(db)
    .await?;

    let mut out = Vec::with_capacity(keys.len());
    for (k,) in keys {
        out.push(resolve(db, tenant_id, &k).await?);
    }
    Ok(out)
}

/// The catalogue plus every plan's assignment matrix — what the admin panel renders.
pub async fn catalogue(db: &PgPool) -> Result<Json, AppError> {
    #[derive(sqlx::FromRow)]
    struct ModuleRow {
        key: String,
        name: String,
        description: Option<String>,
        icon: Option<String>,
        sort_order: i32,
        is_active: bool,
        legacy_feature_key: Option<String>,
    }
    #[derive(sqlx::FromRow)]
    struct FeatureRow {
        module_key: String,
        key: String,
        name: String,
        description: Option<String>,
        kind: String,
        unit: Option<String>,
        sort_order: i32,
        is_active: bool,
        legacy_feature_key: Option<String>,
    }

    let modules: Vec<ModuleRow> = sqlx::query_as(
        "SELECT key, name, description, icon, sort_order, is_active, legacy_feature_key
           FROM modules ORDER BY sort_order, key",
    )
    .fetch_all(db)
    .await?;

    let features: Vec<FeatureRow> = sqlx::query_as(
        "SELECT m.key AS module_key, f.key, f.name, f.description, f.kind, f.unit,
                f.sort_order, f.is_active, f.legacy_feature_key
           FROM module_features f JOIN modules m ON m.id = f.module_id
          ORDER BY m.sort_order, f.sort_order, f.key",
    )
    .fetch_all(db)
    .await?;

    let plans: Vec<(String, String, i32)> = sqlx::query_as(
        "SELECT slug, name, sort_order FROM plans ORDER BY sort_order NULLS LAST, slug",
    )
    .fetch_all(db)
    .await?;

    let module_assign: Vec<(String, String, bool)> = sqlx::query_as(
        "SELECT p.slug, m.key, pm.enabled
           FROM plan_modules pm JOIN plans p ON p.id = pm.plan_id JOIN modules m ON m.id = pm.module_id",
    )
    .fetch_all(db)
    .await?;

    let feature_assign: Vec<(String, String, bool, Option<f64>)> = sqlx::query_as(
        "SELECT p.slug, f.key, pf.enabled, pf.limit_value::float8
           FROM plan_module_features pf
           JOIN plans p ON p.id = pf.plan_id
           JOIN module_features f ON f.id = pf.module_feature_id",
    )
    .fetch_all(db)
    .await?;

    let mut mods = Vec::new();
    for m in modules {
        let mut feats = Vec::new();
        for f in features.iter().filter(|f| f.module_key == m.key) {
            let mut assigns = serde_json::Map::new();
            for (slug, _fkey, enabled, limit) in
                feature_assign.iter().filter(|(_, k, _, _)| *k == f.key)
            {
                assigns.insert(
                    slug.clone(),
                    serde_json::json!({ "enabled": enabled, "limit_value": limit }),
                );
            }
            feats.push(serde_json::json!({
                "key": f.key, "name": f.name, "description": f.description,
                "kind": f.kind, "unit": f.unit, "sort_order": f.sort_order,
                "is_active": f.is_active, "legacy_feature_key": f.legacy_feature_key,
                "assignments": json_object(assigns),
            }));
        }
        let mut assigns = serde_json::Map::new();
        for (slug, _mkey, enabled) in module_assign.iter().filter(|(_, k, _)| *k == m.key) {
            assigns.insert(slug.clone(), serde_json::json!({ "enabled": enabled }));
        }
        mods.push(serde_json::json!({
            "key": m.key, "name": m.name, "description": m.description, "icon": m.icon,
            "sort_order": m.sort_order, "is_active": m.is_active,
            "legacy_feature_key": m.legacy_feature_key,
            "features": feats,
            "assignments": json_object(assigns),
        }));
    }

    Ok(serde_json::json!({
        "plans": plans
            .into_iter()
            .map(|(slug, name, sort_order)| serde_json::json!({
                "slug": slug, "name": name, "sort_order": sort_order
            }))
            .collect::<Vec<_>>(),
        "modules": mods,
    }))
}

fn json_object(m: serde_json::Map<String, Json>) -> Json {
    Json::Object(m)
}

/// Keep the legacy `PATCH /api/plans/:id {features: {...}}` write path alive.
///
/// The admin console used to toggle plan flags by writing that JSONB, and `plans.features` is still
/// the column it writes. Now that the registry is the source of truth, a write that only landed in
/// the JSONB would look successful and change nothing — so each recognised key is mirrored onto the
/// matching `plan_modules` / `plan_module_features` row. Unknown keys are left alone (they are not
/// entitlement), and no hardcoded key list is involved: the mapping is the `legacy_feature_key`
/// column the seed already carries.
pub async fn sync_legacy_features(
    db: &PgPool,
    plan_id: Uuid,
    features: &Json,
) -> Result<usize, AppError> {
    let Some(obj) = features.as_object() else {
        return Ok(0);
    };
    let mut changed = 0usize;
    for (k, v) in obj {
        if let Some(b) = v.as_bool() {
            let n = sqlx::query(
                "INSERT INTO plan_modules (plan_id, module_id, enabled)
                 SELECT $1, id, $2 FROM modules WHERE key = $3 OR legacy_feature_key = $3
                 ON CONFLICT (plan_id, module_id)
                 DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
            )
            .bind(plan_id)
            .bind(b)
            .bind(k)
            .execute(db)
            .await?;
            changed += n.rows_affected() as usize;

            let n = sqlx::query(
                "INSERT INTO plan_module_features (plan_id, module_feature_id, enabled)
                 SELECT $1, id, $2 FROM module_features WHERE key = $3 OR legacy_feature_key = $3
                 ON CONFLICT (plan_id, module_feature_id)
                 DO UPDATE SET enabled = EXCLUDED.enabled, updated_at = now()",
            )
            .bind(plan_id)
            .bind(b)
            .bind(k)
            .execute(db)
            .await?;
            changed += n.rows_affected() as usize;
        } else if let Some(num) = v.as_f64() {
            let n = sqlx::query(
                "INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
                 SELECT $1, id, true, $2 FROM module_features
                  WHERE (key = $3 OR legacy_feature_key = $3) AND kind = 'limit'
                 ON CONFLICT (plan_id, module_feature_id)
                 DO UPDATE SET enabled = true, limit_value = EXCLUDED.limit_value, updated_at = now()",
            )
            .bind(plan_id)
            .bind(num)
            .bind(k)
            .execute(db)
            .await?;
            changed += n.rows_affected() as usize;
        }
    }
    Ok(changed)
}
