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

/// Materialize `settings` into the static marketing page + the three legal pages.
///
/// Returns `(written, skipped)`; each skipped entry is `(path, reason)`. Nothing here is fatal —
/// the targets only exist where the files do, and the caller prints or returns the outcome so no
/// surface can claim a regeneration that did not happen.
///
/// A legal field that is absent or blank is left ALONE (the code defaults carry `""`), so running
/// the applier can never blank out a live policy page.
pub(crate) fn apply_to_disk(settings: &serde_json::Value) -> (Vec<String>, Vec<(String, String)>) {
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();

    if !std::path::Path::new(SITE_ROOT).is_dir() {
        skipped.push((
            SITE_ROOT.to_string(),
            "directory is not present in this runtime (host-only path)".to_string(),
        ));
        return (written, skipped);
    }

    match fs::read_to_string(SITE_INDEX) {
        Ok(before) => {
            let after = inject_settings(&before, settings);
            if after == before {
                skipped.push((SITE_INDEX.to_string(), "unchanged".to_string()));
            } else {
                match fs::write(SITE_INDEX, &after) {
                    Ok(_) => written.push(SITE_INDEX.to_string()),
                    Err(e) => skipped.push((SITE_INDEX.to_string(), e.to_string())),
                }
            }
        }
        Err(e) => skipped.push((SITE_INDEX.to_string(), e.to_string())),
    }

    for (slug, title, key) in [
        ("terms", "Terms of Service", "legal_tos"),
        ("privacy", "Privacy Policy", "legal_privacy"),
        ("refunds", "Refund & Cancellation Policy", "legal_refunds"),
    ] {
        // Blank or absent means "no policy text configured" -> leave the file that is there.
        let text = match settings.get(key).and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t,
            _ => continue,
        };
        let path = format!("{}{}.html", SITE_ROOT, slug);
        let page = legal_page(title, text);
        match fs::read_to_string(&path) {
            Ok(before) if before == page => {
                skipped.push((path, "unchanged".to_string()));
            }
            _ => match fs::write(&path, &page) {
                Ok(_) => written.push(path),
                Err(e) => skipped.push((path, e.to_string())),
            },
        }
    }

    (written, skipped)
}

/// One legal page, as a pure function so the applier can compare before writing.
fn legal_page(title: &str, text: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="UTF-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{} — CoreSwift CRM</title><style>body{{font-family:system-ui,sans-serif;background:#0f0f0f;color:#e5e5e5;line-height:1.7;margin:0;padding:0}}.container{{max-width:800px;margin:0 auto;padding:60px 24px}}h1{{font-size:2rem;color:#f59e0b}}a{{color:#f59e0b}}</style></head><body><div class="container"><h1>{}</h1>{}</div></body></html>"#,
        title, title, text
    )
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
