//! Deciding what is a person asking for help and what is a machine talking to a machine.
//!
//! Every rule here exists because of something that actually landed in this database: tickets whose
//! subjects are DMARC reports, bounce reports, vendor marketing and luxury-watch spam, not one of them
//! a customer. A queue is only worth reading if the noise is kept out of it, so the tests below are the
//! price of admission for a message to become a ticket.
//!
//! Two families of rule, deliberately different in strength:
//!
//! * **A sender that is a machine, by name.** A bounce DSN, a DMARC reporter, `postmaster@`, a
//!   `marketing@`/`noreply@` mailbox, a host that only exists to bounce mail. One signal is enough;
//!   no human writes from these and a robot never asks for support.
//! * **Bulk promotional mail wearing a support address.** Spammers send from `supportmount@`,
//!   `supportpress@` — a desk name with junk glued on — carrying a sale. This needs BOTH signals (an
//!   impersonated desk address AND promotional content), because either one alone is ordinary: real
//!   customers do write from `support@`, and real customers do write about a Black Friday order.
//!
//! Nothing here "drops" mail. The message is stored and stays visible in the shared inbox; only the
//! ticket is withheld, and the inbox can turn any message into a ticket with one press
//! (`POST /api/private-email/messages/:id/ticket`). That is what makes a widening filter safe: a wrong
//! call costs one click, not a lost request.
//!
//! The header-based rules (auto-reply, bulk precedence, delivery-status reports, mailing lists) are
//! already applied by the inbound provider parser and arrive as `InboundEmail::is_auto_generated`.

use std::collections::HashSet;
use std::sync::OnceLock;

/// The vendored community disposable / temporary-mailbox blocklist (MIT), fetched once and
/// committed beside the inbox module. `include_str!` embeds it, so the process never opens a
/// socket or a file to answer this question.
const DISPOSABLE_DOMAINS_TXT: &str = include_str!("../private_email/disposable_domains.txt");

/// The blocklist as a set, built on first use and kept for the life of the process.
fn disposable_domains() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        DISPOSABLE_DOMAINS_TXT
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.trim_end_matches('.'))
            .collect()
    })
}

/// Is the sender's host a disposable / temporary mailbox provider?
///
/// Matches the exact domain AND any subdomain (`inbox.mailinator.com` -> `mailinator.com`),
/// walking label boundaries so `mailinator.com.evil.net` is not a match.
pub fn is_disposable_domain(from: &str) -> bool {
    let host = host_of(from);
    if host.is_empty() {
        return false;
    }
    let set = disposable_domains();
    let mut candidate: &str = &host;
    loop {
        if set.contains(candidate) {
            return true;
        }
        match candidate.split_once('.') {
            Some((_, rest)) => candidate = rest,
            None => return false,
        }
    }
}

/// The local part of an address (`"noreply"` for `"noreply@dmarc.yahoo.com"`).
fn local_part(from: &str) -> &str {
    from.split('@').next().unwrap_or("").trim()
}

/// The host part of an address, lowercased (`"dmarc.yahoo.com"`).
fn host_of(from: &str) -> String {
    from.split_once('@')
        .map(|(_, host)| {
            host.trim()
                .trim_end_matches('>')
                .trim_end_matches('.')
                .to_ascii_lowercase()
        })
        .unwrap_or_default()
}

/// A subject that announces a machine wrote it, with no header to prove it.
///
/// DMARC aggregate reports arrive from an ordinary-looking address with an ordinary `To`, no
/// `Auto-Submitted`, and a subject beginning `Report Domain:`. TLS reports (RFC 8460) and ARF
/// (RFC 5965) complaints are the same shape. All of them are filed by mail servers, and answering one
/// is at best noise and at worst a mail loop.
pub fn subject_is_machine_report(subject: &str) -> bool {
    let s = subject.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "report domain:",
        "report-id:",
        "aggregate report",
        "dmarc aggregate",
        "tls-rpt",
        "tls report",
        "arf report",
        "feedback report",
        "undelivered mail returned to sender",
        "mail delivery failed",
        "delivery status notification",
        "returned mail:",
        "failure notice",
        "new message on microsoft teams",
        "new voicemail in mailbox",
        "automatic reply:",
        "auto-reply:",
        "out of office",
        "out of the office",
        "abwesenheitsnotiz",
        "read receipt",
        "dsn: ",
    ];
    MARKERS.iter().any(|m| s.contains(m))
}

