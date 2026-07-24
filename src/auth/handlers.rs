//! Auth handlers: register, login, refresh, me, logout.
//!
//! All handlers are tenant-scoped. On register, a new tenant can be created
//! or an existing tenant slug can be specified.

use axum::{
    extract::{State, Json, Request, Extension},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;
use chrono::Utc;

use argon2::{Argon2, PasswordHasher};
use password_hash::SaltString;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use super::models::*;
use super::middleware;

/// POST /api/auth/register — Create a new account.
/// Every signup creates their own isolated tenant (account).
/// Admins and tenants are both full account holders — no distinction.
/// Provide account_name/slug to customize, or one is auto-generated from email.
/// Provide invite_token to join an existing tenant as a team member.
pub async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validate input
    if req.email.is_empty() || req.password.is_empty() || req.name.is_empty() {
        return Err(AppError::Validation(
            "Name, email, and password are required".to_string(),
        ));
    }
    if req.password.len() < 8 {
        return Err(AppError::Validation(
            "Password must be at least 8 characters".to_string(),
        ));
    }
    if !req.email.contains('@') {
        return Err(AppError::Validation("Invalid email format".to_string()));
    }

    // Determine tenant
    let tenant_id = resolve_account(&state, &req).await?;

    // Check for duplicate user
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND email = $2",
    )
    .bind(tenant_id)
    .bind(&req.email)
    .fetch_one(&state.db)
    .await?;

    if existing > 0 {
        return Err(AppError::Duplicate(format!(
            "User with email '{}' already exists in this tenant",
            req.email
        )));
    }

    // Hash password
    let password_hash = hash_password(&req.password)?;

    // Create user as admin (first user in tenant gets owner role)
    let is_first_user = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?
        == 0;

    let role = if is_first_user { "owner" } else { "member" };

    let user = sqlx::query_as::<_, TeamMember>(
        r#"INSERT INTO users (id, tenant_id, email, password_hash, name, role)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&req.email)
    .bind(&password_hash)
    .bind(&req.name)
    .bind(role)
    .fetch_one(&state.db)
    .await?;

    // Fetch tenant info
    let tenant = sqlx::query_as::<_, crate::account::models::Account>(
        "SELECT id, name, slug, logo_url, primary_color, accent_color, custom_domain, settings, is_active, created_at, updated_at FROM tenants WHERE id = $1"
    )
    .bind(tenant_id)
    .fetch_one(&state.db)
    .await?;

    // Generate tokens
    let (access_token, refresh_token, expires_in) = generate_tokens(&user, &state)?;


    // Send welcome email via template system
    let vars = json!({
        "name": &req.name,
        "email": &req.email,
        "password": &req.password,
        "account_name": &tenant.name,
        "app_url": "https://app.coreswiftcrm.com",
    });
    let _ = crate::email::send_template_email(
        &state.db,
        tenant_id,
        &req.email,
        "welcome",
        &vars,
    )
    .await
    .map_err(|e| { tracing::warn!(error = %e, "Welcome email via template failed"); e })
    .ok();
    let mut next_steps = vec![
        "Connect your apps — POST /api/native/apps/{slug}/connect".to_string(),
        "Create contacts — POST /api/contacts".to_string(),
        "Set up pipelines — POST /api/pipelines".to_string(),
    ];
    if is_first_user {
        next_steps.insert(0, format!(
            "Invite team members — use your tenant slug: '{}'", tenant.slug
        ));
    }

    Ok((
        StatusCode::CREATED,
        Json(json!(RegisterResponse {
            access_token,
            refresh_token,
            token_type: "Bearer".to_string(),
            expires_in,
            team_member: user.into(),
            account: AccountResponse {
                id: tenant.id,
                name: tenant.name,
                slug: tenant.slug,
                is_active: tenant.is_active,
            },
            next_steps,
        })),
    ))
}

