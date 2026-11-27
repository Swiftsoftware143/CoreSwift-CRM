//! Platform-admin authority — who may act ACROSS tenants.
//!
//! `Claims.role` is ACCOUNT-scoped (see `super::models::Claims`: "Role within their account") and
//! every registration path writes `owner` for the first user of a tenant. No role string can
//! therefore express platform authority: a gate written as `role != "owner" && role !=
//! "agency_admin"` admitted every one of the 38 `owner` rows measured on production 2026-09-22 to
//! `GET /api/admin/tenants` and `POST /api/admin/impersonate` (cross-tenant account takeover).
//!
//! The authority is an explicit column on `users` — `is_platform_admin`, migration 071 — resolved
//! from the DATABASE by `claims.sub` on every platform request. Consequences:
//!
//! - it cannot be carried in a token, so a stale, forged-shaped or wrongly-issued claim never
//!   grants it;
//! - revoking it is one `UPDATE` and takes effect on the next request, not at token expiry;
//! - the tenant role vocabularies (`owner`, `account_owner`, `admin`, `member`) are never
//!   consulted, so a tenant cannot promote itself into platform authority.
//!
//! A missing row, an inactive user, a non-UUID subject or a NULL flag are all a DENY: the gate
//! fails closed. Tenant-level `owner`/`admin` keep their tenant-scoped powers through the tenant
//! handlers, which are intentionally untouched; they simply grant nothing platform-wide.

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use sqlx::PgPool;
use uuid::Uuid;

use super::models::Claims;
use crate::errors::AppError;
use crate::AppState;

/// The policy, isolated from I/O: only an explicit `true` grants platform authority.
pub fn grants(flag: Option<bool>) -> bool {
    matches!(flag, Some(true))
}

/// Resolve platform authority from the database for one authenticated subject.
///
/// A subject that is not a user id resolves to `false` (deny) rather than an error — machine
/// surfaces and smoke tests mint claims with synthetic subjects. A database failure is NOT
/// swallowed: the caller gets a 5xx, because "the authority lookup is broken" must never read as
/// "allowed" (nor as a misleading "forbidden").
pub async fn is_platform_admin(db: &PgPool, sub: &str) -> Result<bool, AppError> {
    let Ok(user_id) = Uuid::parse_str(sub) else {
        return Ok(false);
    };
    let flag: Option<bool> =
        sqlx::query_scalar("SELECT is_platform_admin FROM users WHERE id = $1 AND is_active")
            .bind(user_id)
            .fetch_optional(db)
            .await
            .map_err(|e| AppError::Internal(format!("platform-admin lookup failed: {e}")))?;
    Ok(grants(flag))
}

/// Handler-level guard: 403 unless the subject is a platform admin.
pub async fn require_platform_admin(db: &PgPool, sub: &str) -> Result<(), AppError> {
    if is_platform_admin(db, sub).await? {
        return Ok(());
    }
    tracing::warn!(subject = %sub, "refused a platform-admin surface for a non-operator");
    Err(AppError::Forbidden)
}

/// The OPERATOR-SEAT rule for cross-tenant read surfaces that a VA must also work (kanban
/// t_42872c18 — the plan-request queue).
///
/// A platform admin always qualifies. So does a plain `admin` ROLE **inside the workspace that
/// holds a platform admin** — that is the seat the owner invites a VA into. The qualifier is what
/// keeps this honest: `role = 'admin'` is also a CUSTOMER workspace role (`middleware::is_account_admin`
/// treats `admin` as an account admin), so granting on the role string alone would hand a
/// customer's own teammate every other tenant's plan requests. A tenant `admin` with no platform
/// admin in their workspace is refused, exactly like an `owner`.
///
/// The role is taken from the DATABASE by [`is_platform_staff`], never from the token claim.
pub fn grants_staff(flag: Option<bool>, role: &str, in_platform_workspace: bool) -> bool {
    matches!(flag, Some(true)) || (role == "admin" && in_platform_workspace)
}

/// Resolve the operator-seat rule from the database for one subject. Fails closed: no row, an
/// inactive user, a non-UUID subject, a NULL flag and a missing platform workspace are all a DENY.
pub async fn is_platform_staff(db: &PgPool, sub: &str) -> Result<bool, AppError> {
    let Ok(user_id) = Uuid::parse_str(sub) else {
        return Ok(false);
    };
    let row: Option<(bool, String, bool)> = sqlx::query_as(
        "SELECT COALESCE(u.is_platform_admin, false), u.role, \
                EXISTS (SELECT 1 FROM users p \
                         WHERE p.is_platform_admin AND p.is_active \
                           AND p.tenant_id = u.tenant_id) \
           FROM users u \
          WHERE u.id = $1 AND u.is_active",
    )
    .bind(user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Internal(format!("platform-staff lookup failed: {e}")))?;
    Ok(match row {
        Some((flag, role, in_workspace)) => grants_staff(Some(flag), &role, in_workspace),
        None => false,
    })
}

/// Router-level guard: one choke point for a platform-only router.
///
/// Must be layered so that it runs AFTER [`super::middleware::auth_middleware`], which is what
/// injects `Claims`; `Router::layer` wraps outermost-last, so the gate is added to the router
/// BEFORE the auth layer. Called without claims it answers 401 rather than guessing, so a layer
/// misordering is visible as 401 and never as a silent pass.
pub async fn require_platform_admin_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let Some(claims) = req.extensions().get::<Claims>().cloned() else {
        tracing::warn!(path = %req.uri().path(), "platform-admin gate reached without claims");
        return Err(AppError::Unauthorized);
    };
    require_platform_admin(&state.db, &claims.sub).await?;
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::{grants, grants_staff};

    /// The gate must fail closed: no row, a NULL flag, or a false flag is never an operator.
    #[test]
    fn only_an_explicit_true_grants_platform_authority() {
        assert!(grants(Some(true)));
        assert!(!grants(Some(false)));
        assert!(!grants(None));
    }

    /// The VA seat is an `admin` in the platform workspace — NOT a customer workspace admin.
    /// Measured risk this pins: `admin` is also a customer account role, so a role-only grant would
    /// hand a tenant's own teammate every other tenant's plan requests.
    #[test]
    fn a_va_seat_is_an_admin_inside_the_platform_workspace_only() {
        // platform admin, whatever the role
        assert!(grants_staff(Some(true), "owner", false));
        assert!(grants_staff(Some(true), "agency_admin", false));
        // the VA: admin role in the operator's workspace, no platform flag
        assert!(grants_staff(Some(false), "admin", true));
        // a CUSTOMER workspace admin is not an operator
        assert!(!grants_staff(Some(false), "admin", false));
        // and none of the customer roles qualify, in or out of the platform workspace
        for role in [
            "owner",
            "account_owner",
            "agency_admin",
            "member",
            "client_admin",
        ] {
            assert!(
                !grants_staff(Some(false), role, true),
                "{role} must be refused"
            );
            assert!(
                !grants_staff(Some(false), role, false),
                "{role} must be refused"
            );
        }
        // A NULL flag is never platform AUTHORITY, but it does not cancel the VA arm either: the
        // seat is decided by role + workspace, and `users.is_platform_admin` is NOT NULL in the
        // schema (the resolver COALESCEs anyway), so this only documents the split.
        assert!(!grants_staff(None, "owner", true));
        assert!(grants_staff(None, "admin", true));
    }
}
