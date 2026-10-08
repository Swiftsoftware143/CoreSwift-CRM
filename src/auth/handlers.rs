//! Auth handlers: register, login, refresh, me, logout.
//!
//! All handlers are tenant-scoped. On register, a new tenant can be created
//! or an existing tenant slug can be specified.

use axum::{
    extract::{Extension, Request, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use serde_json::{json, Value};
use uuid::Uuid;

use argon2::{Argon2, PasswordHasher};
use password_hash::SaltString;

use super::middleware;
use super::models::*;
use crate::errors::{ApiResult, AppError};
use crate::security::email_addr;
use crate::sql_json::row_json;
use crate::AppState;

/// Request header a fleet harness sets to record that the tenant it is about to mint is a probe.
/// Its value lands verbatim in `tenants.probe_harness` (migration 099). Fleet policy:
/// `/opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md` (answers 3(b)/3(c), card t_66db3251).
pub(crate) const HARNESS_HEADER: &str = "x-swift-harness";

/// Validate a harness marker: `trim()`, lowercase, then `^[a-z0-9][a-z0-9._-]{2,63}$`.
///
/// A missing header, a non-UTF-8 value, or a value that fails the shape all yield `None`, and the
/// tenant is created with `probe_harness = NULL` — a rejected value must never fail the signup, so
/// there is no error path here at all.
///
/// The marker is read from this HEADER only, never from a body or query field: the signup body is
/// attacker-controlled on a public route, and the header is the convention the fleet's harnesses
/// follow. Validated by hand rather than by `regex` — the crate is not in the dependency graph, and
/// the check is a byte scan. `to_lowercase()` can lengthen a non-ASCII string, but every byte it
/// could produce (`0xC3`… `0xFF`) fails the class test below, so a non-ASCII value is always `None`.
pub(crate) fn harness_marker(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(HARNESS_HEADER)?.to_str().ok()?;
    let value = raw.trim().to_lowercase();
    let bytes = value.as_bytes();
    if bytes.len() < 3 || bytes.len() > 64 {
        return None;
    }
    let shaped = bytes.iter().enumerate().all(|(i, b)| match b {
        b'a'..=b'z' | b'0'..=b'9' => true,
        // `[a-z0-9._-]{2,63}` — the first character can never be one of these.
        b'.' | b'_' | b'-' => i > 0,
        _ => false,
    });
    shaped.then_some(value)
}

/// POST /api/auth/register — Create a new account.
/// Every signup creates their own isolated tenant (account).
/// Admins and tenants are both full account holders — no distinction.
/// Provide account_name/slug to customize, or one is auto-generated from email.
/// Provide invite_token to join an existing tenant as a team member.
pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut req): Json<RegisterRequest>,
) -> ApiResult<impl IntoResponse> {
    // ── Address boundary (kanban t_9252c512) ────────────────────────────────────────────────
    // FIRST, before any SELECT and long before any INSERT. `users.email` is both the login identity
    // and the only address the welcome/credentials mail can ever reach; this handler used to check
    // only `contains('@')` and bind `req.email` verbatim, so `a@b`, `bad@`, `@x.com` and `" a@b "`
    // all became real accounts no mail could ever be delivered to. normalize() trims + lowercases
    // as well as validates, and the normalised value is what the dup check reads, the INSERT
    // stores, the tokens carry and the welcome mail is sent to.
    req.email = email_addr::normalize(&req.email).map_err(AppError::Validation)?;

    // David's signup model (2026-09-29): the page collects NAME + EMAIL only, so `password` may
    // arrive empty. The server then mints one and emails it; the user confirms their address by
    // signing in with it (the real two-step check) and changes it later in profile settings. A
    // caller that still supplies a password is honoured and validated exactly as before.
    if req.name.is_empty() {
        return Err(AppError::Validation(
            "Name and email are required".to_string(),
        ));
    }
    if !req.password.is_empty() && req.password.len() < 8 {
        return Err(AppError::Validation(
            "Password must be at least 8 characters".to_string(),
        ));
    }
    let password = if req.password.is_empty() {
        crate::auth::signup::generate_temp_password()
    } else {
        req.password.clone()
    };

    // `users.email` carries a GLOBAL unique constraint (`users_email_key`, migration 002) and
    // `login` resolves a user by email alone — one address, one workspace, forever. This handler
    // used to test only `(tenant_id, email)`, while a signup that carries no invite mints a FRESH
    // tenant for a
    // signup that carries no invite: the scoped check passed, the INSERT hit the global index, and
    // AppError mapped the sqlx error to a 500 "Database error". A returning user learned nothing,
    // and the request had already created a workspace row (an orphan tenant).
    // Check the constraint the database actually enforces, and check it BEFORE `resolve_account`,
    // which mints that tenant and consumes the invite token.
    if sqlx::query_scalar::<_, String>("SELECT email FROM users WHERE email = $1 LIMIT 1")
        .bind(&req.email)
        .fetch_optional(&state.db)
        .await?
        .is_some()
    {
        return Err(email_taken_error(&req.email));
    }

    // Everything that creates rows — the workspace, the entry-plan row, the accepted invite and
    // the owner user — is ONE unit minted by ONE writer (`auth::signup::create_account`, kanban
    // t_e968e9ad). The machine door `POST /api/v1/internal/provision-free-account` calls the same
    // function, so the two doors can never drift into two account shapes. It runs on a
    // transaction, so a rejection at any point (the duplicate check, the seat ceiling, a missing
    // free plan) leaves no orphan tenant and no burned invite.
    let mut tx = state.db.begin().await.map_err(AppError::Database)?;

    // The harness marker is read from the header ONCE, before the tenant exists, and only ever
    // reaches the INSERT arm that mints a tenant; the invite arm joins an existing tenant and has
    // nothing to mark.
    let harness = harness_marker(&headers);
    let account = crate::auth::signup::create_account(
        &state.db,
        &mut tx,
        crate::auth::signup::MintRequest {
            email: &req.email,
            name: &req.name,
            password: &password,
            account_name: req.account_name.as_deref(),
            account_slug: req.account_slug.as_deref(),
            invite_token: req.invite_token.as_deref(),
            into_tenant: None,
            // The self-serve signup keeps the platform default; the tag door is the one with the
            // operator-configurable entry plan (`provision_entry_plan_slug`).
            entry_plan_slug: crate::billing::DEFAULT_PLAN_SLUG,
            harness: harness.as_deref(),
        },
    )
    .await?;

    let user = account.user;
    let tenant_id = account.tenant_id;
    let tenant_name = account.tenant_name;
    let tenant_slug = account.tenant_slug;
    let tenant_is_active = account.tenant_is_active;
    let is_first_user = account.is_first_user;

    // Platform authority is resolved from the DATABASE by the token subject, never from
    // `user.role`: a fresh signup is `owner` of its own tenant, which grants nothing
    // platform-wide. The app shell gates its ADMIN nav on this value, so it ships in the payload.
    let platform_admin =
        crate::auth::platform_admin::is_platform_admin(&state.db, &user.id.to_string()).await?;

    // The workspace, its entry plan and the owner user are one unit: nothing above this line
    // survives a failure here.
    tx.commit().await.map_err(AppError::Database)?;

    // Generate tokens
    let (access_token, refresh_token, expires_in) = generate_tokens(&user, &state)?;

    // The credentials mail is the SAME template the machine door sends
    // (`auth::signup::send_credentials_email`) — one implementation, so a change to the
    // credentials shape cannot land on only one door.
    crate::auth::signup::send_credentials_email(
        &state.db,
        tenant_id,
        &tenant_name,
        &req.email,
        &req.name,
        &password,
    )
    .await;
    let mut next_steps = vec![
        "Connect your apps — POST /api/native/apps/{slug}/connect".to_string(),
        "Create contacts — POST /api/contacts".to_string(),
        "Set up pipelines — POST /api/pipelines".to_string(),
    ];
    if is_first_user {
        next_steps.insert(
            0,
            format!(
                "Invite team members — use your tenant slug: '{}'",
                tenant_slug
            ),
        );
    }

    Ok((
        StatusCode::CREATED,
        Json(json!(RegisterResponse {
            access_token,
            refresh_token,
            token_type: "Bearer".to_string(),
            expires_in,
            team_member: team_member_payload(user, platform_admin),
            platform_admin,
            account: AccountResponse {
                id: tenant_id,
                name: tenant_name,
                slug: tenant_slug,
                is_active: tenant_is_active,
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
    // The same normalisation `register` stores by, matched case-insensitively so an account stored
    // with capitals (or created before normalisation existed — live has
    // `Swiftimpactsolutions@gmail.com`) still resolves when the customer retypes their address in
    // different casing. A malformed value is NOT refused here: login answers its own 401 for every
    // wrong credential, so it must not become an account-existence oracle — it simply matches
    // nothing.
    let user = sqlx::query_as::<_, TeamMember>(
        "SELECT * FROM users WHERE lower(email) = $1 AND is_active = true",
    )
    .bind(email_addr::lookup_key(&req.email))
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

    let platform_admin =
        crate::auth::platform_admin::is_platform_admin(&state.db, &user.id.to_string()).await?;

    let (access_token, refresh_token, expires_in) = generate_tokens(&user, &state)?;

    Ok(Json(json!(TokenResponse {
        access_token,
        refresh_token,
        token_type: "Bearer".to_string(),
        expires_in,
        team_member: team_member_payload(user, platform_admin),
        platform_admin,
    })))
}

/// POST /api/auth/refresh — Exchange refresh token for new access token.
pub async fn refresh(
    State(state): State<AppState>,
    Json(req): Json<RefreshRequest>,
) -> ApiResult<impl IntoResponse> {
    let claims = middleware::verify_token(&req.refresh_token, &state.config.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user =
        sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE id = $1 AND is_active = true")
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
pub async fn me(State(state): State<AppState>, request: Request) -> ApiResult<impl IntoResponse> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::Unauthorized)?;

    let claims = middleware::verify_token(token, &state.config.jwt_secret)?;

    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let user =
        sqlx::query_as::<_, TeamMember>("SELECT * FROM users WHERE id = $1 AND is_active = true")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::Unauthorized)?;

    // The same resolution /api/admin/* uses, so the shell and the router cannot disagree:
    // one source (`users.is_platform_admin`), one subject (the token), one request.
    let platform_admin =
        crate::auth::platform_admin::is_platform_admin(&state.db, &user.id.to_string()).await?;

    Ok(Json(json!({
        "team_member": team_member_payload(user, platform_admin),
        "platform_admin": platform_admin,
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

    // tenant_invites.role has a CHECK for admin/member only; validate here so a bad role is a
    // clean 422 instead of a constraint violation surfacing as a 500.
    let role = match req.role.trim() {
        "admin" => "admin",
        "member" => "member",
        _ => {
            return Err(AppError::Validation(
                "role must be 'admin' or 'member'".to_string(),
            ))
        }
    };

    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let token = uuid::Uuid::new_v4().to_string();

    sqlx::query(
        "INSERT INTO tenant_invites (id, tenant_id, token, role, expires_at) VALUES ($1, $2, $3, $4, NOW() + INTERVAL '7 days')"
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(&token)
    .bind(role)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "invite_token": token,
        "invite_url": format!("/auth/register?invite_token={}", token),
        "expires_in_days": 7,
        "role": role,
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
    let invites = sqlx::query_scalar::<_, serde_json::Value>(
        &row_json!("SELECT id, token, role, accepted, expires_at, created_at FROM tenant_invites WHERE tenant_id = $1 ORDER BY created_at DESC")
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

/// A returning user signing up again is the common case, not an exotic one — and it used to answer
/// a 500 "Database error". `users.email` is globally unique (`users_email_key`) and `login` resolves
/// a user by email alone, so an address belongs to exactly one workspace: say that, and say what to
/// do instead.
pub(crate) fn email_taken_error(email: &str) -> AppError {
    AppError::Duplicate(format!(
        "Email '{}' is already registered — sign in instead",
        email
    ))
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
///
/// `pub(crate)` so `profile::handlers::change_password` hashes with the SAME parameters as
/// `register`/`login` — a second copy of this is how a hash scheme drifts and users get locked out.
pub(crate) fn hash_password(password: &str) -> Result<String, AppError> {
    use argon2::{
        password_hash::{PasswordHasher, SaltString},
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
pub(crate) fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
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
    // The SAME boundary rule as `register`, from the same function: an address that could never
    // receive the reset mail is refused with the same 422/field shape instead of silently
    // reporting "if the email exists…" for an address that cannot exist as a mailbox. The reply
    // stays unconditional for every well-formed address, so it still leaks nothing about accounts;
    // the lookup matches `lower(email)` so a row stored before normalisation still resolves.
    let email = email_addr::normalize(&req.email).map_err(AppError::Validation)?;

    // Look up user. `tenant_id` comes along because the mail path needs it: `outbound_messages.tenant_id`
    // is a NOT NULL FK to `tenants`, so queueing the reset with `Uuid::nil()` violated the constraint
    // and the send was swallowed by the `let _ =` below — the reset mail never left the box at all,
    // silently (measured 2026-10-08, kanban t_71cc3ad8). The row's own workspace is the correct tenant.
    let user = sqlx::query_as::<_, UserRow>(
        "SELECT id, name, tenant_id FROM users WHERE lower(email) = $1",
    )
    .bind(&email)
    .fetch_optional(&state.db)
    .await?;

    let user = match user {
        Some(u) => u,
        None => {
            // Don't reveal whether email exists — return success either way
            return Ok(Json(
                json!({"message": "If that email is registered, a reset link has been sent."}),
            ));
        }
    };

    // Create reset token (expires in 1 hour)
    let token = Uuid::new_v4().to_string();
    let expires_at = Utc::now() + chrono::Duration::hours(1);

    sqlx::query(
        "INSERT INTO password_resets (id, user_id, token, expires_at) VALUES ($1, $2, $3, $4)",
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
        user.tenant_id,
        &email,
        "password_reset",
        &vars,
    )
    .await
    .map_err(|e| {
        tracing::warn!(error = %e, "Failed to send password reset email via template");
        e
    })
    .ok();

    Ok(Json(
        json!({"message": "If that email is registered, a reset link has been sent."}),
    ))
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
        return Err(AppError::Validation(
            "Password must be at least 8 characters".to_string(),
        ));
    }

    // Find valid reset token
    let reset = sqlx::query_as::<_, PasswordResetRow>(
        "SELECT id, user_id, expires_at, used FROM password_resets WHERE token = $1",
    )
    .bind(&req.token)
    .fetch_optional(&state.db)
    .await?
    .ok_or_else(|| AppError::Validation("Invalid or expired reset token".to_string()))?;

    if reset.used {
        return Err(AppError::Validation(
            "Token has already been used".to_string(),
        ));
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

    Ok(Json(
        json!({"message": "Password has been reset successfully."}),
    ))
}

// ── Internal row types ──

#[derive(Debug, sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    name: String,
    tenant_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct PasswordResetRow {
    id: Uuid,
    user_id: Uuid,
    expires_at: chrono::DateTime<Utc>,
    used: bool,
}

pub async fn get_usage(
    State(state): State<AppState>,
    Extension(claims): Extension<crate::auth::models::Claims>,
) -> Result<Json<Value>, AppError> {
    let tid = uuid::Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::BadRequest("Invalid account".into()))?;
    let usage = crate::features::get_usage_json(&state.db, tid).await;
    Ok(Json(usage))
}
