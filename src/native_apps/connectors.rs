//! Native App Connectors — actual API clients for each first-party app
//!
//! Each app gets a module here that knows how to talk to that app's API.
//! Connectors are loaded dynamically based on the `slug` field.

use std::collections::HashMap;

pub mod adaswift;
pub mod funnelswift;
pub mod incentiveswift;
pub mod missedcall_responder;
pub mod multi_directory;
pub mod workflowswift;

/// Registered connector metadata
pub struct AppConnector {
    pub slug: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub auth_type: &'static str,
    pub auth_fields: &'static [&'static str],
    pub access_level: &'static str, // "admin" | "admin_tenant"
}

/// All known native apps
pub static NATIVE_APPS: &[AppConnector] = &[
    AppConnector {
        slug: "adaswift",
        name: "AdaSwift Console",
        description: "Client viewing portal — clients see their scan reports and account status. Admin-only: connect with an AdaSwift admin bearer token to push clients and pull scan reports.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin",
    },
    AppConnector {
        slug: "funnelswift",
        name: "FunnelSwift",
        description: "Sales funnel builder — mobile (Expo) app for building and managing sales funnels. Tenants connect their own FunnelSwift account.",
        auth_type: "api_key",
        // FunnelSwift's connector takes the tenant's API key plus the API base the
        // tenant's instance answers on (same shape as every other connector). It used
        // to advertise `webhook_secret`, which the connector never read — so the
        // Integration Center collected a field nothing consumed and the connection
        // test hit a hardcoded URL no deployment answers.
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "workflowswift",
        name: "WorkflowSwift Automation",
        description: "n8n-based workflow automation engine (Postgres backend). Tenants connect their workflow instance.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "missedcall-responder",
        name: "MissedCall Responder",
        description: "Callback Pro SaaS — missed call handling with SMS auto-reply, hybrid LLM suite, lead kanban board.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "incentiveswift",
        name: "IncentiveSwift",
        description: "Loyalty and rewards engine — programmes, campaigns, members and points. Tenants connect their own IncentiveSwift account with an API key from its API Keys screen.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin_tenant",
    },
    AppConnector {
        slug: "multi-directory",
        name: "Multi-Directory App",
        description: "Multi-account business directory system with automated follow-up sequences across directories. Admin-only — internal tool.",
        auth_type: "api_key",
        auth_fields: &["api_key", "base_url"],
        access_level: "admin",
    },
];

/// Why a caller may not connect `app` — the ONE access rule BOTH connecting doors apply
/// (kanban t_0db782c6).
///
/// The two doors, and what each knows:
///
/// * [`crate::native_apps::handlers::connect_app`] — the Integration Center (SPA) door. It has
///   a `Claims`, so it passes `is_account_admin(claims.role)`.
/// * [`crate::webhook::actions::route_action`]'s `native.connect` arm — the automation door. It
///   has a TENANT and no user: an `automation_webhooks` row carries `tenant_id`, `name`,
///   `webhook_token` and `allowed_actions` — never an author. A token therefore carries no role
///   and is treated as the tenant's least-privileged member (`is_person_admin = false`).
///
/// DECISION: an `access_level = "admin"` connector (`adaswift`, `multi-directory`) is
/// platform-operator tooling — it is driven with an AdaSwift admin bearer token, i.e. a
/// credential the PLATFORM holds on the operator's behalf. Connecting one requires an admin
/// PERSON, which only the SPA door can establish. A tenant-scoped automation token cannot, so
/// the webhook door refuses an app that an account admin of the same tenant may connect from
/// the console. That difference is intended and is exactly why the rule lives here, in one
/// function called by both doors, instead of in either of them: the doors do not disagree about
/// the rule, they differ only in the identity they can prove. An `admin_tenant` connector
/// belongs to the tenant itself, so both doors allow it.
///
/// Rejected alternative (kanban t_0db782c6 option 2 — "tenant tokens are trusted operator
/// tooling"): it would have required widening `connect_app` to match, i.e. handing every tenant
/// that owns a webhook token the platform's operator credentials. That is a privilege expansion
/// with no functional gain: the operator connects these apps from the console.
pub fn connect_refusal(app: &AppConnector, is_person_admin: bool) -> Option<String> {
    if app.access_level == "admin" && !is_person_admin {
        Some(format!(
            "{} is available to the platform operator only — connect it from the Integration \
             Center while signed in as an account admin.",
            app.name
        ))
    } else {
        None
    }
}

