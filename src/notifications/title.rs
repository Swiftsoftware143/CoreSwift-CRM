//! The `notifications.title` contract.
//!
//! `notifications.title` is `VARCHAR(255) NOT NULL` with NO default (migrations/000_baseline_live_schema.sql
//! line 863; confirmed live: `information_schema.columns.column_default` is NULL), while every string a
//! writer of this table has at hand is either NULLABLE (`notification_queue.title` / `.subject`) or
//! unbounded (an action's `message`, a queue row's `body`). An INSERT that names no title therefore
//! raises 23502 and, behind the `let _ =` that guards these best-effort writes, the notification was
//! never written at all — while the queue row still went to `status = 'sent'` (t_ed3c2591).
//!
//! This module is the single place that decides the title, so no call site has to guess: the caller's
//! own most specific label first, then the next one it holds, then the first 255 characters of its own
//! body — never an invented string, never NULL, never longer than the column.

/// `notifications.title` is `VARCHAR(255)`: a longer value raises 22001, which is the same class of
/// silently-swallowed write as the missing title this module exists for.
const TITLE_MAX_CHARS: usize = 255;

/// Resolve the notification title from the strings the caller already has, in order of specificity.
///
/// `body` is the last resort and must be the text the caller is really about to store, so the title is
/// always drawn from the writer's own data. A blank label is not a label: whitespace-only candidates
/// are skipped rather than stored as an empty title.
pub fn resolve<'a>(title: Option<&'a str>, subject: Option<&'a str>, body: &'a str) -> &'a str {
    let candidate = [title, subject]
        .into_iter()
        .flatten()
        .find(|candidate| !candidate.trim().is_empty())
        .unwrap_or(body);
    truncate(candidate)
}

/// Cut to the column's own limit on a CHARACTER boundary (`varchar(255)` counts characters, and
/// slicing a `&str` on a byte index inside a multi-byte char would panic).
fn truncate(value: &str) -> &str {
    match value.char_indices().nth(TITLE_MAX_CHARS) {
        Some((cut, _)) => &value[..cut],
        None => value,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn title_wins_over_subject_and_body() {
        assert_eq!(resolve(Some("t"), Some("s"), "b"), "t");
    }

    #[test]
    fn blank_title_falls_through_to_the_subject_then_the_body() {
        assert_eq!(resolve(Some("   "), Some("s"), "b"), "s");
        assert_eq!(resolve(None, None, "b"), "b");
    }

    #[test]
    fn the_result_is_never_longer_than_the_column() {
        let long = "x".repeat(400);
        assert_eq!(resolve(None, None, &long).chars().count(), 255);
        let multibyte = "é".repeat(400);
        assert_eq!(resolve(None, None, &multibyte).chars().count(), 255);
    }
}
