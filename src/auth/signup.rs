//! The ONE account-minting writer for CoreSwift-CRM (kanban t_e968e9ad).
//!
//! Two doors mint an account, and both call [`create_account`]:
//!
//!   * the self-serve signup — `POST /api/auth/register` ([`super::handlers::register`]); and
//!   * the machine door — `POST /api/v1/internal/provision-free-account`
//!     ([`crate::tag_provision_handler::handle_provision_free_account`]), reached by FunnelSwift
//!     when a lead is tagged with `CoreSwift — Free`.
//!
//! Before this module the signup core lived inline in `register`, so a second door that minted
//! its own tenant + `tenant_plans` row + user would have been a SECOND writer of the same unit —
//! and the two would drift (one seats a plan, the other forgets; one normalises the address, the
//! other does not; one minted the owner, the other a `member`). The mint is therefore one
//! function, running on the CALLER's connection so it commits with whatever else the caller
//! wrote, and the unit it produces is fixed:
//!
//! ```text
//!   tenants row  +  tenant_plans row for the entry plan  +  the owner `users` row
//! ```
//!
//! ## The owner's role
//!
//! The role is the signup's own vocabulary: the FIRST user of a workspace owns it, and
//! `register` has always written `owner` for them. `user_role` has no `admin` member
//! (`{agency_admin, client_admin, team_member, user, company_admin, owner, member}`), so
//! "admin" is not a value this schema can store; and the billing gate the in-app upgrade page
//! goes through admits `owner | admin | agency_admin`
//! ([`crate::billing::handlers::is_tenant_billing_owner`]), so `owner` is both the correct
//! vocabulary and the value the upgrade surface accepts.
//!
//! ## The entry plan
//!
//! [`seat_entry_plan`] refuses anything that is not an ACTIVE plan with `price_monthly = 0`, so
//! a minted workspace can never be seated on a paid tier by a mistyped setting — the mint is
//! refused (`422`) instead. CoreSwift's `plans` table carries `price_monthly`
//! (`billing::DEFAULT_PLAN_SLUG` is `free`).

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::errors::AppError;

use super::models::TeamMember;

/// The first password the server mints when a signup supplies none (David's NAME + EMAIL model).
/// 16 chars of entropy from the same `rand` thread RNG the Argon2 salt uses; the glyph set drops
/// look-alikes (`0O1lI`) so the emailed value is easy to retype on a phone. Never logged.
pub fn generate_temp_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789!@#";
    let mut rng = rand::thread_rng();
    (0..16)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

/// What one mint produced — the owner, the workspace it owns, and whether this call created it.
pub struct CreatedAccount {
    pub user: TeamMember,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub tenant_slug: String,
    /// `tenants.is_active` is NULLABLE, so this mirrors the column: NULL is reported as unknown,
    /// never invented as `true` (`auth::models::AccountResponse` does the same).
    pub tenant_is_active: Option<bool>,
    /// The first user of a workspace owns it; a later join gets the inviter's role.
    pub is_first_user: bool,
}

/// Everything [`create_account`] needs. `email` must ALREADY be normalised
/// (`crate::security::email_addr::normalize`); this function never re-derives it, so the value
/// the dup check reads is the value the INSERT stores.
pub struct MintRequest<'a> {
    pub email: &'a str,
    pub name: &'a str,
    pub password: &'a str,
    /// A field carrying a real value (non-blank once trimmed) names/slugs the workspace
    /// explicitly; a field that is `None` or whitespace-only falls back to the signup's own auto
    /// arm — the name `"<owner>'s Workspace"`, the slug derived from the address (kanban
    /// t_2e40c9a5). The two halves are judged independently, so a custom name survives a missing
    /// slug and the other way round.
    pub account_name: Option<&'a str>,
    pub account_slug: Option<&'a str>,
    /// Join an EXISTING workspace (the signup door's invite arm) instead of minting one.
    pub invite_token: Option<&'a str>,
    /// Mint the owner user INTO this existing, machine-minted, zero-user workspace instead of
    /// creating one (the tag door's adoption arm). `None` on every signup.
    pub into_tenant: Option<Uuid>,
    /// The plan the workspace is seated on. Must resolve to an ACTIVE plan with
    /// `price_monthly = 0` or the mint is refused (`422`).
    pub entry_plan_slug: &'a str,
    /// `x-swift-harness` marker, recorded on a tenant this call creates (`tenants.probe_harness`).
    pub harness: Option<&'a str>,
}