/// Test a connection to the given app with the provided credentials.
/// Returns (success, message, latency_ms).
pub async fn test_connection(
    slug: &str,
    credentials: &serde_json::Value,
) -> (bool, String, Option<i64>) {
    let start = std::time::Instant::now();

    let result = match slug {
        "adaswift" => adaswift::test(credentials).await,
        "funnelswift" => funnelswift::test(credentials).await,
        "incentiveswift" => incentiveswift::test(credentials).await,
        "workflowswift" => workflowswift::test(credentials).await,
        "missedcall-responder" => missedcall_responder::test(credentials).await,
        "multi-directory" => multi_directory::test(credentials).await,
        _ => (false, format!("Unknown app: {}", slug)),
    };

    let latency = Some(start.elapsed().as_millis() as i64);
    (result.0, result.1, latency)
}

/// Push data into a native app from CRM Swift.
pub async fn push_data(
    slug: &str,
    credentials: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match slug {
        "adaswift" => adaswift::push_entity(credentials, entity_type, data).await,
        "funnelswift" => funnelswift::push_entity(credentials, entity_type, data).await,
        "incentiveswift" => incentiveswift::push_entity(credentials, entity_type, data).await,
        "workflowswift" => workflowswift::push_entity(credentials, entity_type, data).await,
        "missedcall-responder" => {
            missedcall_responder::push_entity(credentials, entity_type, data).await
        }
        "multi-directory" => multi_directory::push_entity(credentials, entity_type, data).await,
        _ => Err(format!("Unknown app: {}", slug)),
    }
}

/// Pull data from a native app into CRM Swift.
pub async fn pull_data(
    slug: &str,
    credentials: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    match slug {
        "adaswift" => adaswift::pull_entity(credentials, entity_type, filters).await,
        "funnelswift" => funnelswift::pull_entity(credentials, entity_type, filters).await,
        "incentiveswift" => incentiveswift::pull_entity(credentials, entity_type, filters).await,
        "workflowswift" => workflowswift::pull_entity(credentials, entity_type, filters).await,
        "missedcall-responder" => {
            missedcall_responder::pull_entity(credentials, entity_type, filters).await
        }
        "multi-directory" => multi_directory::pull_entity(credentials, entity_type, filters).await,
        _ => Err(format!("Unknown app: {}", slug)),
    }
}

/// Get app metadata (labels, available entities, etc.)
pub fn get_app_meta(slug: &str) -> Option<serde_json::Value> {
    match slug {
        "adaswift" => Some(adaswift::get_meta()),
        "funnelswift" => Some(funnelswift::get_meta()),
        "incentiveswift" => Some(incentiveswift::get_meta()),
        "workflowswift" => Some(workflowswift::get_meta()),
        "missedcall-responder" => Some(missedcall_responder::get_meta()),
        "multi-directory" => Some(multi_directory::get_meta()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(slug: &str) -> &'static AppConnector {
        NATIVE_APPS
            .iter()
            .find(|a| a.slug == slug)
            .expect("catalogue entry")
    }

    /// The webhook door (`is_person_admin = false`) refuses every admin-only connector —
    /// the exact set the catalogue marks for the platform operator.
    #[test]
    fn tenant_token_may_not_connect_admin_only_apps() {
        for a in NATIVE_APPS.iter().filter(|a| a.access_level == "admin") {
            let reason = connect_refusal(a, false).expect("admin-only app must be refused");
            assert!(
                reason.contains("platform operator only"),
                "refusal must say who may connect it: {reason}"
            );
            assert!(
                reason.contains(a.name),
                "refusal must name the app: {reason}"
            );
        }
        // …and the two connectors the card names are in that set.
        assert!(connect_refusal(app("adaswift"), false).is_some());
        assert!(connect_refusal(app("multi-directory"), false).is_some());
    }

    /// The SPA door passes the signed-in role, so an account admin IS allowed — that is the
    /// intended difference between the doors, and it must not drift.
    #[test]
    fn account_admin_may_connect_admin_only_apps_from_the_console() {
        assert!(connect_refusal(app("adaswift"), true).is_none());
        assert!(connect_refusal(app("multi-directory"), true).is_none());
    }

    /// `admin_tenant` connectors belong to the tenant, so both doors allow them (the webhook
    /// door passes `false` and must still be allowed).
    #[test]
    fn admin_tenant_apps_are_allowed_on_both_doors() {
        for a in NATIVE_APPS
            .iter()
            .filter(|a| a.access_level == "admin_tenant")
        {
            assert!(
                connect_refusal(a, false).is_none(),
                "{} is tenant-scoped and must be connectable by a tenant token",
                a.slug
            );
            assert!(connect_refusal(a, true).is_none(), "{}", a.slug);
        }
        assert!(connect_refusal(app("workflowswift"), false).is_none());
    }
}
