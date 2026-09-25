//! Auth middleware: JWT verification and account extraction.
//!
//! Provides `auth_middleware` for protected routes and utility functions
//! for token creation and role checking.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use super::models::Claims;
use crate::errors::AppError;
use crate::AppState;

/// Auth middleware that extracts team member context from JWT bearer token.
/// For routes that require authentication, attach via
/// `axum::middleware::from_fn_with_state(state, auth_middleware)`.
/// Skips auth for internal sync endpoints (validated by x-internal-key header).
/// # Errors
/// Returns `401 Unauthorized` if the token is missing, malformed, or expired.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    // Skip auth for internal sync routes — validated by x-internal-key header
    let path = req.uri().path();
    if path.ends_with("/internal") || path.contains("/internal/") {
        return Ok(next.run(req).await);
    }

    let auth_header = req
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    let claims = verify_token(token, &state.config.jwt_secret)?;

    // Reject tokens invalidated by POST /api/auth/logout. The logout handler has always written
    // `blacklist:<token>` to Redis, but NOTHING ever read it — so "Logged out successfully" was
    // a lie and a signed-out token stayed replayable for its full remaining life (proven live
    // 2026-09-21: after a 200 logout the same bearer token still returned 200 from a protected
    // route). Fail OPEN on a Redis error or a slow cache: an unreachable blacklist must not lock
    // every authenticated request out of the app, and the JWT is still signature- and
    // expiry-checked regardless.
    {
        let mut conn = state.redis.clone();
        let check = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            redis::cmd("GET")
                .arg(format!("blacklist:{token}"))
                .query_async::<Option<String>>(&mut conn),
        )
        .await;
        match check {
            Ok(Ok(Some(_))) => {
                tracing::warn!("rejected a blacklisted (logged-out) token");
                return Err(AppError::Unauthorized);
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "blacklist lookup failed; allowing request"),
            Err(_) => tracing::warn!("blacklist lookup timed out; allowing request"),
        }
    }

    // A signature-valid token is NOT automatically a valid credential: `aid` names a workspace
    // (`tenants.id`), and a workspace can be retired while its tokens are still inside their 24h
    // access-token life. Deleting a workspace cascades its `users` rows away, so a token that was
    // minted before the delete outlives everything that could have caught it here.
    //
    // Measured live 2026-09-25 (t_524bfbb9) before this check, with a token minted from this
    // container's own JWT_SECRET carrying a workspace id that is not in `tenants`:
    //   * 150 of 153 probed GET routes ACCEPTED it (103 x 200, 46 x 404) — reads answered exactly
    //     like a live workspace that owns nothing, so a caller could not tell "gone" from "empty";
    //   * writes into a `tenants`-referencing table died on the FK as a generic 500
    //     "Database error" (6 of 8 probed creates: portfolio_companies, companies, tags, lists,
    //     email_campaigns, pipelines).
    // Reads and writes disagreed, and the answer depended on which table a route happened to
    // touch. The claim is what is wrong, so the answer belongs here: 401 for a workspace that no
    // longer exists, on every route, exactly like an expired token — and the SPA already turns a
    // 401 into "session expired, sign in again" (`www-app/index.html:149`).
    //
    // Cost: one primary-key lookup on `tenants` per authenticated request (p50 4ms / p95 5ms for
    // POST /api/portfolio before and after, measured). If the database is unreachable the check
    // answers 500 like every handler behind it — it must not fail open, or a dead-workspace token
    // would slip through precisely when the FK is the only thing left to refuse it.
    if !workspace_exists(&state.db, &claims.aid).await? {
        tracing::warn!(
            workspace = %claims.aid,
            user = %claims.sub,
            "rejected a token whose workspace no longer exists"
        );
        return Err(AppError::Unauthorized);
    }

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Is the workspace named by a token's `aid` claim still in `tenants`?
///
/// `Err(AppError::Unauthorized)` when the claim is not even a workspace id; `Err(AppError::Database)`
/// when the database cannot answer — never `Ok(false)`, so an outage is not reported as "your
/// workspace is gone" (`live-error-path-proof`). Both are pinned by the tests below.
pub async fn workspace_exists(db: &sqlx::PgPool, aid: &str) -> Result<bool, AppError> {
    let workspace = uuid::Uuid::parse_str(aid).map_err(|e| {
        tracing::warn!(error = %e, claim = %aid, "token's workspace claim is not a workspace id");
        AppError::Unauthorized
    })?;

    let found: Option<uuid::Uuid> = sqlx::query_scalar("SELECT id FROM tenants WHERE id = $1")
        .bind(workspace)
        .fetch_optional(db)
        .await?;

    Ok(found.is_some())
}

