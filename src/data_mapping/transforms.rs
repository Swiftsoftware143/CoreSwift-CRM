//! Template resolution + deterministic transforms for the data mapper.
//!
//! A mapping template is plain text plus `{{dot.path}}` tokens — the "data pills" the UI inserts.
//! Resolution is purely deterministic: no LLM, no guessing, no network. The same payload and the
//! same template always produce the same value, which is what makes the UI preview truthful.
//!
//! Transform vocabulary (spec §4): `trim`, `lower`, `upper`, `title`, `phone_e164`, `date_iso8601`,
//! `bool`, `number`, `split_name`, `join`, `truncate(n)`, `regex_extract(pattern)`, `static`.
//!
//! `split_name` takes the first word of the value (`John Smith` -> `John`); `split_name:last` takes
//! the last (`John Smith` -> `Smith`). The spec's bare `split_name` cannot express both halves of a
//! full name, and mapping `full_name` onto `first_name`/`last_name` is the common case.

use serde_json::Value;

/// The outcome of resolving one template against one source row.
#[derive(Debug, Default, Clone)]
pub struct Resolved {
    /// `None` when nothing usable came out (missing token, empty after transform).
    pub value: Option<String>,
    /// Tokens that were referenced but resolved to nothing — reported, never silently invented.
    pub missing: Vec<String>,
}

/// Read a dot path (`meta.utm_source`, `tags.0`) out of a JSON value.
fn lookup<'a>(ctx: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = ctx;
    for part in path.split('.') {
        if part.is_empty() {
            return None;
        }
        cur = match cur {
            Value::Object(map) => map.get(part)?,
            Value::Array(items) => {
                let idx: usize = part.parse().ok()?;
                items.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Turn a JSON value into the text a template writes. Arrays join with ", ", objects are compact
/// JSON, numbers keep their own text, booleans become `true`/`false`.
pub fn value_to_text(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        }
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(value_to_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(", "))
            }
        }
        Value::Object(_) => Some(v.to_string()),
    }
}

/// Resolve `template` against `ctx`, reporting tokens that produced nothing.
///
/// A template that is exactly one token yields that token's value verbatim (so an array stays an
/// array's text, not a padded string). Mixed templates concatenate: text is kept, a token that
/// resolves to nothing contributes the empty string and is recorded in `missing`.
pub fn resolve(template: &str, ctx: &Value) -> Resolved {
    let mut out = String::new();
    let mut missing = Vec::new();
    let mut rest = template;

    // Single-token template: return the raw value so `{{tags}}` can keep its array text.
    let trimmed = template.trim();
    if let Some(inner) = trimmed
        .strip_prefix("{{")
        .and_then(|s| s.strip_suffix("}}"))
    {
        let path = inner.trim();
        if !path.contains("{{") && !path.contains("}}") {
            match lookup(ctx, path).and_then(value_to_text) {
                Some(v) => {
                    return Resolved {
                        value: Some(v),
                        missing,
                    }
                }
                None => {
                    missing.push(path.to_string());
                    return Resolved {
                        value: None,
                        missing,
                    };
                }
            }
        }
    }

    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let path = after[..end].trim();
                if path.is_empty() {
                    // Not a token: keep the braces as literal text.
                    out.push_str("{{}}");
                } else {
                    match lookup(ctx, path).and_then(value_to_text) {
                        Some(v) => out.push_str(&v),
                        None => missing.push(path.to_string()),
                    }
                }
                rest = &after[end + 2..];
            }
            None => {
                // Unterminated token: keep the remainder literally rather than eating it.
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);

    let value = if out.trim().is_empty() {
        None
    } else {
        Some(out)
    };
    Resolved { value, missing }
}