/// The owner's name, trimmed — or the ONE refusal every account door shares (kanban t_0ab34bd3).
///
/// Kept out of [`create_account`] so the gate is unit-testable without a database: a whitespace-only
/// name is refused as `AppError::Validation`, which renders as HTTP 422
/// `{"error":true,"message":"name: is required","code":422}`, and the refusal runs BEFORE the
/// first INSERT of the mint. Whitespace is measured the way the database and the signup page see
/// it (`str::trim` — Unicode whitespace), so `""`, `"   "`, `"\t\n "` and `"\u{a0}"` are all
/// refused while `"  Bob  "` becomes `Bob`, which is what `users.name` stores and what the
/// auto-named workspace label `"<name>'s Workspace"` interpolates.
fn validated_owner_name(name: &str) -> Result<&str, AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation("name: is required".to_string()));
    }
    Ok(trimmed)
}

/// Mint one account — or join one, when `invite_token`/`into_tenant` say so.
///
/// `db` is the pool (the seat-ceiling helpers read it directly, as `register` always did); `conn`
/// is the caller's transaction, so a refusal anywhere rolls the whole mint back.
pub async fn create_account(
    db: &PgPool,
    conn: &mut PgConnection,
    req: MintRequest<'_>,
) -> Result<CreatedAccount, AppError> {
    // ── 0. the owner's name: the ONE gate both doors inherit (kanban t_0ab34bd3) ───────────────
    // `req.name` is bound verbatim to `users.name` (step 2) and interpolated into the workspace
    // label `"<name>'s Workspace"` (tenant_identity), so a name that is only whitespace mints a
    // nameless owner in a workspace called `"   's Workspace"`. The public door's `is_empty()`
    // test let `"   "` and `"\t\n "` straight through, and the machine door
    // (POST /api/v1/internal/provision-free-account) reaches this same function — a guard in one
    // handler would leave the other door open (fleet rule: second-door rule). So the refusal
    // lives HERE, at the ONE account writer, before ANY insert: both doors, and any future
    // caller, inherit it. The trimmed value is what every downstream use reads.
    let owner_name = validated_owner_name(req.name)?;

    // ── 1. the workspace: join an invite, adopt a machine-minted one, or mint a new one ────────
    let (tenant_id, tenant_name, tenant_slug, tenant_is_active, invite_role) = if let Some(token) =
        req.invite_token
    {
        let invite = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT tenant_id, role FROM tenant_invites WHERE token = $1 AND accepted = false AND expires_at > NOW()"
        )
        .bind(token)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid or expired invite token".into()))?;

        sqlx::query(
            "UPDATE tenant_invites SET accepted = true, accepted_at = NOW() WHERE token = $1",
        )
        .bind(token)
        .execute(&mut *conn)
        .await?;

        let (name, slug, is_active) = load_tenant(&mut *conn, invite.0).await?;
        (invite.0, name, slug, is_active, Some(invite.1))
    } else if let Some(existing) = req.into_tenant {
        // Adoption: the workspace already exists (minted by the tag path, which wrote a tenant +
        // its free-plan row + a contact and NO user), so this call adds the missing owner to it
        // rather than minting a second workspace for the same person. The plan row is re-seated
        // idempotently (`ON CONFLICT (tenant_id) DO NOTHING`).
        let (name, slug, is_active) = load_tenant(&mut *conn, existing).await?;
        seat_entry_plan(&mut *conn, existing, req.entry_plan_slug).await?;
        (existing, name, slug, is_active, None)
    } else {
        let (name, slug) = tenant_identity(&req, owner_name);
        let tenant_id = Uuid::new_v4();
        let row: (String, String, Option<bool>) = sqlx::query_as(
            r#"INSERT INTO tenants (id, name, slug, probe_harness)
               VALUES ($1, $2, $3, $4)
               RETURNING name, slug, is_active"#,
        )
        .bind(tenant_id)
        .bind(&name)
        .bind(&slug)
        .bind(req.harness)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref dbe) = e {
                if dbe.constraint() == Some("tenants_slug_key") {
                    return AppError::Duplicate(format!("Tenant slug '{}' already exists", slug));
                }
            }
            AppError::Database(e)
        })?;
        // The mint and its plan row commit TOGETHER, so a workspace can never land row-less and
        // resolve through `module_registry::resolve`'s `no_plan` arm (every module granted, no
        // numeric ceiling).
        seat_entry_plan(&mut *conn, tenant_id, req.entry_plan_slug).await?;
        (tenant_id, row.0, row.1, row.2, None)
    };

    // ── 2. the other unique index on `users`: (tenant_id, email) ───────────────────────────────
    // This is the message that tells a team member they already belong to THIS workspace.
    let existing = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM users WHERE tenant_id = $1 AND email = $2",
    )
    .bind(tenant_id)
    .bind(req.email)
    .fetch_one(&mut *conn)
    .await?;

    if existing > 0 {
        return Err(AppError::Duplicate(format!(
            "User with email '{}' already exists in this tenant",
            req.email
        )));
    }

    // ── 3. the seat ceiling (the `limits` module's `limit_max_users`) ──────────────────────────
    // The rejection happens inside the transaction, so a 402 rolls the workspace, the plan row and
    // a burnt invite back with it.
    let user_usage = crate::features::count_active_users(db, tenant_id).await;
    crate::features::enforce_usage_limit(
        db,
        tenant_id,
        "limit_max_users",
        "User",
        "users",
        user_usage,
    )
    .await?;

    // ── 4. the owner user ─────────────────────────────────────────────────────────────────────
    let password_hash = super::handlers::hash_password(req.password)?;

    let is_first_user =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM users WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_one(&mut *conn)
            .await?
            == 0;

    // A first user owns the tenant. Everyone else gets the role the inviter chose.
    let role = if is_first_user {
        "owner"
    } else {
        invite_role.as_deref().unwrap_or("member")
    };

    let user = sqlx::query_as::<_, TeamMember>(
        r#"INSERT INTO users (id, tenant_id, email, password_hash, name, role)
           VALUES ($1, $2, $3, $4, $5, $6)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(req.email)
    .bind(&password_hash)
    .bind(owner_name)
    .bind(role)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| {
        // Two signups racing on the same address cannot both pass the pre-check: whichever loses
        // hit the global index, and that violation has to read as the same 409 instead of a 500.
        if let sqlx::Error::Database(ref dbe) = e {
            if matches!(
                dbe.constraint(),
                Some("users_email_key") | Some("idx_users_tenant_email")
            ) {
                return super::handlers::email_taken_error(req.email);
            }
        }
        AppError::Database(e)
    })?;

    Ok(CreatedAccount {
        user,
        tenant_id,
        tenant_name,
        tenant_slug,
        tenant_is_active,
        is_first_user,
    })
}