/// Verify a JWT token and return the claims.
pub fn verify_token(token: &str, secret: &str) -> Result<Claims, AppError> {
    use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 30;
    validation.validate_exp = true;
    validation.set_issuer(&["coreswift"]);
    validation.set_audience(&["coreswift-api"]);

    let token_data = decode::<Claims>(token, &decoding_key, &validation).map_err(|e| {
        tracing::warn!(error = %e, "JWT verification failed");
        AppError::Unauthorized
    })?;

    Ok(token_data.claims)
}

/// Create a JWT access token.
pub fn create_access_token(claims: &Claims, secret: &str) -> Result<String, AppError> {
    use jsonwebtoken::{encode, EncodingKey, Header};

    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    encode(&Header::default(), claims, &encoding_key).map_err(|e| {
        tracing::error!(error = %e, "Failed to create JWT");
        AppError::Internal("Failed to create token".to_string())
    })
}

/// Check if user has sufficient role level.
/// Role hierarchy: member < admin < account_owner < agency_admin
pub fn require_role(actual: &str, minimum: &str) -> bool {
    let levels = ["member", "admin", "account_owner", "agency_admin"];

    let actual_idx = levels.iter().position(|&r| r == actual).unwrap_or(0);
    let min_idx = levels.iter().position(|&r| r == minimum).unwrap_or(0);

    actual_idx >= min_idx
}

/// Is this role an account administrator?
///
/// The platform carries three role vocabularies: signup writes `owner` (all 15
/// owner rows in the DB), older claims/membership code used `account_owner` and
/// `admin`, and the platform-operations role is `agency_admin`. Handler code that
/// means "this tenant's owner, or a platform operator" must accept ALL of them.
/// Comparing against a single pair is not a style choice — it silently 403s every
/// real account owner, which is what `native_apps::connect_app` was doing (its
/// admin-only connectors were unreachable for owners AND for the platform admin,
/// whose role is `agency_admin`).
pub fn is_account_admin(role: &str) -> bool {
    matches!(role, "owner" | "account_owner" | "admin" | "agency_admin")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool that can never connect. The lazy pool spawns its maintenance task at construction,
    /// so it must be built with a runtime entered (see the `live-error-path-proof` skill).
    fn dead_pool(rt: &tokio::runtime::Runtime) -> sqlx::PgPool {
        let _entered = rt.enter();
        sqlx::postgres::PgPoolOptions::new()
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://probe:probe@127.0.0.1:1/none")
            .expect("lazy pool")
    }

    #[test]
    fn a_claim_that_is_not_a_workspace_id_is_unauthorized() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let db = dead_pool(&rt);
        // No connection is needed: the claim is rejected before the query is built.
        let err = rt
            .block_on(workspace_exists(&db, "not-a-uuid"))
            .expect_err("a non-uuid claim must not be treated as a workspace");
        assert!(matches!(err, AppError::Unauthorized), "got {err:?}");
    }

    #[test]
    fn an_unreachable_database_is_never_reported_as_a_missing_workspace() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let db = dead_pool(&rt);
        // The negative control for the check the middleware runs: a broken database must be an
        // error (500), NOT Ok(false) — otherwise an outage would sign every user out.
        let err = rt
            .block_on(workspace_exists(
                &db,
                "b7ab7492-c400-4000-8000-0000deadbeef",
            ))
            .expect_err("an unreachable database must not answer 'workspace is gone'");
        assert!(matches!(err, AppError::Database(_)), "got {err:?}");
    }
}