/// POST /api/auth/login — Authenticate and get tokens.
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<impl IntoResponse> {
    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE email = $1 AND is_active = true",
    )
    .bind(&req.email)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::InvalidCredentials)?;

    if !verify_password(&req.password, &user.password_hash)? {
        return Err(AppError::InvalidCredentials);
    }

    // Update last login
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(&state.db)
        .await?;

    let (access_token, refresh_token, expires_in) = generate_tokens(&user, &state)?;

    Ok(Json(json!(TokenResponse {
        access_token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in,
        team_member: user.into(),
    })))
}

/// POST /api/auth/refresh — Exchange refresh token for new access token.
pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<impl IntoResponse> {
    let claims = middleware::verify_token(&req.refresh_token, &state.config.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE id = $1 AND is_active = true",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    let (access_token, _, expires_in) = generate_tokens(&user, &state)?;

    Ok(Json(json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": expires_in,
    })))
}

/// GET /api/auth/me — Get current user profile.
pub async fn me(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<impl IntoResponse> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    let claims = middleware::verify_token(token, &state.config.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| AppError::Unauthorized)?;

    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE id = $1 AND is_active = true",
    )
    .bind(user_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::Unauthorized)?;

    Ok(Json(json!({
        "team_member": TeamMemberResponse::from(user),
    })))
}

/// POST /api/auth/invite — Owner/admin creates an invite link for their tenant.
/// Auth middleware injects Claims as Extension.
pub async fn create_invite(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateInviteRequest>,
) -> ApiResult<impl IntoResponse> {
    if claims.role != "owner" && claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let token = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO tenant_invites (id, tenant_id, token, role, expires_at) VALUES ($1, $2, $3, $4, NOW() + INTERVAL '7 days')"
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&token)
    .bind(&req.role)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "invite_token": token,
        "invite_url": format!("/auth/register?invite_token={}", token),
        "expires_in_days": 7,
        "role": req.role,
    })))
}

/// GET /api/auth/invites — List active invites for the tenant.
pub async fn list_invites(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<impl IntoResponse> {
    let claims = extract_claims(&request, &state)?;
    if claims.role != "owner" && claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let invites = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT id, token, role, accepted, expires_at, created_at FROM tenant_invites WHERE tenant_id = $1 ORDER BY created_at DESC"
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({"invites": invites})))
}

/// POST /api/auth/logout — Invalidate tokens.
pub async fn logout(
    State(state): State<AppState>,
    request: Request,
) -> ApiResult<impl IntoResponse> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    let claims = middleware::verify_token(token, &state.config.jwt_secret)?;

    // Blacklist token in Redis for remaining expiry
    let now = Utc::now().timestamp() as usize;
    if claims.exp > now {
        let ttl = claims.exp - now;
        let mut conn = state.redis.clone();
        let _: Result<(), _> = redis::cmd("SET")
            .arg(&[format!("blacklist:{}", token)])
            .arg("1")
            .arg("EX")
            .arg(ttl)
            .query_async(&mut conn)
            .await;
    }

    Ok(Json(json!({"message": "Logged out successfully"})))
}

// ========== Private helpers ==========

