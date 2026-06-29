//! Auth middleware: JWT verification and tenant extraction.
//!
//! Provides `auth_middleware` for protected routes and utility functions
//! for token creation and role checking.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::AppState;
use crate::errors::AppError;
use super::models::Claims;

/// Auth middleware that extracts user context from JWT bearer token.
///
/// For routes that require authentication, attach via
/// `axum::middleware::from_fn_with_state(state, auth_middleware)`.
///
/// Skips auth for internal sync endpoints (validated by x-internal-key header).
///
/// # Errors
///
/// Returns `401 Unauthorized` if the token is missing, malformed, or expired.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    // Skip auth for internal sync routes — validated by x-internal-key header
    let path = req.uri().path();
    if path.ends_with("/internal") {
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
    req.extensions_mut().insert(claims);
    Ok(next.run(req).await)
}

/// Verify a JWT token and return the claims.
pub fn verify_token(token: &str, secret: &str) -> Result<Claims, AppError> {
    use jsonwebtoken::{decode, DecodingKey, Validation, Algorithm};

    let decoding_key = DecodingKey::from_secret(secret.as_bytes());
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = 30; // 30-second clock skew tolerance
    validation.validate_exp = true;

    let token_data = decode::<Claims>(token, &decoding_key, &validation)
        .map_err(|e| {
            tracing::warn!(error = %e, "JWT verification failed");
            AppError::Unauthorized
        })?;

    Ok(token_data.claims)
}

/// Create a JWT access token.
pub fn create_access_token(claims: &Claims, secret: &str) -> Result<String, AppError> {
    use jsonwebtoken::{encode, Header, EncodingKey};

    let encoding_key = EncodingKey::from_secret(secret.as_bytes());
    encode(&Header::default(), claims, &encoding_key)
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to create JWT");
            AppError::Internal("Failed to create token".to_string())
        })
}

/// Check if user has sufficient role level.
///
/// Role hierarchy: user < team_member < client_admin < agency_admin
pub fn require_role(actual: &str, minimum: &str) -> bool {
    let levels = ["user", "team_member", "client_admin", "agency_admin"];

    let actual_idx = levels.iter().position(|&r| r == actual).unwrap_or(0);
    let min_idx = levels.iter().position(|&r| r == minimum).unwrap_or(0);

    actual_idx >= min_idx
}
