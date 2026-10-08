//! Profile handlers — read/rename the caller's own user row, change its password, and carry the
//! caller's own profile PICTURE.
//!
//! Everything here is scoped to the TOKEN SUBJECT (`claims.sub` = users.id). No id is ever taken
//! from the request, so one tenant can never edit another's user, and a platform admin gets no
//! extra reach through these routes. The ONE anonymous read (`serve_avatar`) is keyed by the
//! unguessable user uuid and returns nothing but the picture that user uploaded — the same
//! accept-store-serve path as the tenant email logo, see [`crate::image_store`].
//!
//! Routes:
//! * `GET  /api/profile`            — the caller's own record, incl. the REAL `plan_name`
//! * `PUT  /api/profile`            — `{name?, username?, company?}`; blank name refused 4xx
//! * `PUT  /api/profile/password`   — `{current_password, new_password}`; wrong current -> 401
//! * `POST /api/profile/avatar`     — raw image bytes (magic-byte sniffed, <=2 MB), stored per user
//! * `GET  /api/avatars/:user_id`   — PUBLIC: serve the stored bytes (mounted in main.rs)

use axum::{
    extract::{Extension, Multipart, Path, State},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::handlers::{hash_password, verify_password};
use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Same floor the register flow enforces (`auth::handlers::register`), so a password that could
/// never have been created cannot be set through this route either.
const MIN_PASSWORD_LEN: usize = 8;
const MAX_NAME_LEN: usize = 100;
/// `users.username` / `users.company` are varchar(100) / varchar(255) (migration 118); refuse an
/// over-long value here so it is a clean 4xx instead of a database error surfacing as a 500.
const MAX_USERNAME_LEN: usize = 100;
const MAX_COMPANY_LEN: usize = 255;
/// `webhook::actions` creates invited users with this literal; it is not a verifiable hash.
const PLACEHOLDER_HASH: &str = "PLACEHOLDER_HASH";

#[derive(Debug, serde::Deserialize)]
pub struct UpdateProfileRequest {
    pub name: Option<String>,
    pub username: Option<String>,
    pub company: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ProfileRow {
    id: Uuid,
    tenant_id: Uuid,
    name: String,
    email: String,
    role: String,
    username: Option<String>,
    company: Option<String>,
    avatar_url: Option<String>,
}

fn subject(claims: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)
}

/// The caller-facing shape of the profile record.
fn profile_json(row: &ProfileRow, plan_name: Option<String>) -> Value {
    json!({
        "id": row.id,
        "tenant_id": row.tenant_id,
        "name": row.name,
        "email": row.email,
        "role": row.role,
        "username": row.username,
        "company": row.company,
        "avatar_url": row.avatar_url,
        "plan_name": plan_name,
    })
}

/// The tenant's OWN plan name — the REAL level, never a guess.
///
/// Mirrors the resolution `GET /api/billing/subscription` already answers to this same console:
/// the tenant's `tenant_plans` row joined to its `plans` row, and the `free` plan when the tenant
/// holds no subscription row at all (a tenant with no row really is on free — that is the route's
/// own declared default, not a fallback invented here).
async fn resolve_plan_name(db: &sqlx::PgPool, tenant_id: Uuid) -> Result<Option<String>, AppError> {
    let name: Option<String> = sqlx::query_scalar(
        "SELECT p.name FROM tenant_plans tp \
         JOIN plans p ON p.id = tp.plan_id \
         WHERE tp.tenant_id = $1 \
         ORDER BY tp.created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await?;
    if name.is_some() {
        return Ok(name);
    }
    let free: Option<String> =
        sqlx::query_scalar("SELECT name FROM plans WHERE slug = 'free' LIMIT 1")
            .fetch_optional(db)
            .await?;
    Ok(free)
}

/// GET /api/profile — the caller's own profile, incl. the real plan name and picture URL.
pub async fn get_profile(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let id = subject(&c)?;
    let row = sqlx::query_as::<_, ProfileRow>(
        "SELECT id, tenant_id, name, email, role, username, company, avatar_url \
         FROM users WHERE id = $1 AND is_active = true",
    )
    .bind(id)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Profile not found".to_string()))?;

    let plan_name = resolve_plan_name(&s.db, row.tenant_id).await?;
    Ok(Json(profile_json(&row, plan_name)))
}

/// PUT /api/profile — update the caller's own profile.
///
/// `name` is required and may not be blank (4xx). `username` and `company` are OPTIONAL: an absent
/// key leaves the stored value untouched, an empty string CLEARS it (the console's Company field
/// is clearable), so a rename never silently wipes the other two fields.
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

    let username = req.username.as_deref().map(str::trim);
    if let Some(u) = username {
        if u.chars().count() > MAX_USERNAME_LEN {
            return Err(AppError::Validation(format!(
                "username must be {} characters or fewer",
                MAX_USERNAME_LEN
            )));
        }
    }
    let company = req.company.as_deref().map(str::trim);
    if let Some(co) = company {
        if co.chars().count() > MAX_COMPANY_LEN {
            return Err(AppError::Validation(format!(
                "company must be {} characters or fewer",
                MAX_COMPANY_LEN
            )));
        }
    }

    let row = sqlx::query_as::<_, ProfileRow>(
        "UPDATE users SET \
           name = $2, \
           username = CASE WHEN $3 THEN NULLIF($4, '') ELSE username END, \
           company  = CASE WHEN $5 THEN NULLIF($6, '') ELSE company  END, \
           updated_at = NOW() \
         WHERE id = $1 AND is_active = true \
         RETURNING id, tenant_id, name, email, role, username, company, avatar_url",
    )
    .bind(id)
    .bind(name)
    .bind(username.is_some())
    .bind(username.unwrap_or(""))
    .bind(company.is_some())
    .bind(company.unwrap_or(""))
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Profile not found".to_string()))?;

    Ok(Json(
        json!({ "message": "Profile updated", "profile": profile_json(&row, None) }),
    ))
}

/// PUT /api/profile/password — change the caller's own password.
///
/// The current password is required and verified against the stored argon2 hash: a stolen session
/// token must not be enough to lock the real owner out of the account. A WRONG password answers
/// 401 "Current password is incorrect" (the sentence the form shows); it does not say whether the
/// account exists. A too-short or unchanged new password is a 422.
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
        return Err(AppError::UnauthorizedReason(
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

/// POST /api/profile/avatar — accept the caller's own picture, store it, and point
/// `users.avatar_url` at the user-scoped serve URL.
///
/// The URL is version-stamped (`?v=<epoch>`) so a browser keys on the bytes rather than on a
/// cached 200. One writer per field: this touches only `user_avatars` and `users.avatar_url`.
pub async fn upload_avatar(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<Json<Value>> {
    let id = subject(&c)?;
    let (content_type, bytes) = crate::image_store::read_uploaded_image(&mut multipart).await?;
    let byte_len = bytes.len();

    sqlx::query(
        "INSERT INTO user_avatars (user_id, content_type, bytes, updated_at) \
         VALUES ($1, $2, $3, NOW()) \
         ON CONFLICT (user_id) DO UPDATE SET \
           content_type = EXCLUDED.content_type, \
           bytes = EXCLUDED.bytes, \
           updated_at = NOW()",
    )
    .bind(id)
    .bind(&content_type)
    .bind(&bytes)
    .execute(&s.db)
    .await?;

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let avatar_url = format!("/api/avatars/{id}?v={epoch}");

    sqlx::query("UPDATE users SET avatar_url = $2, updated_at = NOW() WHERE id = $1")
        .bind(id)
        .bind(&avatar_url)
        .execute(&s.db)
        .await?;

    Ok(Json(json!({
        "avatar_url": avatar_url,
        "content_type": content_type,
        "bytes": byte_len,
    })))
}

/// `GET /api/avatars/:user_id` — PUBLIC: serve the stored bytes, or 404.
///
/// Deliberately reads nothing else: no name, no email, no existence of the user beyond whether a
/// picture row exists. The user id is an unguessable uuid. Mounted OUTSIDE the auth middleware in
/// main.rs because an `<img src>` carries no bearer token (see auth::route_policy [PUBLIC_ROUTES]).
pub async fn serve_avatar(
    State(s): State<AppState>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Response> {
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT content_type, bytes FROM user_avatars WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&s.db)
            .await?;
    match row {
        Some((content_type, bytes)) => crate::image_store::image_response(content_type, bytes),
        None => Err(AppError::NotFound(
            "No profile picture for this user".into(),
        )),
    }
}