/// Resolve the tenant for registration — create new account or join via invite.
/// Every person gets their own isolated tenant (account).
/// If no account_name/slug provided, auto-generates one from email.
async fn resolve_account(
    state: &AppState,
    req: &RegisterRequest,
) -> Result<Uuid, AppError> {
    // If invite token provided, look up the invite and join that tenant
    if let Some(token) = &req.invite_token {
        let invite = sqlx::query_as::<_, (Uuid,)>(
            "SELECT tenant_id FROM tenant_invites WHERE token = $1 AND accepted = false AND expires_at > NOW()"
        )
        .bind(token)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid or expired invite token".into()))?;

        // Mark invite as accepted
        sqlx::query("UPDATE tenant_invites SET accepted = true, accepted_at = NOW() WHERE token = $1")
            .bind(token)
            .execute(&state.db)
            .await?;

        return Ok(invite.0);
    }

    if let (Some(name), Some(slug)) = (&req.account_name, &req.account_slug) {
        let tenant = sqlx::query_as::<_, crate::account::models::Account>(
            r#"INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) RETURNING *"#,
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(slug)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref dbe) = e {
                if dbe.constraint() == Some("tenants_slug_key") {
                    return AppError::Duplicate(format!("Tenant slug '{}' already exists", slug));
                }
            }
            AppError::Database(e)
        })?;
        // Auto-assign Free Plan to new tenant
        {
            let free_plan_id = uuid::Uuid::parse_str("ebbdca8c-6ad7-48cb-b580-d321b536671a")
                .map_err(|_| AppError::BadRequest("Invalid plan UUID".into()))?;
            let _ = sqlx::query(
                r#"INSERT INTO tenant_plans (tenant_id, plan_id, status, billing_cycle)
                   VALUES ($1, $2, 'active', 'monthly')
                   ON CONFLICT (tenant_id) DO NOTHING"#
            )
            .bind(tenant.id)
            .bind(free_plan_id)
            .execute(&state.db)
            .await;
        }
        Ok(tenant.id)
    } else {
        // Auto-generate tenant from email — each admin gets their own tenant
        let local_part = req.email.split('@').next().unwrap_or("user");
        let slug = format!("{}-{}", local_part, &uuid::Uuid::new_v4().to_string()[..8]);
        let name = format!("{}'s Workspace", req.name);

        let tenant = sqlx::query_as::<_, crate::account::models::Account>(
            r#"INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3) RETURNING *"#,
        )
        .bind(Uuid::new_v4())
        .bind(&name)
        .bind(&slug)
        .fetch_one(&state.db)
        .await
        .map_err(|e| {
            AppError::Database(e)
        })?;
        // Auto-assign Free Plan to new tenant
        {
            let free_plan_id = uuid::Uuid::parse_str("ebbdca8c-6ad7-48cb-b580-d321b536671a")
                .map_err(|_| AppError::BadRequest("Invalid plan UUID".into()))?;
            let _ = sqlx::query(
                r#"INSERT INTO tenant_plans (tenant_id, plan_id, status, billing_cycle)
                   VALUES ($1, $2, 'active', 'monthly')
                   ON CONFLICT (tenant_id) DO NOTHING"#
            )
            .bind(tenant.id)
            .bind(free_plan_id)
            .execute(&state.db)
            .await;
        }
        Ok(tenant.id)
    }
}

/// Extract JWT claims from an Authorization header.
fn extract_claims(request: &Request, state: &AppState) -> Result<Claims, AppError> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    middleware::verify_token(token, &state.config.jwt_secret)
}

/// Hash a password using argon2.
fn hash_password(password: &str) -> Result<String, AppError> {
    use argon2::{
        password_hash::{SaltString, PasswordHasher},
        Argon2,
    };

    let salt = SaltString::generate(&mut rand::thread_rng());
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| AppError::Hash(format!("Failed to hash password: {}", e)))?;

    Ok(hash.to_string())
}

/// Verify a password against the stored argon2 hash.
fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
    use argon2::{
        password_hash::{PasswordHash, PasswordVerifier},
        Argon2,
    };

    let parsed_hash = PasswordHash::new(hash)
        .map_err(|e| AppError::Hash(format!("Invalid password hash format: {}", e)))?;

    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok())
}

