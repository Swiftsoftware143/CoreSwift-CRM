//! Profile handlers — read/rename the caller's own user row, and change its password.
//!
//! Everything here is scoped to the TOKEN SUBJECT (`claims.sub` = users.id). No id is ever taken
//! from the request, so one tenant can never edit another's user, and a platform admin gets no
//! extra reach through these routes.

use axum::{
    extract::{Extension, State},
    response::IntoResponse,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::handlers::{hash_password, verify_password};
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Same floor the register flow enforces (`auth::handlers::register`), so a password that could
/// never have been created cannot be set through this route either.
const MIN_PASSWORD_LEN: usize = 8;
const MAX_NAME_LEN: usize = 100;
/// `webhook::actions` creates invited users with this literal; it is not a verifiable hash.
const PLACEHOLDER_HASH: &str = "PLACEHOLDER_HASH";

#[derive(Debug, serde::Deserialize)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
struct ProfileRow {
    id: Uuid,
    tenant_id: Uuid,
    name: String,
    email: String,
    role: String,
}

fn subject(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)
}

/// GET /api/profile — the caller's own profile.
pub async fn get_profile(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let id = subject(&c)?;
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT id, tenant_id, name, email, role FROM users WHERE id = $1 AND is_active = true",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Profile not found".to_string()))?;

    Ok(Json(json!(row)))
}

/// PUT /api/profile — rename the caller's own profile. `{"name": "..."}` is the whole contract the
/// Profile tab sends; anything else in the body is ignored rather than silently written.
pub async fn update_profile(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<UpdateProfileRequest>,
) -> ApiResult<impl IntoResponse> {
    let id = subject(&c)?;
    let name = req.name.as_deref().map(str::trim).unwrap_or("");
    if name.is_empty() {
        return Err(AppError::Validation(
            "name is required and cannot be empty".to_string(),
        ));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(AppError::Validation(format!(
            "name must be {} characters or fewer",
            MAX_NAME_LEN
        )));
    }

    let row = sqlx::query_as::<_, ProfileRow>(
        "UPDATE users SET name = $2, updated_at = NOW() \
         WHERE id = $1 AND is_active = true \
         RETURNING id, tenant_id, name, email, role",
    )
    .bind(id)
    .bind(name)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Profile not found".to_string()))?;

    Ok(Json(
        json!({ "message": "Profile updated", "profile": row }),
    ))
}

/// PUT /api/profile/password — change the caller's own password.
///
/// The current password is required and verified against the stored argon2 hash: a stolen session
/// token must not be enough to lock the real owner out of the account. A wrong password answers
/// 400 with a message the form can show; it does not say whether the account exists.
pub async fn change_password(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<ChangePasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    let id = subject(&c)?;

    if req.new_password.chars().count() < MIN_PASSWORD_LEN {
        return Err(AppError::Validation(format!(
            "Password must be at least {} characters",
            MIN_PASSWORD_LEN
        )));
    }
    if req.new_password == req.current_password {
        return Err(AppError::Validation(
            "New password must be different from the current password".to_string(),
        ));
    }

    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND is_active = true")
            .bind(id)
            .fetch_optional(&s.db)
            .await?;
    let stored = stored.ok_or(AppError::Unauthorized)?;

    // Invited-but-never-registered users carry a literal placeholder; refusing beats accepting a
    // "current password" that is really the placeholder text.
    if stored == PLACEHOLDER_HASH {
        return Err(AppError::BadRequest(
            "This account has no password set yet — finish the invite flow first.".to_string(),
        ));
    }

    if !verify_password(&req.current_password, &stored)? {
        return Err(AppError::BadRequest(
            "Current password is incorrect".to_string(),
        ));
    }

    let new_hash = hash_password(&req.new_password)?;
    sqlx::query(
        "UPDATE users SET password_hash = $2, password_changed_at = NOW(), updated_at = NOW() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&new_hash)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({ "message": "Password updated" })))
}
