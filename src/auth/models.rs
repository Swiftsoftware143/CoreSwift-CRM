//! Auth models: TeamMember, Account, Claims, TokenResponse, and request/response types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

/// JWT claims struct stored in access/refresh tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    /// Team member ID (subject)
    pub sub: String,
    /// Account ID (formerly tenant_id)
    pub aid: String,
    /// Role within their account: account_owner | admin | member
    pub role: String,
    /// Expiration timestamp (UTC epoch seconds)
    pub exp: usize,
    /// Issued at timestamp (UTC epoch seconds)
    pub iat: usize,
    /// Audience
    pub aud: Option<String>,
    /// Issuer
    pub iss: Option<String>,
}

/// TeamMember model — database row (from `users` table).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TeamMember {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub email: String,
    pub password_hash: String,
    pub name: String,
    pub role: String,
    pub is_active: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// TeamMember response sent to clients (excludes password_hash).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMemberResponse {
    pub id: Uuid,
    pub account_id: Uuid,
    pub email: String,
    pub name: String,
    pub role: String,
    pub is_active: bool,
    pub last_login_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<TeamMember> for TeamMemberResponse {
    fn from(u: TeamMember) -> Self {
        Self {
            id: u.id,
            account_id: u.tenant_id,
            email: u.email,
            name: u.name,
            role: u.role,
            is_active: u.is_active,
            last_login_at: u.last_login_at,
            created_at: u.created_at,
        }
    }
}

/// The auth payload's `team_member`, with the session's RESOLVED platform authority merged in.
///
/// `platform_admin` is deliberately NOT a field of `TeamMember`/`TeamMemberResponse`: `TeamMember`
/// is a `sqlx::FromRow` over `SELECT *` on `users`, so a field added there makes every other query
/// that decodes it fail (and every `TeamMemberResponse::from` site have to invent a value). The
/// flag is a property of the SESSION — resolved from `users.is_platform_admin` by token subject on
/// every request (`crate::auth::platform_admin`) — not of the row, so it is merged into the
/// serialized object, which is exactly where the SPA reads it (`STATE.user`, taken from
/// `data.team_member`).
pub fn team_member_payload(user: TeamMember, platform_admin: bool) -> Value {
    let mut body = serde_json::to_value(TeamMemberResponse::from(user))
        .unwrap_or_else(|_| Value::Object(Map::new()));
    if let Some(obj) = body.as_object_mut() {
        obj.insert("platform_admin".to_string(), Value::Bool(platform_admin));
    }
    body
}

/// Token response sent after login/register.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    /// Serialized team member + the resolved `platform_admin` flag (see `team_member_payload`).
    pub team_member: Value,
    /// The same resolved value at the top level, for clients that read the payload rather than
    /// the team member. `false` unless the DATABASE says otherwise.
    pub platform_admin: bool,
}

/// Register request body.
/// Every person who signs up gets their own account (separate tenant in DB).
/// Team members can join an existing account via invite.
/// Pass account_name and account_slug to customize the account name,
/// or pass invite_token to join an existing account via invite.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub name: String,
    pub email: String,
    pub password: String,
    pub account_name: Option<String>,
    pub account_slug: Option<String>,
    pub invite_token: Option<String>,
}

/// Register response with account info included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub team_member: Value,
    pub account: AccountResponse,
    pub next_steps: Vec<String>,
    /// The resolved platform authority for this session (see `team_member_payload`).
    pub platform_admin: bool,
}

/// Account info for response (from `tenants` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_active: bool,
}

/// Login request body.
#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Refresh token request body.
#[derive(Debug, Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}

/// Account model reference for auth handlers (from `tenants` table).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub is_active: bool,
}

/// Create invite request.
#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub role: String, // "admin" | "member"
}

/// Helper to extract claims from an Authorization header in a handler
/// that doesn't use the extension extractor (e.g. /me, /logout).
pub fn extract_claims_from_request(
    request: &axum::extract::Request,
    secret: &str,
) -> Result<Claims, crate::errors::AppError> {
    let auth_header = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(crate::errors::AppError::Unauthorized)?;

    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(crate::errors::AppError::Unauthorized)?;

    use crate::auth::middleware;
    middleware::verify_token(token, secret)
}
