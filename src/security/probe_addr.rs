//! `probe_addr` — recipient addresses a fleet harness owns, which must never be handed to a real
//! mail relay (kanban t_12a8d509, parity with the shape closed on the five sibling apps by
//! kanban t_36b55ed2).
//!
//! WHY THIS EXISTS
//!
//! This app was the ONLY fleet app without this class. Measured 2026-10-09 (Mailgun events for
//! `mail.coreswiftcrm.com`, the app's own platform transport):
//! `csproof-1791578449@swiftsoftware.dev` — the address the profile-UI proof mints through
//! `POST /api/auth/register` — shows `accepted` and then a real hard bounce.
//!
//! The send that did it is this app's own credentials mail: subject `Welcome to CoreSwift CRM!`,
//! `From: CoreSwift CRM Help Desk <no-reply@mail.coreswiftcrm.com>` (the app's `EMAIL_FROM`), a
//! `550 5.1.1 mailbox unavailable` from migadu. `swiftsoftware.dev` is a REAL, routable domain the
//! fleet owns, so a probe signup addressed there mails the operator's own inbox, and every one of
//! those messages also burns a delivery — and a bounce — on the sending domain's reputation.
//!
//! Probe residue is supposed to be attributed by DATA (policy:
//! `/opt/swift/docs/fleet-probe-residue-policy-2026-09-28.md`, which classes `swiftsoftware.dev` /
//! `.local` as FLEET-DEV — the harness class). This module is the outbound half of that: a signup
//! addressed into the harness class is still created normally, with its generated password returned
//! to the caller, but its credentials mail is never queued, so a probe can never reach an inbox.
//!
//! THERE ARE TWO DOORS AND THE CLASS IS CLOSED AT BOTH
//!
//!   * `auth::signup::send_credentials_email` — the ONE credentials send every door uses (public
//!     register, the tag → free-account door, the admin mint). It suppresses on the address class
//!     AND on the tenant marker below, so nothing is even queued;
//!   * `communications::providers::deliver_email` — the transport chokepoint, where every
//!     `outbound_messages` row is finally handed to a relay. Rows queued by any of the other nine
//!     `INSERT INTO outbound_messages` sites, by the worker's retry poll, or by a manual send are
//!     caught here.
//!
//! RESERVED CLASS — ALREADY SUPPRESSED BY ITS OWN ARM
//!
//! The RFC 2606 / 6761 names (`example.com/.net/.org`, `*.invalid`, `*.test`) appear in the list
//! below for the same reason as in the sibling apps: a send to them can only bounce. On this app
//! they are ALSO refused by `communications::providers::reserved_domain`, whose refusal runs FIRST
//! at the transport chokepoint and keeps its own message and its own pinned test (measured on the
//! live provider 2026-09-22: 16 accepted-then-498 rows inside five minutes, t_e4d94bd9). Listing
//! them here only means the credentials door no longer queues a row it already knows is
//! undeliverable — it does not change what that arm answers.
//!
//! ONE DELIBERATE EXCEPTION — `*.local` / `localhost`. Both stay OPEN. The fleet's content-level
//! credential harness points a provider at a local SMTP sink and signs up as
//! `<name>@probe.local`, then reads the credentials message off the wire; suppressing `.local`
//! would delete the only harness that can prove the send itself still works. Only the fleet's own
//! `swiftsoftware.local` identity is listed, never the bare `local` TLD. `local.coreswift` (the
//! multi-directory probe's convention, below) is NOT an exception to this: its TLD is `coreswift`,
//! not `local`, so the sink harness at `<name>@<anything>.local` stays open either way.
//!
//! THE OTHER HALF IS THE TENANT MARKER
//!
//! A harness that sends `X-Swift-Harness` marks its tenant (`tenants.probe_harness`, written by
//! `auth::handlers::harness_marker` at creation, migration 099), and the credentials door
//! suppresses on the marker as well — so a probe is silenced even when it addresses a routable
//! domain this list does not know about. Measured live 2026-10-10 before shipping: 37 tenants, 2
//! markers (`CNRY…` the tenancy canary, `walk`), and NEITHER belongs to a customer — so the marker
//! arm cannot silence a real account's credentials mail on this app today.
//!
//! MEASURED AGAIN 2026-10-14 — THE CALLER'S OWN CONVENTION (kanban t_d53f82ae)
//!
//! A second harness mints workspaces here without ever learning our list: the multi-directory
//! network-create smoke (`multi-directory/scripts/network-create-admin-smoke.py`) provisions a
//! CoreSwift workspace per run through `multi-directory/src/coreswift.rs`, which addresses it
//! `md-<slug>@local.coreswift`. That probe sends NO `X-Swift-Harness` header, so the marker arm is
//! NULL for every workspace it mints, and `local.coreswift` was not on this list — so BOTH arms
//! missed and each mintage took a real `Welcome to CoreSwift CRM!` delivery. Live count when this
//! was filed: 6 suppressed-but-sent messages (`outbound_messages`, status `sent`) to
//! `md-zz-nw-probe-<ts>@local.coreswift` / `md-zz-nw-probe-api-<ts>@local.coreswift`, 21:58–22:27
//! UTC, two per probe run, ~2 workspaces every ~15 min while the audit harness ran. The address
//! class is now listed below, which silences the mail at both doors that consult this module no
//! matter what the caller does. The residue itself (the workspaces it mints and never deletes) is
//! the caller's to clean up — that half is carded in the multi-directory lane, not fixable from here.

