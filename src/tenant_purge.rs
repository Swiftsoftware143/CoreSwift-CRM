//! `purge` — the ONE writer that destroys a workspace (kanban t_c88f907f).
//!
//! WHY THIS EXISTS
//!
//! Multi-Directory provisions a CoreSwift workspace for every directory/network it creates
//! (`multi-directory/src/coreswift.rs::provision_tenant` → `POST /api/auth/register` with
//! `md-<slug>@local.coreswift`). Its delete door could not take that workspace back: the only
//! delete in this app was `DELETE /api/account/:id`, which demands an `agency_admin` JWT —
//! a credential a sibling fleet app does not have. So a deleted directory left a live CoreSwift
//! workspace in the operator's tenant list forever, removable only by a fleet tool
//! (`/opt/swift/scripts/probe-identity-cleanup.py`), not by the product.
//!
//! THE DECISION (and why it is a HARD delete, not an archive)
//!
//! * `tenants.is_active = false` DOES hide a workspace from `account::handlers::list`
//!   (`WHERE is_active=true`), but it leaves the row behind, and `users.email` carries a GLOBAL
//!   unique index (`users_email_key`) while `tenants.slug` is unique too — so an archived
//!   workspace blocks re-provisioning the SAME directory slug later (the register call would
//!   fail on the duplicate address/slug). Re-creating a directory that was once deleted is an
//!   ordinary operator action, so archive is the wrong trade.
//! * Every one of the 70 foreign keys pointing at `tenants` is `ON DELETE CASCADE`
//!   (read from `pg_constraint`, 2026-10-17), so a hard delete is one statement that cannot
//!   leave an orphan half behind: the workspace's users, contacts, lists, tags, pipelines,
//!   messages, phone numbers, bookings, plans and audit rows all go with it.
//! * That is also the product sentence the operator already accepted: Multi-Directory's own
//!   delete door says "delete directory and all data". The workspace is the machine mirror of
//!   that directory, so "all data" includes it.
//!
//! GUARDS — a shared fleet key that can delete any workspace is a liability until it refuses.
//! Both doors share [`PROTECTED_MARKERS`]; the fleet door adds an allow-list so the shared key
//! can only ever destroy a workspace that is provably a machine-provisioned mirror:
//!
//! 1. a portfolio/owner account (name or slug carrying an owner marker, or `is_portfolio`) is
//!    never deleted — on EITHER door;
//! 2. the caller must echo the workspace's own slug (`?slug=`) — a stale or mistyped id gets a
//!    409 instead of destroying the wrong workspace;
//! 3. the workspace must carry at least one fleet-harness user (`@local.coreswift`,
//!    `@swiftsoftware.dev`, … — the class `security::probe_addr` already owns) — a real
//!    customer's workspace has none, so the shared key cannot reach it;
//! 4. no REAL user may sign in to it. A human who was invited into a machine workspace would
//!    lose their CRM with it, so the delete is refused and the remedy named.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;
use crate::security::probe_addr::harness_domain;

/// Accounts that are never removed, whatever door asks (David's standing rule: the sister
/// companies always have an account). Matched against the workspace's NAME and SLUG only —
/// never its users' addresses, because every Multi-Directory workspace is addressed
/// `…@local.coreswift` and a `"coreswift"` marker would then protect all of them.
pub const PROTECTED_MARKERS: &[&str] = &[
    "coreswift",
    "swiftsoftware",
    "certifiedtb143",
    "swiftimpact",
    "zaarhub",
    "giraudy",
    "probe-canary",
];