/// Does this sender address belong to a robot rather than a person?
///
/// The list is conservative on purpose: a person may genuinely write from `info@` or `hello@`, so
/// those are absent. Everything here is a mailbox that exists to send, not to be answered.
pub fn sender_is_a_robot(from: &str) -> bool {
    let local = local_part(from);
    if local.is_empty() {
        return false;
    }
    let lower = local.to_ascii_lowercase();
    // `support+tag@` is the same mailbox; strip the tag so it is judged as itself.
    let bare = lower.split('+').next().unwrap_or(&lower);
    let squashed: String = bare.chars().filter(|c| c.is_ascii_alphanumeric()).collect();

    const PREFIXES: &[&str] = &[
        "mailer-daemon",
        "postmaster",
        "hostmaster",
        "noreply",
        "no-reply",
        "do-not-reply",
        "donotreply",
        "bounce",
        "bounces",
        "notifications",
        "dmarc",
        "dmarc-support",
        "abuse",
        "root",
    ];
    if PREFIXES.iter().any(|p| bare.starts_with(p)) {
        return true;
    }

    // A token anywhere in the address, not only at the front: `marketing@`, `news-marketing@`,
    // `hello-marketing@` are all bulk-sender mailboxes.
    const ROBOT_TOKENS: &[&str] = &[
        "noreply",
        "donotreply",
        "mailerdaemon",
        "postmaster",
        "bounces",
        "dmarc",
        "abuse",
        "marketing",
        "newsletter",
        "promotions",
        "promo",
        "notifications",
        "automated",
        "automation",
        "digest",
    ];
    if ROBOT_TOKENS.iter().any(|t| squashed.contains(t)) {
        return true;
    }

    domain_is_a_bounce_host(from)
}

/// A host that exists to bounce or report mail.
///
/// Bulk senders hand out a fresh random local part per message
/// (`38rv2agokb2aqa-…@scoutcamp.bounces.google.com`), so the address carries no signal at all — the
/// host is the only thing left to read.
pub fn domain_is_a_bounce_host(from: &str) -> bool {
    let host = host_of(from);
    if host.is_empty() {
        return false;
    }
    if host.ends_with(".sender-sib.com") || host.ends_with(".bounces.google.com") {
        return true;
    }
    host.split('.')
        .any(|label| label == "bounces" || label == "bounce" || label == "mailer-daemon")
}

/// A desk name with something glued onto it: `supportmount@`, `supportpress@`, `billingteam@`.
///
/// `support@`, `support+pro@` and `info@` are left alone — those are real mailboxes. Only an address
/// that STARTS with a desk word and then keeps going looks like spam imitating one.
fn impersonates_a_desk(from: &str) -> bool {
    const DESKS: &[&str] = &[
        "support",
        "helpdesk",
        "help-desk",
        "customerservice",
        "customer-service",
        "info",
        "sales",
        "billing",
        "accounts",
        "admin",
    ];
    let lower = local_part(from).to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    DESKS.iter().any(|desk| {
        lower
            .strip_prefix(desk)
            .map(|rest| !rest.is_empty() && !rest.starts_with('+'))
            .unwrap_or(false)
    })
}

/// A subject that is selling something rather than asking something.
fn subject_reads_like_an_advert(subject: &str) -> bool {
    let s = subject.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "limited time",
        "black friday",
        "cyber monday",
        "flash sale",
        "clearance",
        "% off",
        "exclusive offer",
        "special offer",
        "last chance",
        "new arrivals",
        "save up to",
        "deal of the day",
        "shop now",
        "newsletter",
        "webinar",
        "case study",
        "product update",
        "your account can send",
    ];
    MARKERS.iter().any(|m| s.contains(m))
}

