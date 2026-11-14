//! Deciding what is a person asking for help and what is a machine talking to a machine.
//!
//! Every rule here exists because of something that actually landed in this database: seventeen tickets
//! whose subjects are DMARC reports and marketing blasts, none of them with a single message on them.
//! A queue is only worth reading if the noise is kept out of it, so the tests below are the price of
//! admission for a message to become a ticket.
//!
//! The header-based rules (auto-reply, bulk precedence, delivery-status reports) are already applied by
//! the inbound provider parser and arrive as `InboundEmail::is_auto_generated`. This module covers what
//! a header cannot say — reports that present as ordinary mail from an ordinary sender.

/// A subject that announces a machine wrote it, with no header to prove it.
///
/// DMARC aggregate reports arrive from `noreply-dmarc-support@…` with an ordinary `To`, no
/// `Auto-Submitted`, and a subject beginning `Report Domain:`. Delivery reports and read receipts are
/// the same shape. All of them are filed by mail servers, and answering one is at best noise and at
/// worst a mail loop.
pub fn subject_is_machine_report(subject: &str) -> bool {
    let s = subject.trim().to_ascii_lowercase();
    if s.is_empty() {
        return false;
    }
    const MARKERS: &[&str] = &[
        "report domain:",
        "report-id:",
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

/// A local part that belongs to a robot rather than a person.
///
/// The list is conservative on purpose: a person may genuinely write from `info@` or `hello@`, so
/// those are absent. Everything here is a mailbox that exists to send, not to be answered.
pub fn sender_is_a_robot(from: &str) -> bool {
    let local = from
        .split('@')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if local.is_empty() {
        return false;
    }
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
    ];
    PREFIXES.iter().any(|p| local.starts_with(p))
}

/// Would filing this message put noise in the queue?
///
/// `own_addresses` are the mailboxes this helpdesk itself sends from. Mail arriving from one of them is
/// a loop: our own notification bouncing back, or an auto-responder answering us.
pub fn is_noise(
    subject: &str,
    from: &str,
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
    sender_is_a_robot(&from_lower)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dmarc_reports_are_noise_without_any_header_saying_so() {
        assert!(subject_is_machine_report(
            "Report Domain: adaswift.com Submitter: yahoo.com Report-ID: <1794634788.299272>"
        ));
        assert!(subject_is_machine_report(
            "Undelivered Mail Returned to Sender"
        ));
        assert!(subject_is_machine_report("Out of Office: back Monday"));
    }

    #[test]
    fn a_person_asking_a_question_is_not_noise() {
        assert!(!subject_is_machine_report("My invoice is wrong"));
        assert!(!sender_is_a_robot("david@gmail.com"));
        assert!(!sender_is_a_robot("info@acme.com"));
    }

    #[test]
    fn robots_and_our_own_mailbox_are_noise() {
        assert!(sender_is_a_robot("mailer-daemon@mail.example.com"));
        assert!(sender_is_a_robot("noreply-dmarc-support@yahoo.com"));
        assert!(is_noise(
            "hello",
            "support@brand.com",
            false,
            &["support@brand.com".to_string()]
        ));
        assert!(!is_noise(
            "hello",
            "customer@example.com",
            false,
            &["support@brand.com".to_string()]
        ));
    }
}
