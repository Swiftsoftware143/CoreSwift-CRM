//! Feature limit enforcement — reads limits from plans table (JSONB features + dedicated columns).
use crate::errors::AppError;
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
// Per-plan BOOLEAN feature gating — "the admin controls what features are
// available per plan".
//
// Resolution order (most specific wins):
//   1. tenant_plans.feature_overrides->>'<key>'   per-tenant override
//   2. plans.features->>'<key>'                   the plan's own flag
//   3. unset  -> ALLOWED
//
// Unset = allowed is deliberate: plans in the wild do not carry every key, and
// denying on absence would silently strip modules from existing tenants the
// moment this shipped. An explicit `false` is what turns a feature off, which is
// what the admin UI writes. Seed the plans with explicit flags for real tiers.
//
// No active plan -> allowed, matching `enforce_feature_limit`'s behaviour.
// ─────────────────────────────────────────────────────────────────────────────
pub async fn enforce_feature_flag(
    db: &PgPool,
    tenant_id: Uuid,
    feature_key: &str,
    label: &str,
) -> Result<(), AppError> {
    let row: Option<(serde_json::Value, Option<serde_json::Value>)> = sqlx::query_as(
        "SELECT p.features, tp.feature_overrides
           FROM tenant_plans tp
           JOIN plans p ON p.id = tp.plan_id
          WHERE tp.tenant_id = $1 AND tp.status = 'active'
          LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?;

    let Some((features, overrides)) = row else {
        return Ok(()); // no active plan — do not lock the tenant out
    };

    let enabled = overrides
        .as_ref()
        .and_then(|o| o.get(feature_key))
        .and_then(|v| v.as_bool())
        .or_else(|| features.get(feature_key).and_then(|v| v.as_bool()));

    match enabled {
        Some(false) => Err(AppError::UpgradeRequired(format!(
            "{} is not available on your current plan. Upgrade to access it.",
            label
        ))),
        _ => Ok(()),
    }
}

/// A feature the admin can toggle per plan. This is the single source of truth the
/// admin UI renders its switches from, so add new keys HERE when gating a module.
pub struct FeatureDef {
    pub key: &'static str,
    pub label: &'static str,
    pub module: &'static str,
    pub note: &'static str,
}

pub const FEATURE_REGISTRY: &[FeatureDef] = &[
    FeatureDef {
        key: "campaigns",
        label: "Campaigns",
        module: "campaigns",
        note: "Sequenced email campaigns",
    },
    FeatureDef {
        key: "automation",
        label: "Automations",
        module: "automation",
        note: "Trigger/action engine",
    },
    FeatureDef {
        key: "checklists",
        label: "Checklists",
        module: "checklists",
        note: "Onboarding/process checklists",
    },
    FeatureDef {
        key: "ai_enabled",
        label: "AI scoring",
        module: "ai, scoring",
        note: "Lead scoring and AI helpers",
    },
    FeatureDef {
        key: "tickets",
        label: "Support tickets",
        module: "tickets",
        note: "In-house ticketing + email-to-ticket",
    },
    FeatureDef {
        key: "affiliates",
        label: "Affiliate system",
        module: "affiliates",
        note: "Referral tracking and payouts",
    },
    FeatureDef {
        key: "native_apps",
        label: "Native app connectors",
        module: "native_apps",
        note: "FunnelSwift, ADASwift, MissedCall, WorkflowSwift, CheatLayer, Multi-Directory",
    },
    FeatureDef {
        key: "telnyx",
        label: "SMS & voice (Telnyx)",
        module: "telnyx, comms",
        note: "SMS, number management, call tracking",
    },
    FeatureDef {
        key: "round_robin",
        label: "Round-robin routing",
        module: "round_robin",
        note: "Fair lead distribution across a team",
    },
    FeatureDef {
        key: "events",
        label: "Event system",
        module: "events",
        note: "Internal event bus",
    },
    FeatureDef {
        key: "monitoring",
        label: "Monitoring & health",
        module: "monitoring",
        note: "Account health scoring and thresholds",
    },
    FeatureDef {
        key: "bookings",
        label: "Bookings & scheduling",
        module: "bookings",
        note: "Calendar booking pages",
    },
    FeatureDef {
        key: "google_calendar",
        label: "Google Calendar sync",
        module: "google_calendar",
        note: "Two-way calendar sync",
    },
    FeatureDef {
        key: "api_access",
        label: "API access",
        module: "personal_api_keys",
        note: "Per-tenant API keys",
    },
    FeatureDef {
        key: "webhooks",
        label: "Webhooks",
        module: "webhook",
        note: "Outbound webhook delivery",
    },
    FeatureDef {
        key: "integrations",
        label: "Integrations",
        module: "integrations",
        note: "n8n and third-party integrations",
    },
    FeatureDef {
        key: "provider_keys",
        label: "Provider keys",
        module: "provider_keys",
        note: "Bring-your-own provider credentials",
    },
    FeatureDef {
        key: "support_widgets",
        label: "Support widgets",
        module: "support_widgets",
        note: "Embeddable support surfaces",
    },
    FeatureDef {
        key: "tracked_links",
        label: "Tracked links",
        module: "tracked_links",
        note: "Click tracking links",
    },
    FeatureDef {
        key: "private_email",
        label: "Private email",
        module: "private_email",
        note: "Own domain + mailboxes (also has its own limits)",
    },
    FeatureDef {
        key: "portfolio",
        label: "Portfolio sync",
        module: "portfolio",
        note: "Cross-tenant portfolio management",
    },
];

pub fn feature_registry_json() -> serde_json::Value {
    serde_json::json!(FEATURE_REGISTRY
        .iter()
        .map(|f| serde_json::json!({
            "key": f.key, "label": f.label, "module": f.module, "note": f.note
        }))
        .collect::<Vec<_>>())
}

/// Middleware state for `gate_mw`: which plan flag protects which module.
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
