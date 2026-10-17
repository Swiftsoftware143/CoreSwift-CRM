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
    if is_portfolio.unwrap_or(false) {
        return Err(AppError::Conflict(format!(
            "refusing to delete the workspace '{name}' ({slug}): it is flagged is_portfolio and is never removed"
        )));
    }

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
