//! Domain error types and HTTP response helpers for the API.
//!
//! Single `AppError` enum with proper HTTP status code mapping.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Domain error type for the CRM application.
/// Each variant maps to an appropriate HTTP status code via `IntoResponse`.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 401 — Authentication required or token expired
    #[error("Authentication required")]
    Unauthorized,

    /// 401 — Bad email/password combination
    #[error("Invalid credentials")]
    InvalidCredentials,

    /// 403 — User lacks permission
    #[error("Forbidden: insufficient permissions")]
    Forbidden,

    /// 404 — Resource not found
    #[error("Resource not found: {0}")]
    NotFound(String),

    /// 409 — Resource already exists (duplicate)
    #[error("Resource already exists: {0}")]
    Duplicate(String),

    /// 422 — Input validation failed
    #[error("Validation error: {0}")]
    Validation(String),

    /// 400 — Bad request (malformed input)
    #[error("{0}")]
    BadRequest(String),

    /// 500 — Database error
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    /// 500 — Redis cache error
    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    /// 401 — JWT token error
    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    /// 500 — Password hashing error
    #[error("Hashing error: {0}")]
    Hash(String),

    /// 500 — Internal server error (catch-all)
    #[error("Internal server error: {0}")]
    Internal(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "Authentication required".to_string()),
            AppError::InvalidCredentials => (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Insufficient permissions".to_string()),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, msg.clone()),
            AppError::Duplicate(msg) => (StatusCode::CONFLICT, msg.clone()),
            AppError::Validation(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg.clone()),
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::Hash(msg) => {
                tracing::error!("Password hashing error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, "Password hashing error".to_string())
            }
            AppError::Internal(msg) => {
                tracing::error!("Internal error: {}", msg);
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal server error".to_string())
            }
            AppError::Database(e) => {
                tracing::error!(error = %e, "Database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Database error".to_string())
            }
            AppError::Redis(e) => {
                tracing::error!(error = %e, "Redis error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Cache error".to_string())
            }
            AppError::Jwt(e) => {
                tracing::warn!(error = %e, "JWT error");
                (StatusCode::UNAUTHORIZED, "Invalid token".to_string())
            }
        };

        let body = Json(json!({
            "error": true,
            "message": message,
            "code": status.as_u16()
        }));

        (status, body).into_response()
    }
}

/// Helper type alias for API results.
pub type ApiResult<T> = Result<T, AppError>;

/// Validates pagination parameters, returning `(page, per_page)` with defaults.
/// - `page` defaults to 1
/// - `per_page` defaults to 50, max 100
pub fn validate_pagination(page: Option<i64>, per_page: Option<i64>) -> (i64, i64) {
    let page = page.unwrap_or(1).max(1);
    let per_page = per_page.unwrap_or(50).clamp(1, 100);
    (page, per_page)
}
