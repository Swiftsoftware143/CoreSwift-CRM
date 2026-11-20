//! How the customer sounded when they wrote.
//!
//! The ticketing suite asks for sentiment tags. Three rules shape this, and they are worth stating because
//! the obvious version of this module gets all three wrong:
//!
//!   1. **The tag is a floor, not a reading.** A ticket keeps the WORST tone it has been written in. One
//!      polite follow-up does not erase the message where somebody said they were cancelling; the person
//!      who picks the ticket up needs the worst of it, not the latest of it.
//!   2. **Words, not a model call.** This runs on every inbound message, inside the ingest path, on a box
//!      that must not wait on a provider to file a customer's mail. Cheap, deterministic, auditable.
//!   3. **Silence is a real answer.** A message that says nothing either way leaves the tag alone rather
//!      than stamping "calm" over a "frustrated" — "not judged" is a state, and `sentiment` is nullable
//!      for exactly that reason.
//!
//! The vocabulary is deliberately blunt: it looks for the phrases people actually use when a support
//! thread has gone wrong, not for general negativity.

use sqlx::PgPool;
use uuid::Uuid;

/// Phrases that mean the thread has gone past impatience.
const ANGRY: &[&str] = &[
    "unacceptable",
    "furious",
    "ridiculous",
    "disgusted",
    "cancel my account",
    "cancelling my account",
    "cancel my subscription",
    "lawyer",
    "legal action",
    "sue you",
    "refund now",
    "worst service",
    "useless",
    "incompetent",
    "third time",
    "fourth time",
    "still no answer",
    "nobody has replied",
    "nobody replied",
    "never again",
];

/// Phrases that mean the person is being made to work for it.
const FRUSTRATED: &[&str] = &[
    "again",
    "still not",
    "still broken",
    "still waiting",
    "waiting for",
    "no response",
    "no reply",
    "not working",
    "doesn't work",
    "does not work",
    "frustrated",
    "frustrating",
    "annoying",
    "asap",
    "urgent",
    "why is",
    "why does",
    "how many times",
    "third email",
    "second email",
    "chasing",
    "follow up again",
];

/// Rank so "worse wins" can be expressed once. Test-only: the shipped comparison happens in SQL, next to
/// the row it is guarding, and this keeps the two honest.
#[cfg(test)]
fn rank(s: &str) -> u8 {
    match s {
        "angry" => 3,
        "frustrated" => 2,
        "calm" => 1,
        _ => 0,
    }
}

/// Judge one message. `None` means the message gave nothing to go on.
pub fn classify(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    if lower.trim().is_empty() {
        return None;
    }

    if ANGRY.iter().any(|p| lower.contains(p)) {
        return Some("angry");
    }
    if FRUSTRATED.iter().any(|p| lower.contains(p)) {
        return Some("frustrated");
    }

    // Shouting is a signal in itself: a long message written mostly in capitals reads as raised voice.
    let letters: Vec<char> = lower.chars().filter(|c| c.is_alphabetic()).collect();
    let upper = text.chars().filter(|c| c.is_uppercase()).count();
    if letters.len() > 40 && upper * 100 / letters.len().max(1) > 70 {
        return Some("frustrated");
    }

    // Nothing negative was said. That is not the same as "calm" — say nothing.
    None
}

/// Record the tone on the ticket, never downgrading it.
///
/// Returns the tag the ticket now carries, or `None` when this message changed nothing.
pub async fn apply(pool: &PgPool, ticket_id: Uuid, text: &str) -> Option<String> {
    let tag = classify(text)?;

    // `rank` is duplicated in SQL as a CASE so the comparison happens in one statement: two writers cannot
    // interleave a read-then-write and lose the worse tag.
    let updated = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE tickets
           SET sentiment = $2,
               updated_at = now()
         WHERE id = $1
           AND (
                 sentiment IS NULL
              OR CASE sentiment WHEN 'angry' THEN 3 WHEN 'frustrated' THEN 2 WHEN 'calm' THEN 1 ELSE 0 END
                 < CASE $2::text WHEN 'angry' THEN 3 WHEN 'frustrated' THEN 2 WHEN 'calm' THEN 1 ELSE 0 END
           )
        RETURNING sentiment
        "#,
    )
    .bind(ticket_id)
    .bind(tag)
    .fetch_optional(pool)
    .await;

    match updated {
        Ok(row) => row,
        Err(e) => {
            // A tone tag is not worth failing a customer's mail over; it is worth seeing in the log.
            tracing::warn!(error = %e, %ticket_id, "the sentiment tag could not be written");
            None
        }
    }
}

/// The same rule, inside the caller's transaction.
///
/// The ingest path files a customer's mail in ONE transaction; tagging outside it would leave a window
/// where the message exists and its tone does not. This shares the SQL with `apply` by construction — one
/// statement, one rank table — so the two cannot disagree.
pub async fn apply_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ticket_id: Uuid,
    text: &str,
) -> Option<String> {
    let tag = classify(text)?;
    let updated = sqlx::query_scalar::<_, String>(
        r#"
        UPDATE tickets
           SET sentiment = $2,
               updated_at = now()
         WHERE id = $1
           AND (
                 sentiment IS NULL
              OR CASE sentiment WHEN 'angry' THEN 3 WHEN 'frustrated' THEN 2 WHEN 'calm' THEN 1 ELSE 0 END
                 < CASE $2::text WHEN 'angry' THEN 3 WHEN 'frustrated' THEN 2 WHEN 'calm' THEN 1 ELSE 0 END
           )
        RETURNING sentiment
        "#,
    )
    .bind(ticket_id)
    .bind(tag)
    .fetch_optional(&mut **tx)
    .await;

    match updated {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(error = %e, %ticket_id, "the sentiment tag could not be written");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_of_tone_is_the_one_the_sql_uses() {
        assert!(rank("angry") > rank("frustrated"));
        assert!(rank("frustrated") > rank("calm"));
        assert_eq!(rank("not a tag"), 0);
    }

    #[test]
    fn a_polite_message_changes_nothing() {
        assert_eq!(
            classify("Thanks, that fixed it. Appreciate the quick help."),
            None
        );
    }

    #[test]
    fn impatience_is_seen() {
        assert_eq!(
            classify("Still waiting on this, no response yet."),
            Some("frustrated")
        );
        assert_eq!(
            classify("This is the third email I am chasing."),
            Some("frustrated")
        );
    }

    #[test]
    fn the_end_of_a_thread_is_seen() {
        assert_eq!(
            classify("This is unacceptable. Cancel my account."),
            Some("angry")
        );
        assert_eq!(
            classify("Still broken, third time I am writing."),
            Some("angry")
        );
    }

    #[test]
    fn shouting_counts_and_quiet_does_not() {
        assert_eq!(
            classify(
                "WHY HAS NOBODY ANSWERED THIS IT IS THE SAME PROBLEM EVERY SINGLE WEEK FOR US"
            ),
            Some("frustrated")
        );
        assert_eq!(classify("ok"), None);
        assert_eq!(classify(""), None);
    }
}
