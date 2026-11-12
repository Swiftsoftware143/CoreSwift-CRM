//! The workspace's own people — invite by email, the roster, removal (kanban t_5f314e9a).
//!
//! David 2026-11-11: *"a real second login INTO the account (not an admin-console row): the owner
//! invites a teammate by email, the system emails them a generated password (never a password
//! field), and they sign in and see the shared inbox."*
//!
//! WHAT EXISTED BEFORE THIS MODULE, and why neither half was that door:
//!   * `POST /api/auth/invite` mints a `tenant_invites` TOKEN and hands the owner a LINK. The
//!     teammate then had to open it, pick their OWN password, and sign up — so the workspace could
//!     not add a person, it could only invite a stranger to create a login. Nothing was emailed.
//!   * the automation webhook's `users.invite` action wrote a `users` row directly with
//!     `password_hash = 'PLACEHOLDER_HASH'` — a seat that exists and NOBODY can ever sign into,
//!     because no password exists and no mail was sent.
//!
//! THE MODEL HERE IS THE ONE THE REST OF THE FLEET SHIPS: name + email in, the SERVER generates the
//! password, hashes it, and mails it through the app's own credentials template. There is no
//! password field anywhere on this path, and the owner never learns the teammate's password.
//!
//! ── THE THREE RULES, each checked BEFORE anything is written ─────────────────────────────────────
//!   1. AUTHORITY — the caller must administer THIS workspace (owner / admin, or the platform
//!      operator's impersonation session). The tenant comes from the session (`claims.aid`), never
//!      from the body, so no caller can mint a person into somebody else's workspace.
//!   2. ALLOWANCE — the plan's `limit_max_users` ceiling is checked FIRST, on the SAME expression the
//!      contract calls `count_active_users` reads (`users.is_active`), because the alternative is a
//!      user row plus a credentials mail for a login the plan does not sell. The refusal is a 402
//!      that NAMES the plan and price that adds seats (`seat_refusal`), which is the wording David
//!      asked for: "a seat-limit refusal with a plain upgrade message".
//!   3. VOCABULARY — the role is parsed against the two roles a workspace may hand out
//!      (`member`, `admin`) before the row is written. `agency_admin` is deliberately NOT offerable:
//!      on this app it is the PLATFORM vocabulary (it opens `/api/admin/*`), so a workspace owner
//!      could otherwise hand a teammate platform-wide authority over every other customer.
//!
//! REMOVAL IS A DEACTIVATION, never a row delete. Three reasons, all measured on this app:
//!   * `users` is referenced by the workspace's own history (`private_email_messages.assigned_user_id`,
//!     `private_email_boxes.user_id`, audit columns), so a delete either fails on the FK or silently
//!     rewrites who owned what;
//!   * it IS revocation: `auth::middleware::user_session_state` reads the row on EVERY authenticated
//!     request and refuses an inactive account with 401, so the token they already hold stops
//!     working on their very next call;
//!   * the customer's data is theirs: nothing they were assigned is destroyed, it is UNASSIGNED
//!     (conversations and mailboxes come back to the workspace), and their personal API key is
//!     deactivated so a credential that outlives a person cannot outlive their access.
//!
//! `password_changed_at` is stamped on removal as well. It is the same column the change/reset
//! writers use, and the session check already refuses a token minted before it — so if the row is
//! ever reactivated, the old session cannot be replayed.

use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use super::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The plan feature every seat is measured against — the admin panel's own knob.
pub const SEAT_KEY: &str = "limit_max_users";

/// What one invite produced. `mail_sent` is the honest half: the seat is real either way, and the
/// roster says so when the credentials mail could not be queued (`users.password_undelivered`).
pub struct InvitedMember {
    pub user_id: Uuid,
    pub email: String,
    pub name: String,
    pub role: String,
    pub mail_sent: bool,
}

#[derive(Debug, Deserialize)]
pub struct InviteMemberRequest {
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Authority
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// May this session administer the workspace's people?
///
/// `is_account_admin` is the app's ONE answer for "this tenant's administrator" (`owner`, the legacy
/// `account_owner`, `admin`, and the platform operator's `agency_admin`). An IMPERSONATION session
/// carries the operator's own role and the TARGET workspace's `aid`, so it passes — which is what
/// makes the same door usable from the operator console without a second, weaker route.
pub fn may_manage_team(claims: &Claims) -> bool {
    crate::auth::middleware::is_account_admin(&claims.role) || claims.role == "impersonated"
}

fn require_team_admin(claims: &Claims) -> ApiResult<Uuid> {
    if !may_manage_team(claims) {
        return Err(AppError::Forbidden);
    }
    Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)
}

