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
///
/// `swiftsoftware.com` (added 2026-11-18, parity with ADASwift `bd74ae1`) is the fleet's own BRAND
/// domain and belongs here for the same reason — no customer can own an address there, so a request
/// from one is fleet machinery by construction. It is deliberately NOT in
/// [`FLEET_HARNESS_DOMAINS`]: that list suppresses MAIL, the owner's proof inbox lives on this
/// domain, and the welcome/credential mail must keep being delivered. Measured on this app's
/// deployed binary 2026-11-18 before the fix: a signup on `realcustomer-<ts>@swiftsoftware.com`
/// with a plan pick wrote one operator page ("A support request needs an answer: Plan request at
/// signup: Professional plan"). Note this domain is deliberately in the fleet residue classifier's
/// `HUMAN_DOM` ("a human owns these rows, never swept"), so no cleanup arm will ever remove what a
/// probe minted here — this alert predicate is the only half that can silence it.
pub const SYNTHETIC_REQUESTER_DOMAINS: &[&str] = &[
    "example.invalid",
    "example.test",
    "example.com",
    "x.com",
    "test.com",
    "swiftsoftware.com",
];

/// Local parts a harness mints. `bounce+` is the shape a deliverability probe uses.
pub const SYNTHETIC_LOCAL_PREFIXES: &[&str] = &["probe-", "canary-", "bounce+", "verify"];

/// Local-part stems the fleet's own probes have ACTUALLY used on routable customer domains
/// (measured 2026-11-26, kanban t_3ce27efb): `BCPROBE3A@coreswiftcrm.com`,
/// `captureaudit<hex>@coreswiftcrm.com`, `ceo-rule-probe@zaarhub.com`,
/// `probe+temperature-trigger@swiftimpactsolutions.com`.
///
/// Deliberately SEPARATE from [`SYNTHETIC_LOCAL_PREFIXES`]: that list is the CREATION gate
/// (`synthetic_requester` refuses to mint a CRM contact from such an address), and the
/// booking-capture harness must be able to mint the very contact it is testing. These stems only
/// ATTRIBUTE a row as a test row ([`probe_contact_addr`], used by the spoke ingest label and the
/// helpdesk junk arm). Matched case-insensitively because the local part is lowercased first.
pub const TEST_ROW_LOCAL_PREFIXES: &[&str] = &[
    "bcprobe",
    "captureaudit",
    "zzprobe",
    "ceo-rule-probe",
    "probe+",
];

/// The fleet's BRAND domain — the one domain in [`SYNTHETIC_REQUESTER_DOMAINS`] where the fleet's
/// own humans hold real mailboxes. A probe identity on it is still machinery, but it has to be
/// named by SHAPE (`realcustomer-<unix-ts>`), never by the domain alone (see
/// [`probe_contact_addr`]).
pub const BRAND_DOMAIN: &str = "swiftsoftware.com";

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

/// Is `addr` a machine identity a fleet harness PUSHES into the CRM — a probe contact — rather
/// than a customer? ADDRESS-ONLY, and that is the point: [`synthetic_requester`] also matches
/// NAME fragments, so passing the pushed name would tag a real contact called "Probe Farm".
///
/// This is the hub's pipeline-facing classifier (kanban t_538195c3). The ADASwift operator tenant
/// is the ONLY path a real ADASwift signup is pushed through, so a probe cannot be excluded at the
/// tenant or blocked at the spoke; the hub labels the ROW instead, which covers every spoke
/// (present and future) with one rule.
///
/// It is deliberately NOT a bare [`synthetic_requester`] answer, because that predicate is the
/// ALERT one and its domain list carries the brand domain (`swiftsoftware.com`) so fleet mail
/// never pages a human. In the CRM the brand domain is where the fleet's own people hold real
/// mailboxes — `david@swiftsoftware.com` is the owner and must stay a customer row — so there the
/// identity has to be machine-SHAPED: `realcustomer-<unix-ts>@swiftsoftware.com`, the identity
/// ADASwift's signup probe pushes.
pub fn probe_contact_addr(addr: &str) -> bool {
    let addr = addr.trim();
    let Some((local, domain)) = addr.rsplit_once('@') else {
        return false;
    };
    let local = local.trim().to_ascii_lowercase();
    let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
    if local.is_empty() || domain.is_empty() {
        return false;
    }

    // 1. A machine-minted local part names harness machinery on ANY domain — the fleet's own proof
    //    scripts address real, registrable domains (`probe-x@maxxspace.com`), and the stems this
    //    fleet actually used (`bcprobe`, `captureaudit`, `zzprobe`, `ceo-rule-probe`, `probe+`) are
    //    listed separately so the CREATION gate keeps letting the capture harness mint its contact.
    if SYNTHETIC_LOCAL_PREFIXES
        .iter()
        .chain(TEST_ROW_LOCAL_PREFIXES.iter())
        .any(|p| local.starts_with(p))
    {
        return true;
    }
    // 2. The harness MAIL class: no customer can hold an address there.
    if harness_domain(addr).is_some() {
        return true;
    }
    // 3. The brand domain — humans live here, machinery has a shape.
    if domain_is(&domain, BRAND_DOMAIN) {
        return machine_minted_local(&local);
    }
    // 4. The rest of the alert predicate's domain class (`x.com`, `test.com`, `example.test`): the
    //    fleet's proof scripts address those, and no captured lead arrives from one. The brand
    //    domain is handled at 3, never here.
    SYNTHETIC_REQUESTER_DOMAINS
        .iter()
        .any(|d| domain_is(&domain, d))
}