/// Is this workspace one of the portfolio companies, according to the REGISTRY the hub's own
/// portfolio sync writes (`portfolio_companies`)?
///
/// The [`PROTECTED_MARKERS`] list above is a FALLBACK for a row the hub has not mirrored yet; it is
/// not the authority. Measured live 2026-11-15: the registry held FIVE companies — BV Premiere
/// Products, Giraudy Capital, SwiftImpact Solutions, Xride Drivers, ZaarHub — the marker list named
/// only some of them, and `tenants.is_portfolio` was true for just ONE (Xride Drivers). BV Premiere
/// Products and Xride Drivers matched NO marker, so a `DELETE` of either id through
/// `account::delete` passed every guard and removed the row — an owner's company destroyed by one
/// click in the console.
///
/// The registry row links to the company's workspace through `account_tenant_id` (written by
/// `portfolio::provision`), and the hub's own tenant through `tenant_id` — the latter is the same
/// row for every company, so matching on it alone would protect the hub and NONE of the companies.
/// Name and slug are matched too (case-insensitively), so a mirror whose link column is still empty
/// is protected the moment its name appears in the registry.
///
/// Reading the registry is what makes a company the sync mirrors TOMORROW protected without anyone
/// editing a constant (the FunnelSwift shape, db393bf).
pub async fn portfolio_registry_hit(
    db: &PgPool,
    tenant_id: Uuid,
    name: &str,
    slug: &str,
) -> Result<Option<String>, AppError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT COALESCE(name, '') FROM portfolio_companies \
          WHERE account_tenant_id = $1 OR tenant_id = $1 \
             OR lower(name) = lower($2) OR lower(slug) = lower($3) \
          ORDER BY COALESCE(account_tenant_id = $1, false) DESC, (tenant_id = $1) DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(name)
    .bind(slug)
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some((n,)) if !n.trim().is_empty() => Some(n),
        Some(_) => Some("portfolio registry".to_string()),
        None => None,
    })
}

/// Which door asked for the destruction. The guards differ; the delete does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// `DELETE /api/account/:id` — an `agency_admin`'s own JWT (the operator console).
    AdminConsole,
    /// `DELETE /api/internal/tenants/:id` — a sibling fleet app holding `INTERNAL_SYNC_KEY`.
    Fleet,
}

/// What was destroyed, reported to the caller so the deletion is auditable from the other side.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PurgeReport {
    pub status: &'static str,
    pub mode: &'static str,
    pub tenant_id: Uuid,
    pub name: String,
    pub slug: String,
    pub users_removed: i64,
    pub contacts_removed: i64,
    pub lists_removed: i64,
}

impl PurgeReport {
    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

/// The owner marker `name`/`slug` carries, if any.
pub fn protected_marker(name: &str, slug: &str) -> Option<&'static str> {
    let hay = format!("{name} {slug}").to_ascii_lowercase();
    PROTECTED_MARKERS
        .iter()
        .copied()
        .find(|marker| hay.contains(marker))
}

/// Split a workspace's users by ADDRESS CLASS: `(machine, human)`.
///
/// "machine" is an address the fleet's own harness class owns (`security::probe_addr`): no
/// message to it can reach a real inbox, so no person signs in with it. Anything else is a
/// human — an unparseable address counts as human on purpose (fail closed).
pub fn split_by_address_class(emails: &[String]) -> (usize, usize) {
    let machine = emails
        .iter()
        .filter(|email| harness_domain(email).is_some())
        .count();
    (machine, emails.len() - machine)
}

/// The slug-confirmation rule, as a pure function so it is unit-testable.
///
/// Driven by the DOOR, not by the caller: the fleet door cannot opt out by OMITTING the parameter.
/// Measured in the first live run of this card's probe — treating `None` as "the console's
/// deliberate omission" let an unconfirmed fleet `DELETE` destroy the workspace, because the query
/// extractor hands a missing `?slug=` over as `None`.
fn check_confirmation(
    door: Door,
    confirmed: Option<&str>,
    slug: &str,
    id: Uuid,
) -> Result<(), String> {
    const REQUIRED: &str = "refusing to delete: the ?slug=<workspace slug> confirmation is required, so a stale id cannot destroy the wrong workspace";
    match (door, confirmed) {
        (Door::Fleet, None) => Err(REQUIRED.to_string()),
        (Door::AdminConsole, None) => Ok(()),
        (_, Some(expected)) => {
            let expected = expected.trim();
            if expected.is_empty() {
                Err(REQUIRED.to_string())
            } else if expected != slug {
                Err(format!(
                    "refusing to delete: workspace {id} has slug '{slug}', the caller confirmed '{expected}'"
                ))
            } else {
                Ok(())
            }
        }
    }
}