/// The workspace's OWNER — its oldest active person, which is the row the signup wrote.
///
/// Read from the database rather than from a role string: this app's first user is written with
/// role `owner`, but an older row (or an operator-appointed admin) must still be the person the
/// roster refuses to remove, or the workspace could end up with no door that can add people back.
async fn workspace_owner(db: &sqlx::PgPool, tenant_id: Uuid) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar(
        "SELECT id FROM users WHERE tenant_id = $1 AND is_active = true \
         ORDER BY created_at, id LIMIT 1",
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The seat ceiling
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The cheapest ACTIVE plan that sells MORE seats than `current`, with its price and allowance.
///
/// Read from the same registry rows the gate enforces (`module_features` / `plan_module_features`),
/// so the sentence can never promise an allowance the plan does not sell. Only a plan a customer can
/// actually buy is named: pointing them at an inactive plan sends them to a screen with no working
/// payment page (the same rule `private_email::feature_gate::next_mailbox_plan` follows).
async fn next_seat_plan(db: &sqlx::PgPool, current: i64) -> Option<(String, f64, i64)> {
    sqlx::query_as::<_, (String, f64, f64)>(
        r#"
        SELECT p.name, p.price_monthly::float8, pf.limit_value::float8
          FROM plans p
          JOIN plan_module_features pf ON pf.plan_id = p.id
          JOIN module_features f ON f.id = pf.module_feature_id
         WHERE f.key = 'limit_max_users'
           AND pf.enabled
           AND p.is_active
           AND pf.limit_value IS NOT NULL
           AND pf.limit_value > $1
         ORDER BY p.price_monthly ASC NULLS LAST
         LIMIT 1
        "#,
    )
    .bind(current as f64)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|(name, price, limit)| (name, price, limit as i64))
}

/// The 402 body for a reached seat ceiling — plain English, the numbers on screen, and the plan that
/// lifts them. No "quota", no "max_users", no error code: this is read by a business owner.
pub async fn seat_refusal(db: &sqlx::PgPool, used: i64, limit: i64) -> AppError {
    if limit <= 0 {
        let unlock = match next_seat_plan(db, 0).await {
            Some((name, price, seats)) => {
                format!("{name} includes {seats} people for ${price:.0}/month.")
            }
            None => "Ask us to add team seats to your plan.".to_string(),
        };
        return AppError::UpgradeRequired(format!(
            "Team seats are not part of your current plan. {unlock}"
        ));
    }

    let noun = if limit == 1 { "person" } else { "people" };
    let detail = match next_seat_plan(db, limit).await {
        Some((name, price, seats)) => format!(
            "{name} raises that to {seats} people for ${price:.0}/month, and you can invite them right away."
        ),
        None => "You are on the largest seat allowance, so free a seat by removing a teammate you no longer need."
            .to_string(),
    };
    AppError::UpgradeRequired(format!(
        "Your plan includes {limit} {noun}, and {used} {} already in use. {detail} Nothing is charged \
         and nobody loses access either way.",
        if used == 1 { "is" } else { "are" }
    ))
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// The ONE seat writer
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// A person's display name from what the owner typed, or — when only an address was given — from the
/// address itself, so the roster never lists a nameless row. `jane.doe@acme.com` becomes `Jane Doe`.
pub fn display_name(raw: Option<&str>, email: &str) -> String {
    let typed = raw.map(str::trim).unwrap_or("");
    if !typed.is_empty() {
        return typed.to_string();
    }
    let local = email.split('@').next().unwrap_or(email);
    let cleaned: String = local
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { ' ' })
        .collect();
    let name = cleaned
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if name.is_empty() {
        local.to_string()
    } else {
        name
    }
}

/// The role a workspace may hand out. Anything else is a 422 that names the field, instead of a row
/// written with a role no gate recognises (and never `agency_admin`, which is PLATFORM authority).
pub fn workspace_role(raw: Option<&str>) -> Result<String, AppError> {
    match raw.map(|r| r.trim().to_lowercase()).as_deref() {
        None | Some("") | Some("member") => Ok("member".to_string()),
        Some("admin") => Ok("admin".to_string()),
        Some(other) => Err(AppError::Validation(format!(
            "role: '{other}' is not a role a workspace can hand out. Use 'member' or 'admin'."
        ))),
    }
}

