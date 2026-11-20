//! Provider Keys module — API key management for external providers.
pub mod handlers;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    let public = Router::new().route(
        "/available-providers",
        axum::routing::get(handlers::list_available_providers),
    );

    let protected = Router::new()
        .route(
            "/provider-keys",
            axum::routing::get(handlers::list_provider_keys).post(handlers::upsert_provider_key),
        )
        .route(
            "/provider-keys/:provider",
            axum::routing::get(handlers::get_provider_key)
                .patch(handlers::update_provider_key)
                .delete(handlers::delete_provider_key),
        )
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "provider_keys", "Provider keys"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    Router::new().merge(public).merge(protected)
}

/// The account's own model keys, as `{provider: plaintext key}`, read from the SEALED store.
///
/// This is the one place model credentials are looked up. The AI paths used to read
/// `tenants.settings->'ai'->'providers'` instead — a different, UNSEALED field that no screen writes, while
/// the screen that does write a key seals it here. Two stores meant a customer could enter a key, watch it
/// save, and still have no AI, with the key they typed sitting readable in a settings blob. Anything that
/// calls a model asks this function.
///
/// Settings are still read afterwards as a fallback, so an account that was configured the old way keeps
/// working; the sealed store always wins, because that is the one the console writes.
/// The model credentials for one account, and where each one came from.
///
/// THE PLATFORM FALLBACK IS EXPLICIT, NOT SILENT. An account with no key of its own used to get nothing —
/// every AI path failed at a provider nobody had configured, and the console showed an empty key list while
/// the feature simply did not work. Now the platform's own credential answers instead (when one is
/// configured), and the caller is TOLD that it did: `platform` lists those providers, so the call can be
/// recorded against our spend rather than the customer's.
pub struct ModelKeys {
    pub keys: std::collections::HashMap<String, String>,
    /// Providers answered by the PLATFORM credential — these cost us, not the account.
    pub platform: std::collections::HashSet<String>,
}

impl ModelKeys {
    /// True when this account has no credential of its own for that provider.
    pub fn on_platform_key(&self, provider: &str) -> bool {
        self.platform.contains(&provider.to_ascii_lowercase())
    }
}

/// The platform's own model credentials, by provider, from the environment.
///
/// Read once per call rather than cached: a key rotated into the environment takes effect without a
/// restart, and this runs a handful of times a minute at most, never in a hot loop.
fn platform_key(provider: &str) -> Option<String> {
    let var = match provider {
        "deepseek" => "DEEPSEEK_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "anthropic" => "ANTHROPIC_API_KEY",
        _ => return None,
    };
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub async fn model_keys(
    db: &sqlx::PgPool,
    tenant_id: uuid::Uuid,
) -> std::collections::HashMap<String, String> {
    model_keys_with_source(db, tenant_id).await.keys
}

/// The same lookup, with the source of each key kept.
pub async fn model_keys_with_source(db: &sqlx::PgPool, tenant_id: uuid::Uuid) -> ModelKeys {
    use std::collections::HashMap;

    let mut map = HashMap::new();

    // ONLY model providers. The table holds every credential an account owns — mailgun, telnyx, and the
    // model keys — and the first version of this query took all of them, so an account with a mail key
    // "had a model key" and every AI call failed at a provider that was never configured.
    let rows: Vec<(uuid::Uuid, String, String)> = sqlx::query_as(
        "SELECT tenant_id, provider, api_key FROM provider_keys
          WHERE tenant_id = $1 AND is_active AND api_key <> ''
            AND lower(provider) IN ('deepseek', 'openai', 'anthropic', 'claude')
          ORDER BY updated_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    .unwrap_or_default();

    for (owner, provider, sealed) in rows {
        let plain = crate::secret_box::open(owner, &sealed);
        if !plain.trim().is_empty() {
            map.entry(provider.to_ascii_lowercase()).or_insert(plain);
        }
    }

    // the older field, only for an account that has nothing in the sealed store
    if map.is_empty() {
        let legacy: Option<Option<serde_json::Value>> =
            sqlx::query_scalar("SELECT settings->'ai'->'providers' FROM tenants WHERE id = $1")
                .bind(tenant_id)
                .fetch_optional(db)
                .await
                .ok()
                .flatten();
        if let Some(Some(serde_json::Value::Object(obj))) = legacy {
            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    if !s.trim().is_empty() {
                        map.insert(k.to_ascii_lowercase(), s.to_string());
                    }
                }
            }
        }
    }

    // THE PLATFORM FALLBACK, applied per provider and only where the account has nothing. Recorded in
    // `platform`, so a call made with our credential is billed to us in the usage table instead of
    // disappearing into a customer's totals.
    let mut platform = std::collections::HashSet::new();
    for provider in ["deepseek", "openai", "anthropic"] {
        if map.contains_key(provider) {
            continue;
        }
        if let Some(key) = platform_key(provider) {
            map.insert(provider.to_string(), key);
            platform.insert(provider.to_string());
        }
    }

    ModelKeys {
        keys: map,
        platform,
    }
}
