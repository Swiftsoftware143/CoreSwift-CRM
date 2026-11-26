//! Free-text hygiene for the CRM's write doors.
//!
//! A company name, a pipeline name, a list name or a deal name pasted out of a spreadsheet, an
//! email or another CRM can carry an INVISIBLE control character. Postgres cannot store a NUL byte
//! in a text column, so the bound statement failed and the caller was answered
//! `500 {"code":500,"error":true,"message":"Database error"}` with nothing saved and no field named
//! — measured live on this app's running binary 2026-11-26 (probe identity, `x-swift-harness`):
//!
//! ```text
//! POST /api/companies  {"name":"Co\u0000one"}   -> 500 Database error
//! POST /api/pipelines  {"name":"P\u0000ipe"}    -> 500 Database error
//! POST /api/deals      {"title":"D\u0000eal"}   -> 500 Database error
//! POST /api/lists      {"name":"L\u0000ist"}    -> 500 Database error
//! POST /api/tags       {"name":"t\u0000ag"}     -> 422 "name: must not contain control characters"
//! ```
//!
//! `security::tag_name` already refuses the byte on the tag door; these are the doors that did not
//! call it. Two rules, because the two kinds of text are different:
//!
//!   * a SINGLE-LINE value (a name) may carry no control character at all — it is rendered into a
//!     list row and a document title, and a newline there is a paste accident;
//!   * a free-text value (notes, a description) may keep line breaks, but never a NUL.
//!
//! A NUL is STRIPPED rather than refused: it is invisible, so refusing it gives the operator an
//! error they cannot act on, while dropping it preserves every character they can see.

use crate::errors::AppError;

/// Remove every NUL byte. Postgres rejects one in a text column, and no person can see it.
pub fn strip_nul(raw: &str) -> String {
    raw.chars().filter(|c| *c != '\u{0}').collect()
}

/// The same for an optional free-text field: `None` stays `None`, a value comes back NUL-free.
pub fn nul_free(raw: Option<String>) -> Option<String> {
    raw.map(|value| strip_nul(&value))
}

/// A single-line value: NUL-stripped, trimmed, required, no control characters, bounded.
///
/// Returns the value to persist — the caller binds THIS, never the raw input.
pub fn single_line(raw: &str, label: &str, max_chars: usize) -> Result<String, AppError> {
    let stripped = strip_nul(raw);
    let value = stripped.trim();
    if value.is_empty() {
        return Err(AppError::Validation(format!("{label}: is required")));
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::Validation(format!(
            "{label}: must not contain line breaks or control characters"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::Validation(format!(
            "{label}: is longer than {max_chars} characters"
        )));
    }
    Ok(value.to_string())
}

/// The same guard for an OPTIONAL single-line value: absent or blank means `None`.
pub fn optional_line(
    raw: Option<&str>,
    label: &str,
    max_chars: usize,
) -> Result<Option<String>, AppError> {
    let Some(raw) = raw else { return Ok(None) };
    let stripped = strip_nul(raw);
    let value = stripped.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().any(char::is_control) {
        return Err(AppError::Validation(format!(
            "{label}: must not contain line breaks or control characters"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::Validation(format!(
            "{label}: is longer than {max_chars} characters"
        )));
    }
    Ok(Some(value.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nul_is_stripped_not_refused() {
        assert_eq!(single_line("Co\u{0}one", "name", 200).unwrap(), "Coone");
    }

    #[test]
    fn a_newline_in_a_name_is_refused_and_named() {
        let err = single_line("Co\none", "name", 200).unwrap_err();
        assert!(format!("{err:?}").contains("name: must not contain"));
    }

    #[test]
    fn a_blank_name_is_required() {
        assert!(single_line("\u{0}  ", "name", 200).is_err());
    }

    #[test]
    fn a_note_keeps_its_line_breaks_but_loses_the_nul() {
        assert_eq!(
            nul_free(Some("line one\u{0}\nline two".to_string())).unwrap(),
            "line one\nline two"
        );
    }
}