/// A body that is a mailshot: an unsubscribe line, a "view in browser", a shop link.
fn body_reads_like_a_mailshot(body: &str) -> bool {
    let s = body.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "unsubscribe",
        "view this email in your browser",
        "view in browser",
        "shop now",
        "explore our collection",
        "limited time offer",
        "you are receiving this because",
        "all rights reserved",
    ];
    MARKERS.iter().any(|m| s.contains(m))
}

/// A body that only a mass mailing carries: an opt-out or a reader-app link, an affiliate
/// disclosure, or the standing phrasing of a customer newsletter.
///
/// Deliberately narrower than [`body_reads_like_a_mailshot`]: none of these is something a person
/// writes when they are asking for help.
fn body_carries_a_bulk_marker(body: &str) -> bool {
    let s = body.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "unsubscribe",
        "view this email in your browser",
        "view in browser",
        "you are receiving this",
        "affiliate links",
        "thank you for being part of",
        "referral fee",
    ];
    MARKERS.iter().any(|m| s.contains(m))
}

/// Does the subject read like a person asking us something (a question, a reply in a thread, or a
/// request), rather than like a newsletter headline?
fn subject_is_a_person_asking(subject: &str) -> bool {
    let s = subject.trim().to_ascii_lowercase();
    if s.contains('?') {
        return true;
    }
    if s.starts_with("re:")
        || s.starts_with("re :")
        || s.starts_with("fwd:")
        || s.starts_with("fw:")
    {
        return true;
    }
    const REQUESTS: &[&str] = &[
        "help",
        "please",
        "question",
        "issue",
        "problem",
        "not working",
        "broken",
        "refund",
        "order",
        "invoice",
        "charged",
        "cancel",
        "error",
        "can't",
        "cannot",
        "unable",
    ];
    REQUESTS.iter().any(|m| s.contains(m))
}

/// A bulk mailshot is not a prospect: the body carries a mass-mailing marker and the subject is a
/// headline, not a request from a person (kanban t_a48c8797).
fn is_bulk_mailshot(subject: &str, body_plain: &str) -> bool {
    body_carries_a_bulk_marker(body_plain) && !subject_is_a_person_asking(subject)
}