/// The ONE credentials mail both doors send: the app's existing `welcome` template, which is the
/// template that carries the password line (`register` has always sent this one with the
/// plaintext password in its variables).
///
/// Does the credentials mail actually leave? `true` ONLY when a message was queued, `false` when the
/// send was suppressed (probe/harness class, kanban t_12a8d509) or the send failed.
/// A send failure is logged and swallowed — the account is already real, and a mail outage must
/// not turn a completed mint into an error response. But it is now REPORTED: `register` turns a
/// `false` on a MINTED password into `users.password_undelivered = true`, which is the state
/// `/api/auth/initial-password` exists for (kanban t_d841df4f). Before this the value was an
/// invisible `()`, so the page had to promise mail whether or not any had been sent.
///
/// ── PROBE / HARNESS ACCOUNTS NEVER SEND (kanban t_12a8d509) ─────────────────────────────────────
/// This app was the only fleet app that mailed its own harnesses. Measured on the app's own
/// transport (Mailgun events for `mail.coreswiftcrm.com`, 2026-10-09 20:41 UTC): the credentials
/// mail `Welcome to CoreSwift CRM!` went to `csproof-1791578449@swiftsoftware.dev` — the address the
/// profile-UI proof mints through `POST /api/auth/register` — and came back as a real hard bounce
/// (`550 5.1.1 mailbox unavailable`, migadu). `swiftsoftware.dev` is a routable domain the fleet
/// owns, so that message landed in the operator's own mailbox on its way to bouncing, and every such
/// send burns a delivery against this account's sending reputation.
///
/// Two independent reasons suppress the queue, so silencing a probe needs no address change anywhere
/// (the same shape the sibling apps ship):
///   1. the recipient is on the fleet's own harness class — `security::probe_addr::harness_domain`,
///      which also catches an ad-hoc probe that never learned to send the header; or
///   2. the tenant carries a `probe_harness` marker — the value the `X-Swift-Harness` header wrote
///      at creation (`auth::handlers::harness_marker`, migration 099), which is what silences a
///      probe that addressed a routable domain this list does not know about.
///
/// The account itself is STILL created, with its generated password returned to the caller exactly
/// as before — only the mail is withheld. A real customer (no header, a domain of their own) reaches
/// the send below byte-identically to before this change.
pub async fn send_credentials_email(
    db: &PgPool,
    tenant_id: Uuid,
    tenant_name: &str,
    email: &str,
    name: &str,
    password: &str,
) -> bool {
    if let Some(reason) = credentials_send_suppression(db, tenant_id, email).await {
        tracing::info!(
            email = %email,
            tenant = %tenant_id,
            reason = %reason,
            "credentials email SUPPRESSED — probe/harness account. The account is real and the \
             generated password was returned to the caller; no mail was queued."
        );
        return false;
    }
    let vars = serde_json::json!({
        "name": name,
        "email": email,
        "password": password,
        "account_name": tenant_name,
        "app_url": "https://app.coreswiftcrm.com",
    });
    match crate::email::send_template_email(db, tenant_id, email, "welcome", &vars).await {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = %e, "Welcome email via template failed");
            false
        }
    }
}

