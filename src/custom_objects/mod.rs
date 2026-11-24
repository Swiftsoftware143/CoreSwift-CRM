//! Custom Objects — the owned no-code backend (Phase 1, MCP-first).
//!
//! Replaces Bolt.new + NoCodeBackend, which sold exactly one thing this platform did not have: an
//! MCP-capable editor describes a data shape and gets objects, fields, records and a key without
//! anyone writing schema. Everything else in their pitch already runs here (hosting, Postgres,
//! tenancy, key minting, MCP), so this is a PROVISIONING module inside CoreSwift-CRM — not a new app
//! and not a new database.
//!
//! ## Two doors, one writer
//!
//! | Door | Address | Credential |
//! |---|---|---|
//! | MCP (JSON-RPC 2.0) | `POST /api/mcp` | workspace API key, or a console session |
//! | REST | `/api/objects/**` | workspace API key, or a console session |
//!
//! Both classes of credential resolve to exactly ONE account and land in [`ObjCtx`]; both doors call
//! the same functions in [`store`]. That is deliberate: validation, uniqueness enforcement and the
//! audit trail cannot drift between the door an agent uses and the door an application uses.
//!
//! ## Tenancy
//!
//! The spec says `workspace_id`; this app's unit is `tenant_id` (the account), because that is what
//! `personal_api_keys` resolves to and a machine caller has no `workspaces.id` at all. A tenant is
//! NEVER read from a body, a query string or a path — only from the credential, here, once.
//!
//! ## Credentials and the app-wide boundary
//!
//! `/api/objects/**` and `/api/mcp` appear in [`crate::auth::route_policy::API_KEY_ROUTES`], so the
//! global boundary only requires that a non-empty bearer be presented; the decision about WHICH
//! credential it is happens in [`authenticate`] below, which accepts exactly two shapes and refuses
//! everything else:
//!
//! * `Authorization: Bearer csk_…` — an issued personal API key, hashed and looked up by
//!   [`crate::external_api::resolve_key`] (the same reader every `/api/external/**` route uses).
//! * `Authorization: Bearer <app JWT>` — a console session, verified by the canonical
//!   [`crate::auth::middleware::verify_token`].
//!
//! `x-api-key` is deliberately not supported: no CoreSwift surface uses it, and a header the boundary
//! never looks at would be a door that answers 401 with no explanation.

pub mod handlers;
pub mod mcp;
pub mod store;

use axum::{
    extract::{Request, State},
    http::header,
    middleware,
    response::{IntoResponse, Response},
    Router,
};
use uuid::Uuid;

use crate::errors::AppError;
use crate::AppState;

/// What one request acts as: the account, and who it is (for the audit row). Never the key itself.
#[derive(Clone, Debug)]
pub struct ObjCtx {
    pub tenant_id: Uuid,
    /// `key:<personal_api_keys.id>` or `user:<users.id>` — enough to attribute a change, nothing
    /// secret.
    pub actor: String,
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The module's ONE credential resolver. Mounted on both doors, it runs before every handler and
/// inserts the [`ObjCtx`] they require — so a handler cannot be reached without a resolved account,
/// and there is no second place where tenancy is decided.
pub async fn authenticate(
    State(state): State<AppState>,
    mut req: Request,
    next: middleware::Next,
) -> Response {
    let Some(token) = bearer(&req) else {
        return AppError::UnauthorizedReason(
            "Authentication required — send the workspace API key as `Authorization: Bearer csk_…` \
             (or your console session token)"
                .into(),
        )
        .into_response();
    };

    let ctx = if token.starts_with("csk_") {
        // The issued-key reader: hashes the key and looks it up in `personal_api_keys`. It also
        // meters the call and stamps `last_used_at`, exactly as the `/api/external/**` routes do.
        match crate::external_api::resolve_key(&state, req.headers()).await {
            Ok((tenant_id, key_id)) => ObjCtx {
                tenant_id,
                actor: format!("key:{key_id}"),
            },
            // One sentence for every failure — unknown, revoked, and not-a-key all look the same, so
            // a caller cannot probe which keys exist.
            Err(_) => return AppError::UnauthorizedReason(
                "that API key is not active — mint a new one in the console (Settings → API keys)"
                    .into(),
            )
            .into_response(),
        }
    } else {
        let claims = match crate::auth::middleware::verify_token(&token, &state.config.jwt_secret) {
            Ok(claims) => claims,
            Err(_) => return AppError::Unauthorized.into_response(),
        };

        // A signature-valid token is NOT a credential by itself. Reproduce the canonical boundary
        // checks from `auth::middleware::auth_middleware` HERE as well, so this door is never weaker
        // than the rest of the app: a logged-out token (Redis blacklist), a token naming a retired
        // workspace, a deleted/disabled user, and a session minted before the account's last password
        // change all answer 401 — exactly as they do on every other route.
        {
            let mut conn = state.redis.clone();
            let check = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                redis::cmd("GET")
                    .arg(format!("blacklist:{token}"))
                    .query_async::<Option<String>>(&mut conn),
            )
            .await;
            if let Ok(Ok(Some(_))) = check {
                tracing::warn!("custom_objects: rejected a blacklisted (logged-out) token");
                return AppError::Unauthorized.into_response();
            }
        }

        // Fail CLOSED on a database error rather than leaking that the database is down; the caller
        // still gets no access, which is the property that matters at a credential boundary.
        match crate::auth::middleware::workspace_exists(&state.db, &claims.aid).await {
            Ok(true) => {}
            _ => {
                tracing::warn!(workspace = %claims.aid, "custom_objects: rejected a token whose workspace is gone");
                return AppError::Unauthorized.into_response();
            }
        }

        match crate::auth::middleware::user_session_state(&state.db, &claims.sub).await {
            Ok(Some((true, changed_at))) => {
                if crate::auth::middleware::session_predates_password_change(
                    Some(claims.iat as i64),
                    changed_at,
                ) {
                    tracing::warn!(user = %claims.sub, "custom_objects: rejected a session that predates the password change");
                    return AppError::Unauthorized.into_response();
                }
            }
            _ => {
                tracing::warn!(user = %claims.sub, "custom_objects: rejected a token whose user is gone or disabled");
                return AppError::Unauthorized.into_response();
            }
        }

        match Uuid::parse_str(claims.aid.trim()) {
            Ok(tenant_id) => ObjCtx {
                tenant_id,
                actor: format!("user:{}", claims.sub),
            },
            Err(_) => return AppError::Unauthorized.into_response(),
        }
    };