/// `domain` is `d` or a subdomain of it — matched on the LABEL boundary, so
/// `x@notswiftsoftware.com` is not on the brand domain.
fn domain_is(domain: &str, d: &str) -> bool {
    domain == d || (domain.len() > d.len() + 1 && domain.ends_with(&format!(".{d}")))
}

/// `<word>-<at least 8 digits>`: a timestamp on a machine-minted mailbox
/// (`realcustomer-1795045484`), never a person (`john-1985`).
fn machine_minted_local(local: &str) -> bool {
    match local.rsplit_once('-') {
        Some((stem, tail)) => {
            !stem.is_empty() && tail.len() >= 8 && tail.bytes().all(|b| b.is_ascii_digit())
        }
        None => false,
    }
}

/// Fictitious numbers that can never receive a text.
///
/// NANP reserves exchange `555` with subscriber `0100`–`0199` for fiction — `813-555-0101`,
/// `305-555-0199`. No carrier will ever deliver to one, so handing the number to a text provider
/// buys nothing but a failed send charged against the same account that carries real customers'
/// texts. The mail layer already withholds unreachable addresses ([`harness_domain`]); this is the
/// same gate applied to the number (parity with XrideDispatch commit 2ede6cb, where the hole was
/// measured live in `notification_log`).
///
/// Deliberately TIGHT: only the reserved fiction block. `813-555-1234` is a legitimate number, so
/// the exchange alone is not enough — the subscriber has to be in 0100–0199 as well.
pub fn undeliverable_number(number: &str) -> Option<&'static str> {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    let digits = digits.trim_start_matches("00");
    if digits.len() < 10 {
        return None;
    }
    let nsn = &digits[digits.len() - 10..];
    let (exchange, line) = (&nsn[3..6], &nsn[6..10]);
    if exchange != "555" {
        return None;
    }
    match line.parse::<u32>() {
        Ok(subscriber) if (100..=199).contains(&subscriber) => Some("nanp-fiction-555-01xx"),
        _ => None,
    }
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

    /// The fleet's own BRAND domain can never belong to a customer, so a request from it is
    /// machinery — but it stays OUT of the mail-suppression list, because the owner's proof inbox
    /// lives there and the welcome/credential mail must still be delivered. Measured on this app's
    /// deployed binary 2026-11-18: an audit signup on this domain paged the operator while
    /// `probe_sweep` had always classified the same address as residue.
    #[test]
    fn the_fleet_brand_domain_never_pages_the_operator_but_still_receives_mail() {
        assert_eq!(
            synthetic_requester("realcustomer-1795045785@swiftsoftware.com", ""),
            Some("swiftsoftware.com")
        );
        // Case, whitespace and a trailing root dot are normalised away, like every other arm.
        assert_eq!(
            synthetic_requester("Happy.Customer@SwiftSoftware.COM.", ""),
            Some("swiftsoftware.com")
        );
        // A subdomain is still the brand class.
        assert_eq!(
            synthetic_requester("inbox@mail.swiftsoftware.com", ""),
            Some("swiftsoftware.com")
        );
        // THE MAIL LAYER MUST NOT LEARN IT: the owner's proof inbox is on this domain.
        assert_eq!(
            harness_domain("realcustomer-1795045785@swiftsoftware.com"),
            None
        );
        assert_eq!(harness_domain("david@swiftsoftware.com"), None);
        // A lookalike domain is a real customer, never a match.
        assert_eq!(synthetic_requester("x@notswiftsoftware.com", ""), None);
        // Controls: the identities this predicate must keep admitting.
        assert_eq!(
            synthetic_requester("owner@albert-tire.com", "Albert Tire"),
            None
        );
        assert_eq!(synthetic_requester("someone@gmail.com", ""), None);
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

#[cfg(test)]
mod number_tests {
    use super::undeliverable_number;

    #[test]
    fn fictitious_numbers_are_held() {
        // Whatever shape the caller hands over.
        assert_eq!(
            undeliverable_number("8135550101"),
            Some("nanp-fiction-555-01xx")
        );
        assert_eq!(
            undeliverable_number("+1 (813) 555-0101"),
            Some("nanp-fiction-555-01xx")
        );
        assert_eq!(
            undeliverable_number("001-305-555-0199"),
            Some("nanp-fiction-555-01xx")
        );
    }

    #[test]
    fn dialable_numbers_are_untouched() {
        // Exchange 555 with a real subscriber part is a legitimate number.
        assert_eq!(undeliverable_number("8135551234"), None);
        assert_eq!(undeliverable_number("+1 305 555 1234"), None);
        assert_eq!(undeliverable_number("3051234567"), None);
        // Too short to judge is not a probe verdict.
        assert_eq!(undeliverable_number("5550101"), None);
        assert_eq!(undeliverable_number(""), None);
    }
}

/// The hub's pipeline label (kanban t_538195c3): a pushed identity is a probe only when the
/// ADDRESS is machine-minted. The controls matter as much as the positives — the brand-domain arm
/// of `synthetic_requester` must never turn an operator's own mailbox into a probe row.
#[cfg(test)]
mod probe_contact_addr_tests {
    use super::probe_contact_addr;

    #[test]
    fn harness_identities_are_probes() {
        // ADASwift's signup probe, as pushed into the SwiftImpact tenant.
        assert!(probe_contact_addr(
            "realcustomer-1795045484@swiftsoftware.com"
        ));
        // A machine-minted local part on a real, registrable domain.
        assert!(probe_contact_addr("probe-x@maxxspace.com"));
        // The reserved class.
        assert!(probe_contact_addr("x@example.invalid"));
        // Shape/normalisation: case, whitespace and the trailing root dot do not decide it.
        assert!(probe_contact_addr(
            "  RealCustomer-1795045484@SwiftSoftware.COM.  "
        ));
        // A fingerprint on a harness mail domain, whatever the local part.
        assert!(probe_contact_addr("someone@mail.swiftsoftware.dev"));
        // The rest of the alert class.
        assert!(probe_contact_addr("proof@test.com"));
        // The stems the fleet's own probes used on ROUTABLE customer domains (2026-11-26).
        assert!(probe_contact_addr("BCPROBE3A@CORESWIFTCRM.COM"));
        assert!(probe_contact_addr("captureaudit0ae2c1@coreswiftcrm.com"));
        assert!(probe_contact_addr("ceo-rule-probe@zaarhub.com"));
        assert!(probe_contact_addr(
            "probe+temperature-trigger@swiftimpactsolutions.com"
        ));
    }

    #[test]
    fn test_row_stems_do_not_gate_creation() {
        // The widened attribution list must NOT leak into the CREATION gate: the booking-capture
        // harness addresses its buyer this way precisely so the capture path can mint the contact
        // it is testing. If this ever returns Some, that harness stops testing capture and starts
        // testing the gate.
        assert_eq!(
            super::synthetic_requester("captureaudit0ae2c1@coreswiftcrm.com", "Dana Buyer"),
            None
        );
    }

    #[test]
    fn real_customers_are_not_probes() {
        assert!(!probe_contact_addr("owner@albert-tire.com"));
        assert!(!probe_contact_addr("jane@acme.io"));
        // The owner's own mailbox on the brand domain — a customer row, never a probe.
        assert!(!probe_contact_addr("david@swiftsoftware.com"));
        // A person with digits in the mailbox is still a person.
        assert!(!probe_contact_addr("john-1985@swiftsoftware.com"));
        // Label boundary: a domain that merely contains the brand name is a customer.
        assert!(!probe_contact_addr("someone@notswiftsoftware.com"));
        assert!(!probe_contact_addr("owner@notalbert-tire.com"));
        // The widened stems must not turn an ordinary mailbox on a customer's own domain into a
        // probe: only the stem AT THE START of the local part counts.
        assert!(!probe_contact_addr("dana.buyer@coreswiftcrm.com"));
        assert!(!probe_contact_addr("audit@zaarhub.com"));
    }

    #[test]
    fn unparseable_or_empty_is_never_a_probe() {
        assert!(!probe_contact_addr(""));
        assert!(!probe_contact_addr("   "));
        assert!(!probe_contact_addr("not-an-address"));
        assert!(!probe_contact_addr("@swiftsoftware.com"));
        assert!(!probe_contact_addr("probe-x@"));
    }
}