/// Why this credentials mail must NOT be queued, or `None` when it must.
///
/// Read-only and deliberately fail-OPEN on the marker arm: if `tenants.probe_harness` cannot be
/// read, the send proceeds. A database hiccup must never be the reason a real customer never
/// receives their password — the address arm above it needs no query at all, and the transport
/// chokepoint (`communications::providers::deliver_email`) refuses the class again before any
/// provider traffic, so a marker read that fails open still cannot mail a harness address.
async fn credentials_send_suppression(db: &PgPool, tenant_id: Uuid, email: &str) -> Option<String> {
    if let Some(domain) = crate::security::probe_addr::harness_domain(email) {
        return Some(format!("fleet harness domain {domain}"));
    }
    match sqlx::query_scalar::<_, Option<String>>("SELECT probe_harness FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(db)
        .await
    {
        Ok(Some(Some(marker))) if !marker.trim().is_empty() => {
            Some(format!("tenants.probe_harness = '{marker}'"))
        }
        // An ordinary account: no marker, no row, or a NULL/blank marker.
        Ok(_) => None,
        Err(e) => {
            tracing::warn!(
                error = %e,
                tenant = %tenant_id,
                "could not read tenants.probe_harness; sending credentials normally"
            );
            None
        }
    }
}

/// The ONE predicate for "this app has a free plan with this slug". Shared by the seat below, the
/// account door's pre-check and the admin console's validation, so the three can never disagree
/// about what a free entry plan is.
pub(crate) const ENTRY_PLAN_SQL: &str =
    "SELECT id FROM plans WHERE slug = $1 AND is_active = true AND price_monthly = 0 LIMIT 1";

/// The plan a mint will be seated on, or `None` when this app has no such free, active plan (the
/// mint must then be refused with 422 rather than seated on something paid).
pub async fn entry_plan_id(db: &PgPool, entry_plan_slug: &str) -> Result<Option<Uuid>, AppError> {
    Ok(sqlx::query_scalar(ENTRY_PLAN_SQL)
        .bind(entry_plan_slug)
        .fetch_optional(db)
        .await?)
}

/// Seat the entry plan on a workspace, refusing anything that is not a free, active plan.
///
/// The predicate is the app's OWN (`plans.slug` + `is_active` + `price_monthly = 0`) — a sibling
/// app's plan name can never resolve here, and a paid tier cannot be seated by a setting. The
/// statement is the one the signup seat writes, and `tenant_plans_tenant_id_key` is a real UNIQUE
/// constraint, so `ON CONFLICT (tenant_id) DO NOTHING` never overwrites an existing row.
pub async fn seat_entry_plan(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    entry_plan_slug: &str,
) -> Result<(), AppError> {
    let plan_id: Option<Uuid> = sqlx::query_scalar(ENTRY_PLAN_SQL)
        .bind(entry_plan_slug)
        .fetch_optional(&mut *conn)
        .await?;

    let Some(plan_id) = plan_id else {
        return Err(AppError::Validation(format!(
            "No free plan '{entry_plan_slug}' is configured in this app: a plan with that slug, \
             is_active = true and price_monthly = 0 is required"
        )));
    };

    sqlx::query(
        r#"INSERT INTO tenant_plans (tenant_id, plan_id, status, billing_cycle)
           VALUES ($1, $2, 'active', 'monthly')
           ON CONFLICT (tenant_id) DO NOTHING"#,
    )
    .bind(tenant_id)
    .bind(plan_id)
    .execute(&mut *conn)
    .await?;

    Ok(())
}

/// A caller-supplied workspace field that IS a customisation: trimmed, and `None` when the caller
/// sent nothing but whitespace.
///
/// The same optional-string shape the app already uses elsewhere
/// (`option.as_deref().map(str::trim).filter(|s| !s.is_empty())`), lifted out so both halves of
/// [`tenant_identity`] judge a value identically.
fn supplied(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// The workspace's name and slug, exactly as the signup has always derived them — except that a
/// supplied field which is not a customisation falls back to the derived one (kanban t_2e40c9a5).
///
/// `owner_name` is the CALLER'S name already trimmed by `create_account` (kanban t_0ab34bd3) —
/// the auto arm interpolates it into `"<name>'s Workspace"`, so `"  Bob  "` mints
/// `Bob's Workspace` and never `"  Bob  's Workspace"`.
///
/// `account_name`/`account_slug` are OPTIONAL and both used to be bound VERBATIM when supplied, so
/// `POST /api/auth/register` with `"account_name": "   "` stored `tenants.name = "   "` — a
/// workspace with a blank label in the tenant list. (`users.name` was never affected: the owner is
/// a real name. Cosmetic, but a label nobody can read.) Both halves are now trimmed, and a half
/// that trims away is not treated as "a workspace called nothing" but as NOT SUPPLIED: the name
/// falls back to `"<owner>'s Workspace"`, the slug to the address-derived one. One rule per column,
/// so a custom name is never thrown away because the slug was omitted (the old tuple match did
/// exactly that), and no caller that used to get a `201` now gets a refusal — a blank explicit
/// value is a caller saying "no preference", which is precisely what the auto arm is for.
fn tenant_identity(req: &MintRequest<'_>, owner_name: &str) -> (String, String) {
    let name = supplied(req.account_name)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}'s Workspace", owner_name));

    let slug = supplied(req.account_slug)
        .map(str::to_string)
        .unwrap_or_else(|| {
            let local_part = req.email.split('@').next().unwrap_or("user");
            format!("{}-{}", local_part, &Uuid::new_v4().to_string()[..8])
        });

    (name, slug)
}

/// Read a workspace's display fields by id.
async fn load_tenant(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(String, String, Option<bool>), AppError> {
    sqlx::query_as::<_, (String, String, Option<bool>)>(
        "SELECT name, slug, is_active FROM tenants WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Workspace {} not found", id)))
}