/// The fleet's own harness domains. Every one of them is fleet-controlled: a message addressed here
/// can only land in the fleet's own mailboxes, never in a customer's.
pub const FLEET_HARNESS_DOMAINS: &[&str] = &[
    "swiftsoftware.dev",
    "swiftsoftware.net",
    "swiftsoftware.local",
    // The multi-directory network-create probe's own convention (kanban t_d53f82ae): its caller
    // registers the workspace it provisions as `md-<slug>@local.coreswift` and sends no marker, so
    // this list is the only arm that can catch it. `coreswift` is not a TLD and nothing can own the
    // name — the same shape as the fleet's own identities above, not the deliberately-open `local`
    // sink. Measured live 2026-10-14: 6 real `Welcome to CoreSwift CRM!` deliveries to this class.
    "local.coreswift",
    "example.invalid",
    "example.com",
    "example.net",
    "example.org",
    "invalid",
    "test",
];

/// The fleet harness domain `addr` belongs to, if any. A subdomain counts
/// (`probe@mail.swiftsoftware.dev` is still the harness class); a domain that merely CONTAINS the
/// name does not (`x@notswiftsoftware.dev` is refused — the match is on the label boundary).
///
/// Everything is normalised first (trim, lowercase, trailing dot), so `Probe@SwiftSoftware.NET.`
/// classifies like the exact form. A value with no `@`, or with an empty domain, is not an address
/// and returns `None` — the caller decides what to do with an unparseable address, this function
/// only answers the harness question.
pub fn harness_domain(addr: &str) -> Option<&'static str> {
    let (_, domain) = addr.rsplit_once('@')?;
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if domain.is_empty() {
        return None;
    }
    FLEET_HARNESS_DOMAINS.iter().copied().find(|d| {
        domain == *d || (domain.len() > d.len() + 1 && domain.ends_with(&format!(".{d}")))
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// SYNTHETIC REQUESTERS — a harness must never page a human (kanban t_a27fd2a4)
//
// Fleet parity with the fix the five sibling apps took on 2026-11-10 (kanban
// t_f17b5802): the support-request and plan-request doors notify the OPERATOR by
// design, so the recipient is the operator whatever address the requester gave.
// CoreSwift was the remaining app without this predicate, measured 2026-11-12:
// a probe identity on a reserved address still paged the operator for every press.
// The row is the record and is still filed; only the ALERT to a human is withheld,
// the same treatment the credentials mail already gives an address that can only
// bounce. The predicate is on the REQUESTER, never on the route: the same door
// serves real customers, and a real customer's request must still reach a human.
// ─────────────────────────────────────────────────────────────────────────────

/// Domains that can only belong to a harness. `x.com` / `test.com` are added on top of the
/// RFC-reserved class because the fleet's own proof scripts address them and they are NOT
/// mail-suppressed (they are real, registrable domains), so nothing else would stop the alert.
pub const SYNTHETIC_REQUESTER_DOMAINS: &[&str] = &[
    "example.invalid",
    "example.test",
    "example.com",
    "x.com",
    "test.com",
];

/// Local parts a harness mints. `bounce+` is the shape a deliverability probe uses.
pub const SYNTHETIC_LOCAL_PREFIXES: &[&str] = &["probe-", "canary-", "bounce+", "verify"];

/// Workspace/account name fragments that name fleet machinery in words.
pub const SYNTHETIC_NAME_FRAGMENTS: &[&str] =
    &["probe", "canary", "synthetic", "smoke test", "test farm"];

/// Why `email`/`name` is a synthetic (harness) identity, or `None` for a real customer.
///
/// Returns the offending domain, local-part prefix or name fragment, so the caller can log a
/// reason instead of a bare boolean. Matches on the label boundary (`x@notexample.com` is a
/// customer), normalises case/whitespace/trailing root dot, and never treats an empty string
/// as a match.
pub fn synthetic_requester(email: &str, name: &str) -> Option<&'static str> {
    let addr = email.trim();
    // The fleet's own harness domains (swiftsoftware.dev/.net/.local) are already classified.
    if let Some(domain) = harness_domain(addr) {
        return Some(domain);
    }
    if let Some((local, domain)) = addr.rsplit_once('@') {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if !domain.is_empty() {
            if let Some(d) = SYNTHETIC_REQUESTER_DOMAINS.iter().copied().find(|d| {
                domain == *d || (domain.len() > d.len() + 1 && domain.ends_with(&format!(".{d}")))
            }) {
                return Some(d);
            }
        }
        let local = local.trim().to_ascii_lowercase();
        if let Some(p) = SYNTHETIC_LOCAL_PREFIXES
            .iter()
            .copied()
            .find(|p| local.starts_with(p))
        {
            return Some(p);
        }
    }
    let name = name.trim().to_ascii_lowercase();
    if !name.is_empty() {
        if let Some(f) = SYNTHETIC_NAME_FRAGMENTS
            .iter()
            .copied()
            .find(|f| name.contains(f))
        {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod synthetic_requester_tests {
    use super::*;

    #[test]
    fn harness_identities_are_named() {
        // A probe on a reserved domain, and the shapes the fleet's walks actually mint.
        assert_eq!(
            synthetic_requester("probe-pay-1794477000@example.invalid", "Probe Walk"),
            Some("example.invalid")
        );
        assert_eq!(synthetic_requester("p3-smoke@x.com", "x"), Some("x.com"));
        assert_eq!(
            synthetic_requester("csproof-1791578449@swiftsoftware.dev", "Walk"),
            Some("swiftsoftware.dev")
        );
        // A local-part prefix alone is enough, even on a routable domain.
        assert_eq!(
            synthetic_requester("canary-abc@albert-tire.com", ""),
            Some("canary-")
        );
        // A workspace whose NAME says it is machinery.
        assert_eq!(
            synthetic_requester("someone@gmail.com", "Probe Workspace"),
            Some("probe")
        );
    }

    #[test]
    fn a_real_customer_is_never_silenced() {
        // Measured real operators/customers on this install.
        assert_eq!(synthetic_requester("swiftsoftware143@yahoo.com", ""), None);
        assert_eq!(
            synthetic_requester("owner@albert-tire.com", "Albert Tire"),
            None
        );
        assert_eq!(
            synthetic_requester("marcusjreed@uberip.com", "Marcus Reed"),
            None
        );
        // Label boundary: a domain that merely CONTAINS a listed one is a customer.
        assert_eq!(synthetic_requester("x@notexample.com", ""), None);
        assert_eq!(synthetic_requester("x@x.com.evil.net", ""), None);
        // Empty input is not a match.
        assert_eq!(synthetic_requester("", ""), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fleet_harness_domains_are_detected() {
        // The addresses this app's own harnesses actually mint (measured off mail.coreswiftcrm.com)
        // and the standing tenancy canary's two probe users, read out of the live tenants table.
        assert_eq!(
            harness_domain("csproof-1791578449@swiftsoftware.dev"),
            Some("swiftsoftware.dev")
        );
        assert_eq!(
            harness_domain("probe-canary-a@swiftsoftware.dev"),
            Some("swiftsoftware.dev")
        );
        assert_eq!(
            harness_domain("zzprobe-profile-1791480727@swiftsoftware.net"),
            Some("swiftsoftware.net")
        );
        assert_eq!(
            harness_domain("x@swiftsoftware.local"),
            Some("swiftsoftware.local")
        );
        // The multi-directory network-create probe's own convention (kanban t_d53f82ae): the exact
        // shape its caller registers, and the one measured taking a real `Welcome to CoreSwift CRM!`
        // delivery on 2026-10-14 because neither arm matched it.
        assert_eq!(
            harness_domain("md-zz-nw-probe-1792016864@local.coreswift"),
            Some("local.coreswift")
        );
        assert_eq!(
            harness_domain("md-zz-nw-probe-api-1792016864@local.coreswift"),
            Some("local.coreswift")
        );
        // A subdomain of it is the same class, and the normalisation above applies to it too.
        assert_eq!(
            harness_domain(" MD-ZZ-NW-PROBE-1@Local.CoreSwift. "),
            Some("local.coreswift")
        );
        // Case, whitespace and a trailing root dot are normalised away.
        assert_eq!(
            harness_domain(" Probe@SwiftSoftware.NET. "),
            Some("swiftsoftware.net")
        );
        // A subdomain is still the harness class.
        assert_eq!(
            harness_domain("probe@mail.swiftsoftware.dev"),
            Some("swiftsoftware.dev")
        );
        // Reserved RFC 2606 / 6761 names are in the class too: a send to them can only bounce, and
        // bounces are how a sending domain gets disabled.
        assert_eq!(harness_domain("x@example.com"), Some("example.com"));
        assert_eq!(harness_domain("probe@example.org"), Some("example.org"));
        assert_eq!(harness_domain("probe@example.net"), Some("example.net"));
        assert_eq!(harness_domain("x@foo.invalid"), Some("invalid"));
        assert_eq!(harness_domain("x@foo.test"), Some("test"));
    }

    #[test]
    fn real_customer_addresses_are_not_suppressed() {
        // A customer's mailbox, and the fleet's own REAL brand domains: the policy classes
        // `coreswiftcrm.com` and `swiftsoftware.com` as human-possible, and the owner's proof inbox
        // must keep taking live mail — the mail-delivery watchdog sends to it every run.
        assert_eq!(harness_domain("certifiedtb143@yahoo.com"), None);
        assert_eq!(harness_domain("swiftsoftware143@yahoo.com"), None);
        assert_eq!(harness_domain("someone@gmail.com"), None);
        assert_eq!(harness_domain("david@swiftsoftware.com"), None);
        // The app's footer no longer names a MAILBOX at all (kanban t_17277fd5): every address this
        // product has printed — `support@coreswiftcrm.com`, then `support@swiftsoftware.net` — sat
        // on a domain publishing no MX, so it names the served contact page instead. `coreswiftcrm.com`
        // stays a real, routable brand domain.
        assert_eq!(harness_domain("hello@coreswiftcrm.com"), None);
        // A domain that merely CONTAINS a harness domain is not a match.
        assert_eq!(harness_domain("x@notswiftsoftware.dev"), None);
        assert_eq!(harness_domain("x@swiftsoftware.dev.evil.com"), None);
        // `*.local` stays OPEN on purpose (the local SMTP-sink credential harness needs it — see
        // the module doc). Everything else reserved is suppressed.
        assert_eq!(harness_domain("cr1sink0a1b@probe.local"), None);
        assert_eq!(harness_domain("x@probe.local"), None);
        // ...and `local.coreswift` is its OWN name, not the `local` TLD: a lookalike that merely
        // contains it, or that carries it as a subdomain of something else, is not the class.
        assert_eq!(harness_domain("md-x@coreswift"), None);
        assert_eq!(harness_domain("md-x@notlocal.coreswift"), None);
        assert_eq!(harness_domain("md-x@local.coreswift.evil.com"), None);
        assert_eq!(harness_domain("md-x@probe.local.coreswift.evil.com"), None);
    }

    #[test]
    fn malformed_values_never_classify() {
        assert_eq!(harness_domain("no-at-sign"), None);
        assert_eq!(harness_domain(""), None);
        // Domain-only classification: an empty local part is still the harness domain. Such an
        // address never gets this far (the signup normalises and rejects it first), and suppressing
        // is the safe direction if one ever did.
        assert_eq!(
            harness_domain("@swiftsoftware.dev"),
            Some("swiftsoftware.dev")
        );
        assert_eq!(harness_domain("trailing@"), None);
    }
}
