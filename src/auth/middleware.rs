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

    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
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
