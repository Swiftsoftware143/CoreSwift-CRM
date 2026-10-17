//! Tag display-name and colour validation, in ONE place, at every boundary that writes `tags`.
//!
//! `tags.name` is `varchar(100)` and `tags.color` is `varchar(7)` (read from the live database
//! 2026-10-17, `information_schema.columns`). Every writer in this app bound the caller's value
//! straight into the INSERT, so a value the column cannot hold reached Postgres and came back as
//! the raw sqlx refusal rendered by [`crate::errors::AppError::Database`]:
//!
//! ```text
//! POST /api/campaigns  {"funnelswift_tag":"<101 chars>"} -> 500 {"code":500,"error":true,"message":"Database error"}
//! POST /api/tags       {"name":"<101 chars>"}            -> 500 {"code":500,"error":true,"message":"Database error"}
//! POST /api/tags       {"color":"#ABCDEFG"}              -> 500 {"code":500,"error":true,"message":"Database error"}
//! ```
//!
//! Measured live on this app before the fix (kanban t_4be4ae4c). A 100-character name and a
//! 7-character colour were accepted in the same run, so the cap is the STORAGE limit, not a new
//! product rule: anything that fitted before still fits.
//!
//! Call this BEFORE the write and bind the value it returns — never the raw input, because it is
//! trimmed (a tag name is rendered into HTML and mail subjects). The handler maps the reason to its
//! own `422`; the reason names the offending field so an operator can fix it.

use crate::errors::AppError;

/// `tags.name` is `varchar(100)` in THIS database — not the 255 some fleet apps use, so the cap is
/// read from the column, never copied from a sibling.
pub const MAX_TAG_NAME_CHARS: usize = 100;

/// `tags.color` is `varchar(7)` (a `#RRGGBB` hex string is exactly 7).
pub const MAX_TAG_COLOR_CHARS: usize = 7;

/// Trim, require a non-empty value, and refuse one this database cannot hold.
///
/// Returns the value to persist. The caller binds THIS, never `raw`.
pub fn normalize(raw: &str) -> Result<String, String> {
    normalize_capped(raw, MAX_TAG_NAME_CHARS, "name")
}

/// The same guard for any other length-capped display value written to `tags`.
pub fn normalize_capped(raw: &str, max_chars: usize, label: &str) -> Result<String, String> {
    let value = raw.trim();

    if value.is_empty() {
        return Err(format!("{label}: is required"));
    }
    // Counted in CHARACTERS, which is what `varchar(n)` measures.
    if value.chars().count() > max_chars {
        return Err(format!("{label}: is longer than {max_chars} characters"));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err(format!("{label}: must not contain control characters"));
    }

    Ok(value.to_string())
}

/// The colour guard: a colour is optional, so a blank means "let the column default apply", but a
/// value `tags.color` cannot hold is the caller's mistake and must not become a 500.
pub fn normalize_color(raw: &str) -> Result<String, String> {
    let value = raw.trim();
    if value.chars().count() > MAX_TAG_COLOR_CHARS {
        return Err(format!(
            "color: is longer than {MAX_TAG_COLOR_CHARS} characters"
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err("color: must not contain control characters".to_string());
    }
    Ok(value.to_string())
}

/// [`normalize`] mapped to the app's own refusal — the shape every HTTP writer wants.
pub fn normalize_or_validation(raw: &str) -> Result<String, AppError> {
    normalize(raw).map_err(AppError::Validation)
}

/// [`normalize_color`] mapped to the app's own refusal.
pub fn color_or_validation(raw: &str) -> Result<String, AppError> {
    normalize_color(raw).map_err(AppError::Validation)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_the_lengths_that_produced_a_500() {
        // Measured live 2026-10-17 before the fix: 101 characters -> 500 "Database error".
        assert_eq!(
            normalize(&"T".repeat(101)),
            Err("name: is longer than 100 characters".to_string())
        );
        assert!(normalize(&"N".repeat(MAX_TAG_NAME_CHARS)).is_ok());
    }

    #[test]
    fn trims_and_refuses_blank() {
        assert_eq!(normalize("  Kinetic  ").unwrap(), "Kinetic");
        assert_eq!(normalize("   "), Err("name: is required".to_string()));
    }

    #[test]
    fn counts_characters_not_bytes() {
        // varchar(100) measures characters: 100 accented letters fit a 100-char column.
        assert!(normalize(&"é".repeat(100)).is_ok());
        assert!(normalize(&"é".repeat(101)).is_err());
    }

    #[test]
    fn refuses_control_characters() {
        assert!(normalize("Tag\nName").is_err());
    }

    #[test]
    fn the_colour_guard_caps_at_the_column() {
        assert!(normalize_color("#3B82F6").is_ok());
        assert_eq!(
            normalize_color("#ABCDEFG"),
            Err("color: is longer than 7 characters".to_string())
        );
        // Blank is the caller saying "no preference" — not a refusal.
        assert_eq!(normalize_color("  ").unwrap(), "");
    }
}