/// Generate access and refresh tokens for a user.
fn generate_tokens(user: &TeamMember, state: &AppState) -> Result<(String, String, i64), AppError> {
    let now = Utc::now().timestamp() as usize;
    let access_exp = now + state.config.jwt_access_expiry as usize;
    let refresh_exp = now + state.config.jwt_refresh_expiry as usize;

    let access_claims = Claims {
        sub: user.id.to_string(),
        aid: user.tenant_id.to_string(),
        role: user.role.clone(),
        exp: access_exp,
        iat: now,
        aud: Some("coreswift-api".to_string()),
        iss: Some("coreswift".to_string()),
    };
    let access_token = middleware::create_access_token(&access_claims, &state.config.jwt_secret)?;

    let refresh_claims = Claims {
        sub: user.id.to_string(),
        aid: user.tenant_id.to_string(),
        role: user.role.clone(),
        exp: refresh_exp,
        iat: now,
        aud: Some("coreswift-api".to_string()),
        iss: Some("coreswift".to_string()),
    };
    let refresh_token = middleware::create_access_token(&refresh_claims, &state.config.jwt_secret)?;

    Ok((access_token, refresh_token, state.config.jwt_access_expiry))
}

/// POST /api/auth/forgot-password — Send password reset email
#[derive(serde::Deserialize)]
pub struct ForgotPasswordRequest {
    pub email: String,
}

pub async fn forgot_password(
    State(state): State<AppState>,
    Json(req): Json<ForgotPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.email.is_empty() || !req.email.contains('@') {
        return Err(AppError::Validation("Valid email is required".to_string()));
    }

    // Look up user
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, name, email FROM users WHERE email = $1"
    )
    .bind(&req.email)
    .fetch_optional(&state.db)
    .await?;

    let user = match user {
        Some(u) => u,
        None => {
            // Don't reveal whether email exists — return success either way
            return Ok(Json(json!({"message": "If that email is registered, a reset link has been sent."})));
        }
    };

    // Create reset token (expires in 1 hour)
    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::hours(1);

    sqlx::query(
        "INSERT INTO password_resets (id, user_id, token, expires_at) VALUES ($1, $2, $3, $4)"
    )
    .bind(Uuid::new_v4())
    .bind(user.id)
    .bind(&token)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    // Send reset email via template system
    let vars = json!({
        "name": user.name,
        "token": token,
        "app_url": "https://app.coreswiftcrm.com",
    });

    let _ = crate::email::send_template_email(
        &state.db,
        Uuid::nil(),
        &req.email,
        "password_reset",
        &vars,
    )
    .await
    .map_err(|e| { tracing::warn!(error = %e, "Failed to send password reset email via template"); e })
    .ok();

    Ok(Json(json!({"message": "If that email is registered, a reset link has been sent."})))
}

/// POST /api/auth/reset-password — Reset password using token
#[derive(serde::Deserialize)]
pub struct ResetPasswordRequest {
    pub token: String,
    pub password: String,
}

pub async fn reset_password(
    State(state): State<AppState>,
    Json(req): Json<ResetPasswordRequest>,
) -> ApiResult<impl IntoResponse> {
    if req.password.len() < 8 {
        return Err(AppError::Validation("Password must be at least 8 characters".to_string()));
    }

    // Find valid reset token
    let reset = sqlx::query_as::<_, PasswordResetRow>(
        "SELECT id, user_id, expires_at, used FROM password_resets WHERE token = $1"
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Validation("Invalid or expired reset token".to_string()))?;

    if reset.used {
        return Err(AppError::Validation("Token has already been used".to_string()));
    }

    if Utc::now() > reset.expires_at {
        return Err(AppError::Validation("Token has expired".to_string()));
    }

    // Hash the new password
    let salt = SaltString::generate(&mut rand::thread_rng());
    let password_hash = Argon2::default()
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("Password hashing failed: {}", e)))?
        .to_string();

    // Update user password
    sqlx::query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind(&password_hash)
        .bind(reset.user_id)
        .execute(&state.db)
        .await?;

    // Mark token as used
    sqlx::query("UPDATE password_resets SET used = true WHERE id = $1")
        .bind(reset.id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({"message": "Password has been reset successfully."})))
}

// ── Internal row types ──

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    name: String,
    email: String,
}

#[derive(Debug, sqlx::FromRow)]
struct PasswordResetRow {
    id: Uuid,
    user_id: Uuid,
    expires_at: chrono::DateTime<Utc>,
    used: bool,
}
