use crate::AppState;
use axum::{extract::State, Json};
use serde_json::json;
use sqlx::Row;
use std::fs;

const SITE_KEY: &str = "coreswift_site";

/// Where the static marketing page and the three legal pages live. These are HOST paths: the app
/// runs in a container with ZERO mounts, so the only process that can write them is the same
/// binary executed on the host (`crm-swift apply-site-settings`, driven by
/// /opt/swift/bin/cs-site-apply.sh). See `apply_to_disk`.
pub(crate) const SITE_ROOT: &str = "/opt/swift/nginx/www/coreswift/";
pub(crate) const SITE_INDEX: &str = "/opt/swift/nginx/www/coreswift/index.html";

/// Markers around the operator-supplied script blocks. The applier runs on a SCHEDULE, so the
/// injection has to be repeatable: without markers a second run appends the same tags a second time
/// and the page grows without bound (measured: one copy per run). Everything between a marker pair is
/// removed before the block is re-inserted from the current settings, and an empty setting removes it.
/// The markers carry their own surrounding whitespace. That is deliberate: the region this module
/// INSERTS and the region it STRIPS have to be the same bytes, or a run leaves its own padding
/// behind (take 1 stripped only the bare markers and the page grew 4 bytes per apply).
const HEAD_MARK_START: &str = "\n  <!-- cs-site:head:start -->\n";
const HEAD_MARK_END: &str = "  <!-- cs-site:head:end -->\n";
const BODY_MARK_START: &str = "\n  <!-- cs-site:body:start -->\n";
const BODY_MARK_END: &str = "  <!-- cs-site:body:end -->\n";

pub async fn get_site(
    State(state): State<AppState>,
) -> Result<Json<serde_json::Value>, crate::errors::AppError> {
    let defaults = default_site_settings();
    let row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.db)
        .await?;
    let settings = match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    };
    Ok(Json(settings))
}

