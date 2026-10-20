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
    // 401 into "Session expired" + sign-out (`www-app/coreswift/index.html`, the 401 arm of its
    // `api()` wrapper — the tenant shell the vhost actually serves).
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

    // A signature-valid token is also NOT proof that the USER behind it still exists (kanban
    // t_53fcb666, fleet-wide). `sub` names a `users` row and everything above is built from the
    // claims alone, so deleting an account left a working credential for the rest of its life —
    // measured on a sibling app 2026-10-20: after `delete from users` + `delete from tenants`, the
    // same token still answered 200 from `/api/v1/auth/me` AND from a platform-admin read. Deleting
    // the row is how the fleet remediates a compromised account and how its probe sweep cleans up
    // throwaways, so deletion has to BE the revocation. One primary-key read, for every role:
    // a missing row, a disabled row and a `sub` that is not a uuid all answer 401 exactly like an
    // expired token (the SPA turns 401 into "Session expired" + sign-out). A database that cannot
    // answer is a 500 from `?`, never a silent pass.
    match user_session_state(&state.db, &claims.sub).await? {
        Some((true, changed_at)) => {
            // SESSION REVOCATION (kanban t_80787a0f): this app already stamps
            // `users.password_changed_at` on both the change and the reset, but nothing read it —
            // so a token minted before the change kept answering 200 afterwards. A session whose
            // `iat` predates it is refused here, in the SAME read, with the same 401 a deleted row
            // gets. NULL (never changed) changes nothing.
            if session_predates_password_change(Some(claims.iat as i64), changed_at) {
                tracing::warn!(
                    user = %claims.sub,
                    "rejected a token minted before the account's password change"
                );
                return Err(AppError::Unauthorized);
            }
        }
        _ => {
            tracing::warn!(
                user = %claims.sub,
                "rejected a token whose user no longer exists or is disabled"
            );
            return Err(AppError::Unauthorized);
        }
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

/// Is the USER named by a token's `sub` claim still a live, enabled row in `users`?
///
/// The sibling of [`workspace_exists`], and the half that was missing: a workspace can be alive
/// while the account that signed in has been deleted or disabled (kanban t_53fcb666). `Ok(false)`
/// for a `sub` that is not even a uuid — such a claim names no row. `Err(AppError::Database)` when
/// the database cannot answer, so an outage is never reported as "your account is gone".
/// What the `users` row named by a token's `sub` still grants: `(is_active, password_changed_at)`,
/// or `None` when no such row exists.
///
/// The existence half is kanban t_53fcb666; the timestamp half is kanban t_80787a0f, which this
/// app already had data for (`password_changed_at` is stamped by BOTH the change and the reset
/// writer) but never read. `password_changed_at` arrives as a unix epoch (`None` = never changed,
/// so no session is restricted).
pub async fn user_session_state(
    db: &sqlx::PgPool,
    sub: &str,
) -> Result<Option<(bool, Option<i64>)>, AppError> {
    let Ok(user_id) = uuid::Uuid::parse_str(sub) else {
        return Ok(None);
    };

    let row: Option<(bool, Option<i64>)> = sqlx::query_as(
        "SELECT is_active, floor(EXTRACT(EPOCH FROM password_changed_at))::bigint
           FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await?;

    Ok(row)
}

/// Does a session minted at `jwt_iat` predate the instant its account last changed its password?
/// (kanban t_80787a0f)
///
/// Pure, so both arms are unit-tested. `changed_at` is `users.password_changed_at` as a unix epoch
/// (`None` = never changed, which is every account until it rotates its password, so every token
/// already in the wild keeps working). A session with no `iat` cannot prove it is newer than the
/// change, and the safe answer for a credential is "cannot prove → refuse". `iat` is second-granularity (and the change's own timestamp is floored to a second for this reason), so a session minted in the SAME second as the change cannot be told apart from a re-login and is allowed.
pub fn session_predates_password_change(jwt_iat: Option<i64>, changed_at: Option<i64>) -> bool {
    match changed_at {
        None => false,
        Some(v) => match jwt_iat {
            None => true,
            Some(iat) => iat < v,
        },
    }
}

/// Refuse a credential whose session predates its account's last password change, for the handlers
/// that parse the bearer THEMSELVES instead of running `auth_middleware` (`GET /api/auth/me`,
/// `GET /api/auth/invites`). Kanban t_80787a0f: measured on the live app after the boundary arm
/// shipped — `GET /api/tags` answered 401 for a token minted before the password change while
/// `GET /api/auth/me` still answered 200, because its own check only asked "does the row exist".
/// A revoked session must not be able to read its own profile through a side door.
pub async fn refuse_revoked_session(db: &sqlx::PgPool, claims: &Claims) -> Result<(), AppError> {
    match user_session_state(db, &claims.sub).await? {
        Some((true, changed_at)) => {
            if session_predates_password_change(Some(claims.iat as i64), changed_at) {
                tracing::warn!(
                    user = %claims.sub,
                    "rejected a token minted before the account's password change"
                );
                return Err(AppError::Unauthorized);
            }
            Ok(())
        }
        _ => {
            tracing::warn!(
                user = %claims.sub,
                "rejected a token whose user no longer exists or is disabled"
            );
            Err(AppError::Unauthorized)
        }
    }
}

pub async fn user_is_live(db: &sqlx::PgPool, sub: &str) -> Result<bool, AppError> {
    let Ok(user_id) = uuid::Uuid::parse_str(sub) else {
        return Ok(false);
    };

    let active: Option<bool> = sqlx::query_scalar("SELECT is_active FROM users WHERE id = $1")
        .bind(user_id)
        .fetch_optional(db)
        .await?;

    Ok(active == Some(true))
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

    /// The reported defect (kanban t_80787a0f): this app already stamped
    /// `users.password_changed_at` on both the change and the reset, but no boundary read it, so a
    /// token minted before the change kept answering 200. The rule is one comparison, and it must
    /// change nothing else: NULL (never changed), a session minted at or after the change, and a
    /// session with no `iat` on an unchanged account all stay exactly as they were.
    #[test]
    fn a_session_minted_before_the_password_change_is_refused_and_nothing_else_is() {
        // NULL column: never restricted, so every token already in the wild keeps working.
        assert!(!session_predates_password_change(Some(1_000), None));
        // Minted BEFORE the change -> refused.
        assert!(session_predates_password_change(Some(1_000), Some(2_000)));
        // Minted AT the change or after it -> allowed (a re-login must work).
        assert!(!session_predates_password_change(Some(2_000), Some(2_000)));
        assert!(!session_predates_password_change(Some(3_000), Some(2_000)));
        // A session that cannot prove it is newer is refused rather than trusted.
        assert!(session_predates_password_change(None, Some(2_000)));
        assert!(!session_predates_password_change(None, None));
    }
}