    req.extensions_mut().insert(ctx);
    next.run(req).await
}

/// The REST surface. Registered in [`crate::auth::route_policy`]'s census as the
/// `custom_objects/mod.rs` nest, so `main.rs` holds no route literals for this module.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", axum::routing::get(handlers::list_objects))
        .route("/", axum::routing::post(handlers::create_object))
        .route("/:slug", axum::routing::delete(handlers::archive_object))
        .route("/:slug", axum::routing::patch(handlers::patch_object))
        .route(
            "/:slug/schema",
            axum::routing::get(handlers::describe_object),
        )
        .route("/:slug/fields", axum::routing::post(handlers::add_field))
        .route(
            "/:slug/fields/:id",
            axum::routing::patch(handlers::patch_field),
        )
        .route(
            "/:slug/fields/:id",
            axum::routing::delete(handlers::archive_field),
        )
        .route(
            "/:slug/records",
            axum::routing::get(handlers::query_records),
        )
        .route(
            "/:slug/records",
            axum::routing::post(handlers::insert_record),
        )
        .route(
            "/:slug/records/:id",
            axum::routing::patch(handlers::update_record),
        )
        .route(
            "/:slug/records/:id",
            axum::routing::delete(handlers::delete_record),
        )
        .route("/:slug/audit", axum::routing::get(handlers::list_audit))
        .layer(middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
}

/// Named here so the module compiles its own unit test of the slug/kind rules without a database.
#[cfg(test)]
mod tests {
    use super::store::{check_slug, clamp_window, KINDS};

    #[test]
    fn the_slug_alphabet_is_the_one_the_check_constraint_enforces() {
        assert!(check_slug("customer_orders").is_ok());
        assert!(check_slug("a").is_ok());
        assert!(check_slug("orders2").is_ok());
        // Refused: an uppercase letter, a leading digit, a hyphen, a space, a path segment, a quote.
        assert!(check_slug("Orders").is_err());
        assert!(check_slug("2orders").is_err());
        assert!(check_slug("customer-orders").is_err());
        assert!(check_slug("customer orders").is_err());
        assert!(check_slug("orders/1").is_err());
        assert!(check_slug("orders'; drop table x; --").is_err());
        assert!(check_slug("").is_err());
        assert!(check_slug(&"a".repeat(64)).is_err());
    }

    #[test]
    fn the_pagination_window_is_bounded() {
        assert_eq!(clamp_window(None, None).unwrap(), (50, 0));
        assert_eq!(clamp_window(Some("10"), Some("5")).unwrap(), (10, 5));
        // A caller cannot ask a database for a million rows, and a negative offset is refused.
        assert_eq!(clamp_window(Some("100000"), None).unwrap(), (200, 0));
        assert!(clamp_window(Some("0"), None).is_err());
        assert!(clamp_window(None, Some("-1")).is_err());
        assert!(clamp_window(Some("ten"), None).is_err());
    }

    #[test]
    fn the_kind_vocabulary_matches_the_check_constraint() {
        // Migration 204's CHECK lists the same six values; this pins the pair so a seventh kind
        // cannot be added to Rust alone and start answering 500 on insert.
        assert_eq!(KINDS, ["text", "number", "bool", "date", "enum", "json"]);
    }
}