/// Would filing this message put noise in the queue?
///
/// `body_plain` is only consulted for the advert rule, together with an impersonated desk address.
/// `own_addresses` are the mailboxes this helpdesk itself sends from: mail arriving from one of them
/// is a loop — our own notification bouncing back, or an auto-responder answering us.
pub fn is_noise(
    subject: &str,
    from: &str,
    body_plain: &str,
    is_auto_generated: bool,
    own_addresses: &[String],
) -> bool {
    if is_auto_generated {
        return true;
    }
    if subject_is_machine_report(subject) {
        return true;
    }
    let from_lower = from.trim().to_ascii_lowercase();
    if from_lower.is_empty() {
        return true;
    }
    if own_addresses
        .iter()
        .any(|a| a.trim().to_ascii_lowercase() == from_lower)
    {
        return true;
    }
    if sender_is_a_robot(&from_lower) {
        return true;
    }
    // A disposable / temporary mailbox cannot be a customer we can ever reach again.
    if is_disposable_domain(&from_lower) {
        return true;
    }
    // A fleet probe identity is machinery, decided from the address alone. `probe_contact_addr` and
    // not `synthetic_requester`: the latter also matches every address on the brand domain
    // (`david@swiftsoftware.com` is a real person here), while this one requires the machine SHAPE
    // on the brand domain and matches a machine-minted local part on any domain.
    if crate::security::probe_addr::probe_contact_addr(&from_lower) {
        return true;
    }
    // Bulk promotional mail wearing a desk address. Both halves are required: see the module note.
    if impersonates_a_desk(&from_lower)
        && (subject_reads_like_an_advert(subject) || body_reads_like_a_mailshot(body_plain))
    {
        return true;
    }
    // A mailshot that did not impersonate a desk: mass-mailing body + a headline subject.
    is_bulk_mailshot(subject, body_plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(subject: &str, from: &str, body: &str) -> bool {
        is_noise(
            subject,
            from,
            body,
            false,
            &["support@brand.com".to_string()],
        )
    }

    #[test]
    fn dmarc_reports_are_noise_without_any_header_saying_so() {
        assert!(subject_is_machine_report(
            "Report Domain: adaswift.com Submitter: yahoo.com Report-ID: <1794634788.299272>"
        ));
        assert!(subject_is_machine_report(
            "Undelivered Mail Returned to Sender"
        ));
        assert!(subject_is_machine_report("Out of Office: back Monday"));
        assert!(subject_is_machine_report("TLS-RPT report for acme.com"));
    }

    #[test]
    fn a_person_asking_a_question_is_not_noise() {
        assert!(!subject_is_machine_report("My invoice is wrong"));
        assert!(!sender_is_a_robot("david@gmail.com"));
        assert!(!sender_is_a_robot("info@acme.com"));
        // A real desk address, with and without a plus tag.
        assert!(!sender_is_a_robot("support@brand.com"));
        assert!(!sender_is_a_robot("support+pro@brand.com"));
        assert!(!noise(
            "Where is my order?",
            "dana@acme.com",
            "It never arrived."
        ));
    }

    #[test]
    fn robots_and_our_own_mailbox_are_noise() {
        assert!(sender_is_a_robot("mailer-daemon@mail.example.com"));
        assert!(sender_is_a_robot("noreply-dmarc-support@yahoo.com"));
        assert!(is_noise(
            "hello",
            "support@brand.com",
            "",
            false,
            &["support@brand.com".to_string()]
        ));
        assert!(!is_noise(
            "hello",
            "customer@acme.com",
            "",
            false,
            &["support@brand.com".to_string()]
        ));
    }

    #[test]
    fn the_measured_queue_noise_is_all_noise() {
        // Every row the live database held on 2026-11-19 has to answer yes.
        assert!(noise(
            "[Limited Time Offer] Redefine Your Style - Luxury Timepieces",
            "supportmount@dm-star.com",
            "Premium collection. Shop now."
        ));
        assert!(noise(
            "[Black Friday 2026 Limited Time Offer] Premium Genuine Leather Travel Bags",
            "supportpress@b1.map-bookshop.ru",
            "TIMELESS LEATHER"
        ));
        assert!(noise(
            "[Black Friday] Elegance Reimagined: Luxury Watches Starting at Just $250",
            "supportherbal@wwwpopshoponline.ru",
            "Luxury Timepiece Collection"
        ));
        assert!(noise(
            "Report Domain: funnelswift.net Submitter: yahoo.com Report-ID: <1>",
            "noreply@dmarc.yahoo.com",
            ""
        ));
        assert!(noise(
            "Important: Accessibility issues identified on your website",
            "bounces-180158782-3752652062@ha.d.sender-sib.com",
            ""
        ));
        assert!(noise(
            "New owner for zaarhub.com",
            "38rv2agokb2aqa-lmpcnjwemmejc.amk@scoutcamp.bounces.google.com",
            ""
        ));
        assert!(noise(
            "CoreSwift mail check 1794380758 for support@",
            "postmaster@zaarhub.com",
            ""
        ));
        assert!(noise(
            "Your Telnyx account can send email, too",
            "marketing@discover.telnyx.com",
            "Add transactional and marketing email alongside your SMS."
        ));
    }

    #[test]
    fn a_customer_writing_about_a_sale_is_still_a_customer() {
        // The advert rule needs an impersonated desk address as well; a person at a real address is
        // never filtered, whatever they write about.
        assert!(!noise(
            "My Black Friday order arrived broken",
            "dana@acme.com",
            "It was a limited time offer and I want a refund."
        ));
        // A real `support@` mailbox at a customer's own company, and a desk-ish address with no
        // promotional content.
        assert!(!noise(
            "Question about my plan",
            "support-team@smallbiz.com",
            "Which tier am I on?"
        ));
        assert!(!noise(
            "Please cancel",
            "support@acme.com",
            "Unsubscribe me from the digest, I am leaving."
        ));
    }

    #[test]
    fn disposable_mailboxes_are_recognised_by_domain_and_subdomain() {
        assert!(is_disposable_domain("someone@mailinator.com"));
        assert!(is_disposable_domain("Someone@Inbox.MAILINATOR.COM."));
        assert!(is_disposable_domain("x@yopmail.com"));
        assert!(!is_disposable_domain("dana@acme.com"));
        assert!(!is_disposable_domain("dana@mailinator.com.evil.net"));
        assert!(!is_disposable_domain("gmail.com"));
    }

    #[test]
    fn the_measured_inbound_senders_never_become_contacts() {
        // The exact 13 senders measured live in tenant SwiftImpact on 2026-11-25, each with the
        // subject the inbox stored. Every one is machine mail or a bulk mailshot, never a prospect.
        let measured: &[(&str, &str, &str)] = &[
            (
                "hostmastercsawa@pop-shop-online.ru",
                "[Black Friday] Defined by Luxury: High-End Timepieces Starting at Just $250",
                "To unsubscribe, click here.",
            ),
            (
                "supportpress@b1.map-bookshop.ru",
                "[Black Friday 2026 Limited Time Offer] Premium Genuine Leather Travel Bags - Classic Style",
                "TIMELESS LEATHER. Shop now.",
            ),
            (
                "supportfah@kloiuhs.com",
                "[Black Friday 2026] Elevate Your Style: Iconic Luxury Watches Starting at $250",
                "To unsubscribe, click here.",
            ),
            (
                "hostmastervivid@mvovwg.com",
                "[Black Friday] Rolex Watches Starting at Just $250",
                "To unsubscribe, click here.",
            ),
            (
                "media@hyeperca.click",
                "Enjoy Dating Beautiful Russian Singles",
                "To unsubscribe, please click here. Click here to unsubscribe.",
            ),
            (
                "trending_items@hyeperca.click",
                "The TOP 10 products are in high demand right now at a great price.",
                "This email contains affiliate links. Shop Now. Unsubscribe.",
            ),
            (
                "3i7x-agokb-yak-vwzmxtgowwotm.kwuacxxwzbaeqnbquxikbawtcbqwva.kwu@scoutcamp.bounces.google.com",
                "New reasons prevent pages from being indexed on site zaarhub.com",
                "",
            ),
            (
                "38rv2agokb2aqa-lmpcnjwemmejc.amkqsnnmprqugdrgkn8arqmjsrgmlq.amk@scoutcamp.bounces.google.com",
                "New owner for zaarhub.com",
                "",
            ),
            (
                "3b1b6agokb2que-pqtgrnaiqqing.eqouwrrqtvuykhvkorcevuqnwvkqpu.eqo@scoutcamp.bounces.google.com",
                "New reasons prevent pages from being indexed on site zaarhub.com",
                "",
            ),
            (
                "officeteams-michelle.perreault@email.cz",
                "New Message on Microsoft Teams Meeting - michelle.perreault",
                "",
            ),
            (
                "vm-michelle.perreault@servermail.com",
                "New voicemail in mailbox 1 on 17.11.2026 18:14:08",
                "",
            ),
            (
                "chris@proactivedata.com",
                "AI, Your Business & the Next 18 Months",
                "Know A Business We Can Help? We pay referral fees of up to $5,000. Thank you for being part of Proactive Data.",
            ),
            (
                "support@pilates-studio.com.au",
                "You deserve more than unpredictable growth",
                "Grow Your Business. You are receiving this email because you were contacted about a business opportunity. Unsubscribe",
            ),
        ];
        for (from, subject, body) in measured {
            assert!(
                noise(subject, from, body),
                "{from} must be refused as a contact"
            );
        }

        // Controls: a person asking a question and a real desk mailbox must still mint a contact.
        assert!(!noise(
            "Where is my order?",
            "dana@acme.com",
            "It never arrived."
        ));
        // `support@brand.com` is a CUSTOMER's desk mailbox here; our own outbound desk is a
        // different address, so the loop rule does not swallow it.
        assert!(!is_noise(
            "Help with my account",
            "support@brand.com",
            "Which tier am I on? I cannot log in.",
            false,
            &["support@ourhelpdesk.com".to_string()]
        ));
    }
}