/// Mint one teammate seat: validate, check the ceiling, write the row, mail the password.
///
/// `invited_by` is `None` for a door with no human caller (the automation webhook), so the roster
/// says "added by automation" rather than inventing a person.
pub async fn mint_member(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    invited_by: Option<Uuid>,
    email_raw: &str,
    name_raw: Option<&str>,
    role_raw: Option<&str>,
) -> Result<InvitedMember, AppError> {
    // ── 1. the input boundary, before any write ──────────────────────────────────────────────────
    let email = crate::security::email_addr::normalize(email_raw).map_err(AppError::Validation)?;
    let role = workspace_role(role_raw)?;
    let name = display_name(name_raw, &email);

    // ── 2. one login per address per workspace, with the message that says so ────────────────────
    let already: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM users WHERE tenant_id = $1 AND lower(email) = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(&email)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?;
    if already.is_some() {
        return Err(AppError::Duplicate(format!(
            "{email} is already part of this workspace."
        )));
    }

    // ── 3. the plan's seat ceiling — the SAME usage expression the conflict check above reads ────
    let used = crate::features::count_active_users(db, tenant_id).await;
    if let Some(limit) = crate::features::usage_ceiling(db, tenant_id, SEAT_KEY).await? {
        if used >= limit {
            return Err(seat_refusal(db, used, limit).await);
        }
    }

    // ── 4. the login: generated here, hashed here, mailed here, and never logged ─────────────────
    let password = super::signup::generate_temp_password();
    let password_hash = super::handlers::hash_password(&password)?;

    // `first_name`/`last_name` are written too: `/api/private-email/members` and the assignment
    // pickers read THOSE columns, and a row that carries only `name` renders as a blank person.
    let (first_name, last_name) = split_name(&name);

    let user_id = Uuid::new_v4();
    let inserted = sqlx::query(
        r#"INSERT INTO users (id, tenant_id, email, password_hash, name, role,
                              first_name, last_name, is_active, invited_by, invited_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, true, $9, NOW())"#,
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(&email)
    .bind(&password_hash)
    .bind(&name)
    .bind(&role)
    .bind(&first_name)
    .bind(&last_name)
    .bind(invited_by)
    .execute(db)
    .await
    .map_err(|e| {
        // `users.email` is UNIQUE across the whole install: a person already signing in to ANOTHER
        // workspace cannot be a second workspace's teammate without a second identity. That is a
        // refusal a human can act on, not a 500 "Database error".
        if let sqlx::Error::Database(ref dbe) = e {
            if matches!(
                dbe.constraint(),
                Some("users_email_key") | Some("idx_users_tenant_email")
            ) {
                return AppError::Duplicate(format!(
                    "{email} already signs in to another workspace on CoreSwift. A person can belong \
                     to one workspace at a time, so invite a different address or have that account \
                     closed first."
                ));
            }
        }
        AppError::Database(e)
    })?;
    debug_assert_eq!(inserted.rows_affected(), 1);

    // ── 5. the credentials mail — the ONLY way the teammate learns the password ───────────────────
    let tenant_name: String = sqlx::query_scalar("SELECT name FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(db)
        .await
        .map_err(AppError::Database)?
        .unwrap_or_else(|| "your workspace".to_string());
    let mail_sent = super::signup::send_credentials_email(
        db,
        tenant_id,
        &tenant_name,
        &email,
        &name,
        &password,
    )
    .await;
    if !mail_sent {
        // The seat is real either way. Record that the password never left, which is the state the
        // console reads to say "we could not email this person — set their first password here".
        sqlx::query("UPDATE users SET password_undelivered = true WHERE id = $1")
            .bind(user_id)
            .execute(db)
            .await
            .map_err(AppError::Database)?;
    }

    Ok(InvitedMember {
        user_id,
        email,
        name,
        role,
        mail_sent,
    })
}

/// "Jane Doe" -> ("Jane", "Doe"); a single word fills the first name and leaves the last empty.
fn split_name(name: &str) -> (String, String) {
    let mut parts = name.split_whitespace();
    let first = parts.next().unwrap_or("").to_string();
    let rest = parts.collect::<Vec<_>>().join(" ");
    (first, rest)
}

// ─────────────────────────────────────────────────────────────────────────────────────────────────
// Handlers
// ─────────────────────────────────────────────────────────────────────────────────────────────────

/// `POST /api/auth/team/invite` — add a teammate to THIS workspace and email them their password.
pub async fn invite_member(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<InviteMemberRequest>,
) -> ApiResult<Json<Value>> {
    let tenant_id = require_team_admin(&claims)?;
    let caller = Uuid::parse_str(&claims.sub).ok();

    let invited = mint_member(
        &state.db,
        tenant_id,
        caller,
        &req.email,
        req.name.as_deref(),
        req.role.as_deref(),
    )
    .await?;

    let used = crate::features::count_active_users(&state.db, tenant_id).await;
    let limit = crate::features::usage_ceiling(&state.db, tenant_id, SEAT_KEY)
        .await?
        .unwrap_or(-1);

    Ok(Json(json!({
        "user_id": invited.user_id,
        "email": invited.email,
        "name": invited.name,
        "role": invited.role,
        // `true` = a message was queued to the app's own transport. The console says so on screen
        // instead of promising mail that never left.
        "mail_sent": invited.mail_sent,
        "message": if invited.mail_sent {
            format!("{} has been added. We emailed {} their password for this workspace.", invited.name, invited.email)
        } else {
            format!(
                "{} has been added, but the email carrying the password could not be sent. Share the \
                 sign-in address and use \"Set a password\" in their profile, or ask us to resend it.",
                invited.name
            )
        },
        "seats": { "used": used, "limit": if limit < 0 { Value::Null } else { json!(limit) } },
    })))
}

/// `GET /api/auth/team/members` — who is in this workspace, and how many seats the plan sells.
///
/// Any signed-in member may read the roster: the Inbox's assignment picker is the same list, and a
/// teammate has to know who they are handing a conversation to. Only the OWNER/ADMIN controls
/// (invite, remove) are authority-gated.
pub async fn list_members(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let owner_id = workspace_owner(&state.db, tenant_id).await?;
    let caller = Uuid::parse_str(&claims.sub).ok();

    let rows = sqlx::query(
        r#"SELECT id, email, name, first_name, role, is_active, created_at, invited_at, invited_by,
                  password_undelivered,
                  (SELECT u2.email FROM users u2 WHERE u2.id = users.invited_by) AS invited_by_email
             FROM users
            WHERE tenant_id = $1
            ORDER BY is_active DESC, created_at ASC"#,
    )
    .bind(tenant_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let members: Vec<Value> = rows
        .iter()
        .map(|r| {
            use sqlx::Row;
            let id: Uuid = r.get("id");
            let first: Option<String> = r.try_get("first_name").ok();
            let name: String = r.try_get("name").unwrap_or_default();
            let display = if name.trim().is_empty() {
                first.unwrap_or_default()
            } else {
                name
            };
            json!({
                "id": id,
                "email": r.try_get::<String, _>("email").unwrap_or_default(),
                "name": display,
                "role": r.try_get::<String, _>("role").unwrap_or_default(),
                "is_active": r.try_get::<bool, _>("is_active").unwrap_or(false),
                "is_owner": owner_id == Some(id),
                "is_me": caller == Some(id),
                "joined_at": r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("created_at").ok().flatten(),
                "invited_at": r.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("invited_at").ok().flatten(),
                "invited_by": r.try_get::<Option<String>, _>("invited_by_email").ok().flatten(),
                "password_undelivered": r.try_get::<bool, _>("password_undelivered").unwrap_or(false),
            })
        })
        .collect();

    let used = crate::features::count_active_users(&state.db, tenant_id).await;
    let limit = crate::features::usage_ceiling(&state.db, tenant_id, SEAT_KEY)
        .await?
        .unwrap_or(-1);

    Ok(Json(json!({
        "members": members,
        "seats": { "used": used, "limit": if limit < 0 { Value::Null } else { json!(limit) } },
        "can_manage": may_manage_team(&claims),
    })))
}

/// `DELETE /api/auth/team/members/:id` — take a teammate's access away. Deletes nothing.
///
/// Every statement is scoped by the SESSION's tenant, so an id from another workspace is a 404 that
/// cannot confirm the row exists. Two refusals are this door's own: the caller's own login (sign out
/// instead) and the workspace owner (the row that can always add people back).
pub async fn remove_member(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(user_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let tenant_id = require_team_admin(&claims)?;
    let me = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;
    if user_id == me {
        return Err(AppError::BadRequest(
            "That is your own login. Sign out instead of removing yourself from the workspace."
                .into(),
        ));
    }
    if workspace_owner(&state.db, tenant_id).await? == Some(user_id) {
        return Err(AppError::Forbidden);
    }

    let target: Option<String> = sqlx::query_scalar(
        "SELECT email FROM users WHERE id = $1 AND tenant_id = $2 AND is_active = true",
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    let email = target.ok_or_else(|| {
        AppError::NotFound("That teammate is not active in this workspace.".into())
    })?;

    // The revocation, the unassignments and the deactivated key commit TOGETHER: a half-done removal
    // would either leave a live session on an unassigned inbox or an assignment on a person who
    // cannot see it.
    let mut tx = state.db.begin().await.map_err(AppError::Database)?;

    // `is_active = false` is the revocation the auth boundary already enforces (`user_session_state`
    // refuses an inactive row with 401 on their very next request). `password_changed_at` is stamped
    // so a token minted a second before the removal cannot be replayed if the row is reactivated.
    sqlx::query(
        "UPDATE users SET is_active = false, password_changed_at = NOW(), updated_at = NOW() \
          WHERE id = $1 AND tenant_id = $2 AND is_active = true",
    )
    .bind(user_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    // Their conversations come back to the workspace — the mail and the ticket stay where the team
    // can see them, only the name of who was working it goes away.
    let conversations = sqlx::query(
        "UPDATE private_email_messages SET assigned_user_id = NULL, updated_at = NOW() \
          WHERE tenant_id = $1 AND assigned_user_id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?
    .rows_affected() as i64;

    // …and so do the inboxes they were assigned.
    let mailboxes = sqlx::query(
        "UPDATE private_email_boxes SET user_id = NULL, updated_at = NOW() \
          WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?
    .rows_affected() as i64;

    // A personal API key is a credential that outlives a session; it must not outlive the access
    // that justified it.
    let api_keys = sqlx::query(
        "UPDATE personal_api_keys SET is_active = false, updated_at = NOW() \
          WHERE tenant_id = $1 AND user_id = $2 AND is_active = true",
    )
    .bind(tenant_id)
    .bind(user_id)
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?
    .rows_affected() as i64;

    tx.commit().await.map_err(AppError::Database)?;

    let used = crate::features::count_active_users(&state.db, tenant_id).await;
    let limit = crate::features::usage_ceiling(&state.db, tenant_id, SEAT_KEY)
        .await?
        .unwrap_or(-1);

    Ok(Json(json!({
        "removed": true,
        "email": email,
        "deleted": false,
        "unassigned": { "conversations": conversations, "mailboxes": mailboxes, "api_keys": api_keys },
        "seats": { "used": used, "limit": if limit < 0 { Value::Null } else { json!(limit) } },
        "message": format!(
            "{email} can no longer sign in. Their conversations and inboxes are back in the workspace \
             and nothing was deleted."
        ),
    })))
}

#[cfg(test)]
mod tests {
    use super::{display_name, may_manage_team, split_name, workspace_role};
    use crate::auth::models::Claims;

    fn claims(role: &str) -> Claims {
        Claims {
            sub: uuid::Uuid::new_v4().to_string(),
            aid: uuid::Uuid::new_v4().to_string(),
            role: role.into(),
            exp: 0,
            iat: 0,
            aud: None,
            iss: None,
            wsid: None,
        }
    }

    /// The authority rule, both ways: the workspace's own administrators and the operator's
    /// impersonation session pass; a plain member and an unknown vocabulary do not.
    #[test]
    fn only_an_administrator_may_add_or_remove_people() {
        for role in [
            "owner",
            "account_owner",
            "admin",
            "agency_admin",
            "impersonated",
        ] {
            assert!(
                may_manage_team(&claims(role)),
                "{role} must be able to manage"
            );
        }
        for role in ["member", "user", "team_member", ""] {
            assert!(!may_manage_team(&claims(role)), "{role} must not");
        }
    }

    /// A workspace may hand out `member` or `admin` and nothing else — in particular NOT
    /// `agency_admin`, which is platform authority on this app.
    #[test]
    fn a_workspace_cannot_hand_out_platform_authority() {
        assert_eq!(workspace_role(None).unwrap(), "member");
        assert_eq!(workspace_role(Some("")).unwrap(), "member");
        assert_eq!(workspace_role(Some(" Member ")).unwrap(), "member");
        assert_eq!(workspace_role(Some("ADMIN")).unwrap(), "admin");
        for bad in [
            "agency_admin",
            "owner",
            "super_admin",
            "platform_admin",
            "nonsense",
        ] {
            assert!(workspace_role(Some(bad)).is_err(), "{bad} must be refused");
        }
    }

    /// The roster must never list a nameless person: an address-only invite derives a readable name.
    #[test]
    fn an_address_only_invite_still_gets_a_name() {
        assert_eq!(display_name(Some("Jane Doe"), "jane@acme.com"), "Jane Doe");
        assert_eq!(display_name(Some("   "), "jane@acme.com"), "Jane");
        assert_eq!(display_name(None, "jane.doe@acme.com"), "Jane Doe");
        assert_eq!(
            display_name(None, "jane_doe+sales@acme.com"),
            "Jane Doe Sales"
        );
        assert_eq!(display_name(None, "123@acme.com"), "123");
        assert_eq!(split_name("Jane Doe"), ("Jane".into(), "Doe".into()));
        assert_eq!(split_name("Jane"), ("Jane".into(), String::new()));
    }
}
