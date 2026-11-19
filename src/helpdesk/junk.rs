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
    // Bulk promotional mail wearing a desk address. Both halves are required: see the module note.
    impersonates_a_desk(&from_lower)
        && (subject_reads_like_an_advert(subject) || body_reads_like_a_mailshot(body_plain))
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
            "customer@example.com",
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
}