pub async fn update_site(
    State(state): State<AppState>,
    Json(req): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, crate::errors::AppError> {
    let existing = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(&state.db)
        .await?;
    let merged = match existing {
        Some(r) => {
            let v: serde_json::Value = r.try_get("value")?;
            merge_json(v, req)
        }
        None => req,
    };
    sqlx::query("INSERT INTO admin_settings (key, value, description, updated_at) VALUES ($1, $2::jsonb, 'CoreSwift CRM site settings', NOW()) ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()")
        .bind(SITE_KEY).bind(merged.to_string()).execute(&state.db).await?;
    // Deliberately NO file writes on this path. The static pages are HOST paths and this service
    // runs in a container with no mount for them, so `regenerate_html` could only ever answer
    // 500 — after the row above had already committed. An admin then saw a failure for a save
    // that had in fact landed. The row IS the source of truth (`get_site` reads it); the pages
    // are materialized by the host-side applier, which is their only writer.
    Ok(Json(json!({
        "message": "Site settings saved",
        "settings": merged,
        "static_pages": {
            "writer": "/opt/swift/bin/cs-site-apply.sh (crm-swift apply-site-settings)",
            "within_minutes": 5
        }
    })))
}

/// The stored settings merged over the code defaults — the same value `get_site` serves, and the
/// input to the applier.
pub(crate) async fn load_settings(
    db: &sqlx::PgPool,
) -> Result<serde_json::Value, crate::errors::AppError> {
    let defaults = default_site_settings();
    let row = sqlx::query("SELECT value FROM admin_settings WHERE key = $1")
        .bind(SITE_KEY)
        .fetch_optional(db)
        .await?;
    Ok(match row {
        Some(r) => {
            let val: serde_json::Value = r.try_get("value")?;
            merge_json(defaults, val)
        }
        None => defaults,
    })
}

/// The files the applier OWNS, as pure render functions of the settings row — so a caller can
/// compare the rendered bytes with what is served WITHOUT writing anything (`--check`).
///
/// Returns `(targets, skipped)`; each skipped entry is `(path, reason)`. `targets` is
/// `(path, rendered_bytes)`.
///
/// A legal field that is absent or blank is left ALONE (the code defaults carry `""`), so running
/// the applier can never blank out a live policy page.
pub(crate) fn plan(settings: &serde_json::Value) -> (Vec<(String, String)>, Vec<(String, String)>) {
    let mut targets: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    if !std::path::Path::new(SITE_ROOT).is_dir() {
        skipped.push((
            SITE_ROOT.to_string(),
            "directory is not present in this runtime (host-only path)".to_string(),
        ));
        return (targets, skipped);
    }

    match fs::read_to_string(SITE_INDEX) {
        Ok(before) => targets.push((SITE_INDEX.to_string(), inject_settings(&before, settings))),
        Err(e) => skipped.push((SITE_INDEX.to_string(), format!("unreadable: {}", e))),
    }

    for (slug, title, key) in [
        ("terms", "Terms of Service", "legal_tos"),
        ("privacy", "Privacy Policy", "legal_privacy"),
        ("refunds", "Refund & Cancellation Policy", "legal_refunds"),
    ] {
        let path = format!("{}{}.html", SITE_ROOT, slug);
        match settings.get(key).and_then(|v| v.as_str()) {
            // Blank or absent means "no policy text configured" -> leave the file that is there.
            Some(t) if !t.trim().is_empty() => {
                targets.push((path, legal_page(title, &escape_addresses(t))))
            }
            _ => skipped.push((
                path,
                format!(
                    "{} is blank or absent - the published page is left alone",
                    key
                ),
            )),
        }
    }

    (targets, skipped)
}

/// Materialize `settings` into the static marketing page + the three legal pages.
///
/// Idempotent: a file is rewritten only when its bytes would change. Returns `(written, skipped)`;
/// each skipped entry is `(path, reason)`. Nothing here is fatal — the targets only exist where the
/// files do, and the caller prints the outcome so no surface can claim a regeneration that did not
/// happen.
pub(crate) fn apply_to_disk(settings: &serde_json::Value) -> (Vec<String>, Vec<(String, String)>) {
    let (targets, mut skipped) = plan(settings);
    let mut written: Vec<String> = Vec::new();

    for (path, rendered) in targets {
        match fs::read_to_string(&path) {
            Ok(before) if before == rendered => {
                skipped.push((path, "unchanged".to_string()));
            }
            _ => match fs::write(&path, rendered.as_bytes()) {
                Ok(_) => written.push(path),
                Err(e) => skipped.push((path, e.to_string())),
            },
        }
    }

    (written, skipped)
}

/// One legal page, as a pure function so the applier can compare before writing.
///
/// The FRAME is the bytes the three served policy pages already carry — derived from them
/// (`audits/t_4dea90a6/frame.txt`), never retyped; the settings row carries the BODY that sits
/// between the `<h1>{title}</h1>` line and the `.back` footer (`body_<slug>.txt`). Reconciled
/// under kanban t_4dea90a6 so `render(row) == served` byte-for-byte and the applier is a no-op
/// on these three pages.
fn legal_page(title: &str, text: &str) -> String {
    // the frame carries BOTH title slots (<title> and <h1>); "{}" is the body
    format!(concat!("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"UTF-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<title>{} — CoreSwift CRM</title>\n<style>\n*{{margin:0;padding:0;box-sizing:border-box}}\nbody{{font-family:system-ui,-apple-system,sans-serif;background:#0f0f0f;color:#e5e5e5;line-height:1.7}}\n.container{{max-width:800px;margin:0 auto;padding:60px 24px}}\nh1{{font-size:2rem;color:#f59e0b;margin-bottom:8px}}\nh2{{font-size:1.2rem;color:#ffffff;margin:24px 0 12px}}\np,li{{color:#9ca3af;margin-bottom:8px}}\na{{color:#f59e0b}}\n.back{{margin-top:40px;padding-top:20px;border-top:1px solid rgba(255,255,255,.08)}}\n</style>\n<link rel=\"icon\" type=\"image/svg+xml\" href=\"/favicon-d6446083.svg\"><link rel=\"alternate icon\" href=\"/favicon-d6446083.ico\"><link rel=\"apple-touch-icon\" href=\"/favicon-d6446083.svg\"></head>\n<body>\n<div class=\"container\">\n<h1>{}</h1>\n", "{}", "\n<div class=\"back\"><a href=\"/\">← Back to CoreSwift CRM</a></div>\n</div>\n</body>\n</html>"), title, title, text)
}

/// Write every address in an HTML body as the entity `&#64;`.
///
/// The served-page convention for this fleet: a literal address in a served HTML page is rewritten
/// per-request by the edge (Cloudflare) and is flagged as a hazard by the repo/served parity gate, so
/// an address in page TEXT is authored as the entity, which renders identically and is left alone
/// (docs/fleet-marketing-www.md §6.1, kanban t_d4347fb5). The DB holds the human form
/// (`support@swiftsoftware.net`) because that is what an operator types and reads in the panel; this
/// applier is the single enforcement point that turns it into the entity on the page, so a panel edit
/// cannot re-introduce a literal address. Ported from ADASwift (kanban t_1f427190 / t_4dea90a6).
///
/// The served-page convention for this fleet: a literal address in a served HTML page is rewritten
/// per-request by the edge (Cloudflare) and is flagged as a hazard by the repo/served parity gate, so
/// an address in page TEXT is authored as the entity, which renders identically and is left alone
/// (docs/fleet-marketing-www.md §6.1, kanban t_d4347fb5 / t_cda04aec). The DB holds the human form
/// (`support@swiftimpactsolutions.com`) because that is what an operator types and reads in the
/// panel; the applier is the single enforcement point that turns it into the entity on the page.
///
/// The shape matched is the gate's own ADDRESS regex: `[A-Za-z0-9._%+-]+@([A-Za-z0-9-]+\.)+[A-Za-z]{2,}`.
fn escape_addresses(text: &str) -> String {
    let b = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'@' && looks_like_address(b, i) {
            out.push_str("&#64;");
            i += 1;
        } else {
            // Copy one full UTF-8 char, never a byte: the legal text is prose and may carry any
            // character.
            let ch = match text[i..].chars().next() {
                Some(c) => c,
                None => break,
            };
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn looks_like_address(b: &[u8], at: usize) -> bool {
    // Local part: at least one local char immediately before the '@'.
    let mut ls = at;
    while ls > 0 && is_local_char(b[ls - 1]) {
        ls -= 1;
    }
    if ls == at {
        return false;
    }

    // Domain: a run of domain chars, at least one dot, and an alphabetic TLD of 2+ chars.
    //
    // The run may end in sentence punctuation that IS a domain char — a policy sentence ends
    // `...notice to support@example.com.` and the '.' belongs to the sentence, not the domain. The
    // gate's own regex ends on an alphabetic label, so the trailing dots/hyphens are trimmed before
    // the labels are validated; otherwise the address would be skipped and a literal '@' would be
    // published (measured: terms/privacy, kanban t_1f427190).
    let mut de = at + 1;
    while de < b.len() && is_domain_char(b[de]) {
        de += 1;
    }
    while de > at + 1 && (b[de - 1] == b'.' || b[de - 1] == b'-') {
        de -= 1;
    }
    let domain = match std::str::from_utf8(&b[at + 1..de]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let parts: Vec<&str> = domain.split('.').collect();
    if parts.len() < 2 {
        return false;
    }
    let tld = parts[parts.len() - 1];
    if tld.len() < 2 || !tld.bytes().all(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    parts
        .iter()
        .all(|p| !p.is_empty() && !p.starts_with('-') && !p.ends_with('-'))
}

fn is_local_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'%' | b'+' | b'-')
}

fn is_domain_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'.' || c == b'-'
}

fn inject_settings(html: &str, s: &serde_json::Value) -> String {
    let mut r = html.to_string();
    if let Some(t) = s.get("title").and_then(|v| v.as_str()) {
        replace_title(&mut r, t);
    }
    if let Some(d) = s.get("description").and_then(|v| v.as_str()) {
        upsert_meta(&mut r, "description", d);
    }
    if let Some(k) = s.get("keywords").and_then(|v| v.as_str()) {
        upsert_meta(&mut r, "keywords", k);
    }
    upsert_og(&mut r, "og:title", s.get("og_title"));
    upsert_og(&mut r, "og:description", s.get("og_description"));
    upsert_og(&mut r, "og:image", s.get("og_image_url"));
    if let Some(sj) = s.get("schema_json").and_then(|v| v.as_str()) {
        upsert_schema(&mut r, sj);
    }
    let ga = s.get("ga_id").and_then(|v| v.as_str()).unwrap_or("");
    let gtm = s.get("gtm_id").and_then(|v| v.as_str()).unwrap_or("");
    remove_ga_gtm(&mut r);
    if !ga.is_empty() {
        inject_head(&mut r, &format!("<script async src=\"https://www.googletagmanager.com/gtag/js?id={}\"></script><script>window.dataLayer=window.dataLayer||[];function gtag(){{dataLayer.push(arguments);}}gtag('js',new Date());gtag('config','{}');</script>", ga, ga));
    }
    if !gtm.is_empty() {
        inject_head(&mut r, &format!("<script>(function(w,d,s,l,i){{w[l]=w[l]||[];w[l].push({{'gtm.start':new Date().getTime(),event:'gtm.js'}});var f=d.getElementsByTagName(s)[0],j=d.createElement(s);j.async=true;j.src='https://www.googletagmanager.com/gtm.js?id='+i;f.parentNode.insertBefore(j,f);}})(window,document,'script','dataLayer','{}');</script>", gtm));
    }
    // Script blocks carry an operator's arbitrary HTML, so they go in a marked region that this
    // function removes first — that (and not a `!is_empty()` check) is what makes a re-run a no-op.
    if let Some(hs) = s.get("head_scripts").and_then(|v| v.as_str()) {
        inject_marked_block(&mut r, hs, "</head>", HEAD_MARK_START, HEAD_MARK_END);
    }
    if let Some(bs) = s.get("body_scripts").and_then(|v| v.as_str()) {
        inject_marked_block(&mut r, bs, "</body>", BODY_MARK_START, BODY_MARK_END);
    }
    r
}

/// Drop the previously injected block (if any) and re-insert `content` before `at`.
/// `content.trim().is_empty()` is the removal case, so clearing the setting clears the page.
fn inject_marked_block(r: &mut String, content: &str, at: &str, start: &str, end: &str) {
    strip_marked(r, start, end);
    if content.trim().is_empty() {
        return;
    }
    if let Some(p) = r.rfind(at) {
        // [start .. end] here is exactly what strip_marked() removes on the next run.
        r.insert_str(p, &format!("{}{}\n{}", start, content, end));
    }
}

/// Remove every `<start>…<end>` region. An unterminated `start` is dropped on its own so a
/// half-written marker can never be left to accumulate.
fn strip_marked(r: &mut String, start: &str, end: &str) {
    while let Some(p) = r.find(start) {
        match r[p..].find(end) {
            Some(e) => {
                let stop = p + e + end.len();
                r.replace_range(p..stop, "");
            }
            None => r.replace_range(p..p + start.len(), ""),
        }
    }
}

fn replace_title(r: &mut String, t: &str) {
    if let Some(p) = r.find("<title>") {
        let a = p + 7;
        if let Some(e) = r[a..].find("</title>") {
            r.replace_range(a..a + e, t);
        }
    } else {
        inject_head(r, &format!("<title>{}</title>", t));
    }
}
fn upsert_meta(r: &mut String, n: &str, c: &str) {
    let pat = format!("<meta name=\"{}\"", n);
    if let Some(p) = r.find(&pat) {
        let a = &r[p..];
        if let Some(e) = a.find('>') {
            r.replace_range(
                p..p + e + 1,
                &format!("<meta name=\"{}\" content=\"{}\">", n, c),
            );
        }
    } else {
        inject_head(r, &format!("<meta name=\"{}\" content=\"{}\">", n, c));
    }
}
fn upsert_og(r: &mut String, p: &str, v: Option<&serde_json::Value>) {
    if let Some(c) = v.and_then(|v| v.as_str()) {
        let pat = format!("<meta property=\"{}\"", p);
        if let Some(pos) = r.find(&pat) {
            let a = &r[pos..];
            if let Some(e) = a.find('>') {
                r.replace_range(
                    pos..pos + e + 1,
                    &format!("<meta property=\"{}\" content=\"{}\">", p, c),
                );
            }
        } else {
            inject_head(r, &format!("<meta property=\"{}\" content=\"{}\">", p, c));
        }
    }
}
fn upsert_schema(r: &mut String, s: &str) {
    let o = r#"<script type="application/ld+json">"#;
    if let Some(p) = r.find(o) {
        let a = p + o.len();
        if let Some(e) = r[a..].find("</script>") {
            r.replace_range(a..a + e, s);
        }
    } else {
        inject_head(
            r,
            &format!(r#"<script type="application/ld+json">{}</script>"#, s),
        );
    }
}
fn remove_ga_gtm(r: &mut String) {
    for (sp, ep) in &[
        (
            r#"<script async src="https://www.googletagmanager.com/gtag/js"#,
            "</script>",
        ),
        (r#"<script>window.dataLayer"#, "</script>"),
        (r#"<script>(function(w,d,s,l,i)"#, "</script>"),
        (
            r#"<noscript><iframe src="https://www.googletagmanager.com/ns.html"#,
            "</noscript>",
        ),
    ] {
        loop {
            if let Some(p) = r.find(sp) {
                if let Some(e) = r[p..].find(ep) {
                    r.replace_range(p..p + e + ep.len(), "");
                    continue;
                }
            }
            break;
        }
    }
    while r.contains("\n\n\n") {
        *r = r.replace("\n\n\n", "\n\n");
    }
}
fn inject_head(r: &mut String, c: &str) {
    if let Some(p) = r.rfind("</head>") {
        r.insert_str(p, &format!("\n  {}", c));
    }
}
fn merge_json(a: serde_json::Value, b: serde_json::Value) -> serde_json::Value {
    match (a, b) {
        (serde_json::Value::Object(mut am), serde_json::Value::Object(bm)) => {
            for (k, v) in bm {
                am.insert(k, v);
            }
            serde_json::Value::Object(am)
        }
        (_, b) => b,
    }
}

fn default_site_settings() -> serde_json::Value {
    json!({
        "title": "CoreSwift CRM | Smart Customer Relationship Management",
        "description": "Automated follow-ups, smart pipelines, built-in calendar, and integrated workflows. The all-in-one CRM for growing businesses.",
        "keywords": "CRM, customer relationship management, sales pipeline, email automation, lead management, deal tracking",
        "og_title": "CoreSwift CRM — All-in-One Customer Relationship Platform",
        "og_description": "Automated follow-ups, smart pipelines, and integrated workflows for growing businesses.",
        "og_image_url": "", "favicon_url": "", "canonical_url": "https://coreswiftcrm.com",
        "ga_id": "", "gtm_id": "", "head_scripts": "", "body_scripts": "",
        "schema_json": "{\"@context\":\"https://schema.org\",\"@type\":\"SoftwareApplication\",\"name\":\"CoreSwift CRM\",\"applicationCategory\":\"BusinessApplication\",\"description\":\"All-in-one CRM platform with automated follow-ups, smart pipelines, and built-in calendar.\"}",
        "legal_tos": "", "legal_privacy": "", "legal_refunds": "",
        "homepage": { "headline": "The CRM That Works While You Sleep", "subheadline": "Automated follow-ups, smart pipelines, and built-in calendar." }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The served-page convention: an address in page TEXT is published as the entity, so the
    // applier — not the operator — is the enforcement point. Synthetic addresses only.
    #[test]
    fn an_address_in_legal_text_is_published_as_the_entity() {
        assert_eq!(
            escape_addresses("Email: support@example.com"),
            "Email: support&#64;example.com"
        );
        assert_eq!(
            escape_addresses("write to a.b+tag@ex-ample.co.uk."),
            "write to a.b+tag&#64;ex-ample.co.uk."
        );
    }

    #[test]
    fn an_address_at_the_end_of_a_sentence_is_still_escaped() {
        // Measured on ADASwift's terms/privacy (kanban t_1f427190): the sentence's full stop sits
        // inside the domain char run, so a naive scan skipped the address and would have published
        // a literal '@'. CoreSwift's served pages carry the same shape (`support&#64;…`).
        assert_eq!(
            escape_addresses("Contact: support@swiftsoftware.net.\n"),
            "Contact: support&#64;swiftsoftware.net.\n"
        );
    }

    #[test]
    fn a_plain_at_sign_that_is_not_an_address_is_left_alone() {
        assert_eq!(
            escape_addresses("@media (max-width: 600px)"),
            "@media (max-width: 600px)"
        );
        assert_eq!(
            escape_addresses("cost is 5@ 10 units"),
            "cost is 5@ 10 units"
        );
        assert_eq!(escape_addresses("a@b"), "a@b"); // no dotted domain
        assert_eq!(escape_addresses("v1@2.3"), "v1@2.3"); // numeric TLD
        assert_eq!(escape_addresses("@example.com"), "@example.com"); // no local part
    }

    #[test]
    fn an_already_escaped_address_is_not_double_escaped() {
        assert_eq!(
            escape_addresses("support&#64;example.com"),
            "support&#64;example.com"
        );
    }

    // The VALUE guard: a blank legal_* must never reach a target (SITE_ROOT is present on the host,
    // so the index is planned read-only; either way no legal page may be a target while blank).
    #[test]
    fn a_blank_legal_value_is_never_a_write_target() {
        let settings = json!({"legal_refunds": "", "legal_tos": "   ", "legal_privacy": "text"});
        let (targets, skipped) = plan(&settings);
        assert!(targets.iter().all(|(p, _)| !p.ends_with("refunds.html")));
        assert!(targets.iter().all(|(p, _)| !p.ends_with("terms.html")));
        assert!(skipped.iter().any(|(p, _)| p.ends_with("refunds.html")));
        assert!(skipped.iter().any(|(p, _)| p.ends_with("terms.html")));
    }
}