/// What was destroyed and what was kept, reported to the caller so an erasure is auditable.
#[derive(Debug, Clone, serde::Serialize)]
pub struct EraseReport {
    pub status: &'static str,
    pub mode: &'static str,
    pub tenant_id: Uuid,
    pub users_removed: i64,
    pub contacts_removed: i64,
    pub lists_removed: i64,
    /// Billing rows copied to `erasure_billing_retention` before the cascade (tax retention).
    pub billing_documents_retained: i64,
    pub audit_id: Uuid,
    pub erased_at: chrono::DateTime<chrono::Utc>,
}

impl EraseReport {
    pub fn to_json(&self) -> Value {
        json!(self)
    }
}

/// The guards EVERY destroy door applies, in one place: a portfolio/owner company is never
/// removed. Returns the workspace's `(name, slug)` once it passed.
///
/// Extracted so the self-service `erase_tenant` and the operator/fleet `delete_tenant` cannot
/// drift apart — `guarded-destructive-actions`: a guard on one door is not a guard.
async fn guarded_workspace(db: &PgPool, id: Uuid) -> Result<(String, String), AppError> {
    let (name, slug, is_portfolio) = sqlx::query_as::<_, (String, String, Option<bool>)>(
        "SELECT name, slug, is_portfolio FROM tenants WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Workspace {id} not found")))?;

    // ── 1. never a portfolio/owner account, on any door ────────────────────────────────────────
    if let Some(marker) = protected_marker(&name, &slug) {
        return Err(AppError::Conflict(format!(
            "refusing to delete the workspace '{name}' ({slug}): it is a portfolio account ('{marker}') and is never removed"
        )));
    }
    // The registry the hub's portfolio sync writes is the AUTHORITY; the marker list above is a
    // fallback for a company it has not mirrored yet (measured live 2026-11-15: the registry held
    // five companies, `is_portfolio` was true for one, and two of them matched no marker — either
    // was deletable from the console in one click).
    if let Some(company) = portfolio_registry_hit(db, id, &name, &slug).await? {
        return Err(AppError::Conflict(format!(
            "refusing to delete the workspace '{name}' ({slug}): it is the portfolio company '{company}' in the registry and is never removed"
        )));
    }
    if is_portfolio.unwrap_or(false) {
        return Err(AppError::Conflict(format!(
            "refusing to delete the workspace '{name}' ({slug}): it is flagged is_portfolio and is never removed"
        )));
    }
    Ok((name, slug))
}