fn title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut start_of_word = true;
    for ch in s.chars() {
        if ch.is_whitespace() {
            start_of_word = true;
            out.push(ch);
        } else if start_of_word {
            out.extend(ch.to_uppercase());
            start_of_word = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    out
}

fn phone_e164(s: &str) -> String {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.len() {
        10 => format!("+1{digits}"),
        11 if digits.starts_with('1') => format!("+{digits}"),
        0 => s.trim().to_string(),
        _ if s.trim_start().starts_with('+') => s.trim().to_string(),
        _ => format!("+{digits}"),
    }
}

fn date_iso8601(s: &str) -> String {
    let t = s.trim();
    // Already ISO (date or datetime): normalise the date part only.
    if t.len() >= 10 && t.as_bytes().get(4) == Some(&b'-') && t.as_bytes().get(7) == Some(&b'-') {
        return t[..10].to_string();
    }
    // MM/DD/YYYY (US) and DD-MM-YYYY are the two shapes the sibling apps emit.
    let parts: Vec<&str> = t
        .split(|c| c == '/' || c == '-' || c == '.')
        .map(str::trim)
        .collect();
    if parts.len() == 3 {
        if let (Ok(a), Ok(b), Ok(c)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            let (y, m, d) = if a > 31 {
                (a, b, c)
            } else if c > 31 {
                // Slash form is US month-first; dash form is day-first only when unambiguous.
                if t.contains('/') {
                    (c, a, b)
                } else if a > 12 {
                    (c, b, a)
                } else {
                    (c, a, b)
                }
            } else {
                return t.to_string();
            };
            if (1..=12).contains(&m) && (1..=31).contains(&d) {
                return format!("{y:04}-{m:02}-{d:02}");
            }
        }
    }
    t.to_string()
}

fn as_bool_text(s: &str) -> String {
    let t = s.trim().to_ascii_lowercase();
    let yes = matches!(
        t.as_str(),
        "1" | "true" | "yes" | "y" | "on" | "si" | "sí" | "x"
    );
    yes.to_string()
}

/// Apply one transform to one value. An unknown transform is reported by the caller (the mapping is
/// refused at save time), so here it is a passthrough rather than an error.
pub fn apply_transform(transform: Option<&str>, value: &str) -> String {
    let Some(t) = transform.map(str::trim).filter(|t| !t.is_empty()) else {
        return value.to_string();
    };
    let lower = t.to_ascii_lowercase();
    if lower == "trim" {
        return value.trim().to_string();
    }
    if lower == "lower" || lower == "lower_trim" {
        return value.trim().to_lowercase();
    }
    if lower == "upper" {
        return value.trim().to_uppercase();
    }
    if lower == "title" {
        return title_case(value.trim());
    }
    if lower == "phone_e164" {
        return phone_e164(value);
    }
    if lower == "date_iso8601" {
        return date_iso8601(value);
    }
    if lower == "bool" {
        return as_bool_text(value);
    }
    if lower == "number" {
        let cleaned: String = value
            .chars()
            .filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
            .collect();
        return cleaned;
    }
    if lower == "split_name" {
        return value
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
    }
    if lower == "split_name:last" {
        return value
            .split_whitespace()
            .last()
            .unwrap_or_default()
            .to_string();
    }
    if lower == "join" {
        return value.to_string();
    }
    if lower == "static" {
        return value.to_string();
    }
    if let Some(arg) = lower
        .strip_prefix("truncate(")
        .and_then(|s| s.strip_suffix(')'))
    {
        if let Ok(n) = arg.trim().parse::<usize>() {
            return value.chars().take(n).collect();
        }
        return value.to_string();
    }
    value.to_string()
}

/// The transform names the mapper offers, in the order the UI should show them.
pub const TRANSFORMS: &[&str] = &[
    "trim",
    "lower",
    "upper",
    "title",
    "phone_e164",
    "date_iso8601",
    "bool",
    "number",
    "split_name",
    "split_name:last",
    "join",
    "truncate(120)",
    "regex_extract(pattern)",
    "static",
];

/// Is this transform name one the runner understands? `truncate(n)` and `regex_extract(p)` are
/// parameterised, so they are matched by shape.
pub fn is_known_transform(name: &str) -> bool {
    let n = name.trim().to_ascii_lowercase();
    if n.is_empty() {
        return true;
    }
    if n.starts_with("truncate(") && n.ends_with(')') {
        return n[9..n.len() - 1].trim().parse::<usize>().is_ok();
    }
    if n.starts_with("regex_extract(") && n.ends_with(')') {
        return n[14..n.len() - 1].trim().len() >= 2;
    }
    TRANSFORMS.contains(&n.as_str())
}

/// `regex_extract(pattern)` — returns the first capture group, or the whole match.
pub fn regex_extract(transform: &str, value: &str) -> String {
    let t = transform.trim();
    let Some(inner) = t
        .strip_prefix("regex_extract(")
        .and_then(|s| s.strip_suffix(')'))
    else {
        return value.to_string();
    };
    let pattern = inner.trim().trim_matches('"').trim_matches('\'');
    if pattern.is_empty() {
        return value.to_string();
    }
    let Ok(re) = regex::Regex::new(pattern) else {
        return value.to_string();
    };
    match re.captures(value) {
        Some(caps) => caps
            .get(1)
            .or_else(|| caps.get(0))
            .map(|m| m.as_str().to_string())
            .unwrap_or_default(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn single_token_keeps_its_own_text() {
        let ctx = json!({"tags": ["webinar", "q4"], "email": "a@b.com"});
        assert_eq!(
            resolve("{{tags}}", &ctx).value.as_deref(),
            Some("webinar, q4")
        );
        assert_eq!(resolve("{{email}}", &ctx).value.as_deref(), Some("a@b.com"));
    }

    #[test]
    fn mixed_template_keeps_text_and_names_the_missing_token() {
        let ctx = json!({"agent": {"name": "Ada"}});
        let r = resolve("Captured by {{agent.name}} at {{event.captured_at}}", &ctx);
        // The literal text is kept verbatim, including the separator before the dropped token;
        // `transform: "trim"` is what removes it. Resolution never invents or reshapes text.
        assert_eq!(r.value.as_deref(), Some("Captured by Ada at "));
        assert_eq!(r.missing, vec!["event.captured_at".to_string()]);
    }

    #[test]
    fn nested_and_indexed_paths_resolve() {
        let ctx = json!({"meta": {"utm_source": "google"}, "items": [{"sku": "A1"}]});
        assert_eq!(
            resolve("{{meta.utm_source}}", &ctx).value.as_deref(),
            Some("google")
        );
        assert_eq!(
            resolve("{{items.0.sku}}", &ctx).value.as_deref(),
            Some("A1")
        );
    }

    #[test]
    fn transforms_are_deterministic() {
        assert_eq!(apply_transform(Some("trim"), "  x  "), "x");
        assert_eq!(apply_transform(Some("lower"), " A@B.COM "), "a@b.com");
        assert_eq!(apply_transform(Some("title"), "john smith"), "John Smith");
        assert_eq!(
            apply_transform(Some("phone_e164"), "(305) 555-1234"),
            "+13055551234"
        );
        assert_eq!(
            apply_transform(Some("date_iso8601"), "03/14/2026"),
            "2026-03-14"
        );
        assert_eq!(apply_transform(Some("bool"), "Yes"), "true");
        assert_eq!(apply_transform(Some("split_name"), "John Smith"), "John");
        assert_eq!(
            apply_transform(Some("split_name:last"), "John Smith"),
            "Smith"
        );
        assert_eq!(apply_transform(Some("truncate(3)"), "abcdef"), "abc");
        assert!(is_known_transform("truncate(50)"));
        assert!(!is_known_transform("truncate(x)"));
    }

    #[test]
    fn regex_extract_takes_the_first_group() {
        assert_eq!(
            regex_extract("regex_extract((\\d{5}))", "zip 33139"),
            "33139"
        );
    }
}