#[cfg(test)]
mod tests {
    use super::{supplied, tenant_identity, validated_owner_name, MintRequest};
    use crate::errors::AppError;

    /// Every shape of "no name" a door can present — including the exact two the parent card
    /// measured live (`"   "` and `"\t\n "`) — is refused with the ONE shared message. This is
    /// door-independent by construction: both `/api/auth/register` and
    /// `/api/v1/internal/provision-free-account` refuse through this same function.
    #[test]
    fn a_whitespace_only_owner_name_is_refused_with_the_shared_message() {
        for blank in [
            "",
            " ",
            "   ",
            "\t",
            "\n",
            "\t\n ",
            "\u{a0}",
            " \u{2003} ",
            "\r\n",
        ] {
            match validated_owner_name(blank) {
                Err(AppError::Validation(message)) => {
                    assert_eq!(message, "name: is required", "input {blank:?}");
                }
                other => panic!("{blank:?} must be refused, got {other:?}"),
            }
        }
    }

    /// The trim reaches the value, not just the gate: this is what `users.name` binds and what
    /// `tenant_identity` interpolates into `"<name>'s Workspace"`.
    #[test]
    fn a_padded_owner_name_is_trimmed_for_the_store_and_the_label() {
        assert_eq!(validated_owner_name("  Bob  ").unwrap(), "Bob");
        assert_eq!(validated_owner_name("\tBob\n").unwrap(), "Bob");
        assert_eq!(validated_owner_name("Bob").unwrap(), "Bob");
        assert_eq!(validated_owner_name("\u{a0}Bob\u{a0}").unwrap(), "Bob");
    }