/// The customer's own irreversible erasure (`POST /api/auth/erase-data`).
///
/// The caller has already proved three things (session, password, the typed word) in the handler;
/// this function owns the DESTRUCTION and nothing else, through the same one-writer path as every
/// other door. It differs from [`delete_tenant`] in exactly two ways, both required by the fleet
/// standard §4:
///
///   * the billing documents (the plan the customer was on and its price history) are copied into
///     `erasure_billing_retention` INSIDE the same transaction, because the cascade would otherwise
///     destroy the records tax law requires us to keep;
///   * one `account_erasures` row is written in that same transaction, so a successful erase always
///     leaves exactly one audit row — and a failed one leaves none.
///
/// The workspace row itself is deleted by the same `DELETE FROM tenants` the other doors use, so
/// the 100-odd `ON DELETE CASCADE` foreign keys take users, sessions, API keys, contacts, deals,
/// tickets and the workspace's own audit trail with it in one statement.
pub async fn erase_tenant(
    db: &PgPool,
    id: Uuid,
    actor_user_id: Uuid,
    actor_email: &str,
) -> Result<EraseReport, AppError> {
    // Portfolio/owner accounts are never erased, whatever door asks (same guards as delete_tenant).
    guarded_workspace(db, id).await?;

    let mut tx = db.begin().await?;
    let users_removed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let contacts_removed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let lists_removed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lists WHERE tenant_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let plans_retained: i64 = sqlx::query(
        "INSERT INTO erasure_billing_retention (erased_tenant_id, source, document) \
         SELECT $1, 'tenant_plans', to_jsonb(t) FROM tenant_plans t WHERE t.tenant_id = $1 \
         RETURNING id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?
    .len() as i64;
    let lines_retained: i64 = sqlx::query(
        "INSERT INTO erasure_billing_retention (erased_tenant_id, source, document) \
         SELECT $1, 'plan_lines', to_jsonb(t) FROM plan_lines t WHERE t.tenant_id = $1 \
         RETURNING id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await?
    .len() as i64;
    let billing_documents_retained = plans_retained + lines_retained;

    let (audit_id, erased_at): (Uuid, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "INSERT INTO account_erasures \
             (erased_tenant_id, actor_user_id, actor_email, action, records) \
         VALUES ($1, $2, $3, 'erase_data', $4) \
         RETURNING id, erased_at",
    )
    .bind(id)
    .bind(actor_user_id)
    .bind(actor_email)
    .bind(json!({
        "users": users_removed,
        "contacts": contacts_removed,
        "lists": lists_removed,
        "billing_documents_retained": billing_documents_retained,
    }))
    .fetch_one(&mut *tx)
    .await?;

    let done = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() != 1 {
        return Err(AppError::NotFound(format!("Workspace {id} not found")));
    }
    tx.commit().await?;

    tracing::info!(
        "[purge] workspace ERASED door=OwnerErase tenant={id} actor={actor_user_id} users={users_removed} contacts={contacts_removed} lists={lists_removed} billing_docs_kept={billing_documents_retained} audit={audit_id}"
    );

    Ok(EraseReport {
        status: "erased",
        mode: "hard",
        tenant_id: id,
        users_removed,
        contacts_removed,
        lists_removed,
        billing_documents_retained,
        audit_id,
        erased_at,
    })
}

/// Destroy one workspace and everything its foreign keys cascade to.
///
/// `confirmed_slug` is the caller's echo of the workspace slug (the fleet door requires it; the
/// console door passes `None` — the operator picked the row in the UI).
pub async fn delete_tenant(
    db: &PgPool,
    id: Uuid,
    door: Door,
    confirmed_slug: Option<&str>,
) -> Result<PurgeReport, AppError> {
    let (name, slug) = guarded_workspace(db, id).await?;

    // ── 2. the caller must name the workspace it means ─────────────────────────────────────────
    check_confirmation(door, confirmed_slug, &slug, id).map_err(AppError::Conflict)?;

    // ── 3./4. the fleet door only ever reaches a machine-provisioned mirror ────────────────────
    if door == Door::Fleet {
        let emails: Vec<String> =
            sqlx::query_scalar("SELECT email FROM users WHERE tenant_id = $1")
                .bind(id)
                .fetch_all(db)
                .await?;
        let (machine, human) = split_by_address_class(&emails);
        if machine == 0 {
            return Err(AppError::Conflict(format!(
                "refusing to delete the workspace '{name}' ({slug}): no fleet-harness user signs in to it, so it is not a machine-provisioned workspace"
            )));
        }
        if human > 0 {
            return Err(AppError::Conflict(format!(
                "refusing to delete the workspace '{name}' ({slug}): {human} real user(s) sign in to it and would lose their CRM — remove those users first, or delete the workspace from the CoreSwift account console"
            )));
        }
    }

    // ── the delete: counts first (same transaction), then the one statement ────────────────────
    let mut tx = db.begin().await?;
    // One const statement per table (never a built string: the pre-deploy gate refuses
    // concatenated SQL, and a literal is auditable at a glance).
    let users_removed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE tenant_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let contacts_removed: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id = $1")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    let lists_removed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lists WHERE tenant_id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let done = sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if done.rows_affected() != 1 {
        return Err(AppError::NotFound(format!("Workspace {id} not found")));
    }
    tx.commit().await?;

    tracing::info!(
        "[purge] workspace deleted door={door:?} tenant={id} slug={slug} users={users_removed} contacts={contacts_removed} lists={lists_removed}"
    );

    Ok(PurgeReport {
        status: "deleted",
        mode: "hard",
        tenant_id: id,
        name,
        slug,
        users_removed,
        contacts_removed,
        lists_removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_portfolio_account_is_protected() {
        // The live account labels on 2026-10-17 (read from `tenants`).
        for (name, slug) in [
            ("SwiftSoftware", "coreswift"),
            ("ZaarHub Florida", "zaarhub-florida"),
            ("Giraudy Capital", "giraudy-capital"),
            ("SwiftImpact Solutions", "swiftimpact-solutions"),
            ("ZaarHub", "zaarhub"),
            ("Swift Admin's Workspace", "swiftsoftware143-0505f092"),
            ("David Giraudy's Workspace", "certifiedtb143-6af12981"),
            ("Tenancy Canary A", "probe-canary-a"),
        ] {
            assert!(
                protected_marker(name, slug).is_some(),
                "{name} / {slug} must be protected from deletion"
            );
        }
    }

    #[test]
    fn a_customer_or_machine_workspace_is_not_protected() {
        for (name, slug) in [
            ("Multi-Directory: Miami Dental", "miami-dental"),
            ("Reyan Talukdar's Workspace", "reyan1ngc0814-f0a9ce9d"),
            ("Marcus James Reed's Workspace", "marcusjreed-4e0036c2"),
        ] {
            assert!(
                protected_marker(name, slug).is_none(),
                "{name} / {slug} is not a portfolio account"
            );
        }
    }

    /// The rule the first live probe run broke: an OMITTED `?slug=` is not "no confirmation owed"
    /// on the fleet door. A guard that only arms when the caller supplies a value is not a guard.
    #[test]
    fn the_fleet_door_cannot_omit_the_slug_confirmation() {
        let id = Uuid::nil();
        assert!(check_confirmation(Door::Fleet, None, "md-x", id).is_err());
        assert!(check_confirmation(Door::Fleet, Some(""), "md-x", id).is_err());
        assert!(check_confirmation(Door::Fleet, Some("   "), "md-x", id).is_err());
        assert!(check_confirmation(Door::Fleet, Some("other-slug"), "md-x", id).is_err());
        assert!(check_confirmation(Door::Fleet, Some("md-x"), "md-x", id).is_ok());
        // The console door picked the row in the UI, so it owes no echo — and it is unaffected.
        assert!(check_confirmation(Door::AdminConsole, None, "md-x", id).is_ok());
        assert!(check_confirmation(Door::AdminConsole, Some("wrong"), "md-x", id).is_err());
    }

    #[test]
    fn the_multi_directory_convention_is_machine_class_and_a_human_not() {
        let emails = vec![
            "md-miami-dental@local.coreswift".to_string(),
            "probe-canary-a@swiftsoftware.dev".to_string(),
            "owner@yahoo.com".to_string(),
        ];
        assert_eq!(split_by_address_class(&emails), (2, 1));
        // An unparseable address counts as human (fail closed).
        assert_eq!(
            split_by_address_class(&["not-an-address".to_string()]),
            (0, 1)
        );
        assert_eq!(split_by_address_class(&[]), (0, 0));
    }
}