    /// The same family, the OTHER input (kanban t_2e40c9a5): a whitespace-only `account_name` is
    /// not a customisation. The old bytes stored it verbatim, so the workspace label in the tenant
    /// list was blank. Every blank shape falls back to the derived label instead, and — because the
    /// halves are judged independently — the caller keeps the slug they did supply.
    #[test]
    fn a_whitespace_only_account_name_cannot_become_the_workspace_label() {
        for blank in ["", " ", "   ", "\t\n ", "\u{a0}", " \u{2003} "] {
            let (name, slug) = identity(Some(blank), Some("probe-x"));
            assert_eq!(
                name, "Bob's Workspace",
                "a blank account_name ({blank:?}) must not be the stored label"
            );
            assert_eq!(name.trim(), name, "{blank:?} left padding on the label");
            assert!(!name.trim().is_empty(), "{blank:?} produced a blank label");
            assert_eq!(
                slug, "probe-x",
                "a blank name must not cost the caller their slug"
            );
        }
    }

    /// The control for the test above: a real pair IS the customisation, and it is trimmed on both
    /// halves exactly like `users.name`.
    #[test]
    fn a_supplied_pair_is_honoured_and_trimmed() {
        assert_eq!(
            identity(Some("  Acme Ltd  "), Some("  acme  ")),
            ("Acme Ltd".to_string(), "acme".to_string())
        );
    }

    /// One rule per column: the old tuple match fell to the auto arm and THREW AWAY a supplied
    /// custom name whenever the slug was omitted. The name now survives, and the slug is derived
    /// from the address.
    #[test]
    fn a_custom_name_survives_a_missing_slug() {
        let (name, slug) = identity(Some("Acme Ltd"), None);
        assert_eq!(name, "Acme Ltd");
        assert!(
            slug.starts_with("probe-x-"),
            "address-derived slug, got {slug:?}"
        );
        assert_eq!(slug.trim(), slug);
    }

    /// The untouched control: nothing supplied (and the same the other way round) derives both
    /// fields exactly as the signup always did.
    #[test]
    fn a_missing_pair_still_derives_both_fields() {
        let (name, slug) = identity(None, None);
        assert_eq!(name, "Bob's Workspace");
        assert!(
            slug.starts_with("probe-x-"),
            "address-derived slug, got {slug:?}"
        );
        assert_eq!(slug.len(), "probe-x-".len() + 8);

        let (name, slug) = identity(None, Some("  acme "));
        assert_eq!(name, "Bob's Workspace");
        assert_eq!(slug, "acme");
    }

    /// One mint request, with only the two optional account fields varying.
    fn identity(account_name: Option<&str>, account_slug: Option<&str>) -> (String, String) {
        tenant_identity(
            &MintRequest {
                email: "probe-x@example.invalid",
                name: "Bob",
                password: "generated-by-the-server",
                account_name,
                account_slug,
                invite_token: None,
                into_tenant: None,
                entry_plan_slug: "free",
                harness: None,
            },
            "Bob",
        )
    }
}
