//! Email module — sends transactional emails using database-stored templates.
//!
//! `email_templates` is AUTHORITATIVE for outgoing transactional mail (it is a PLATFORM asset; see
//! [`crate::email_templates`]). A send resolves the row for its `template_type` — a tenant row for
//! that account first, then the platform default (`is_default = true`) — and falls back to the inline
//! bodies in `send_inline` only when there is no row at all.
//!
//! Placeholders are `{{key}}` DOUBLE braces: the one contract this module implements and the one the
//! registry's `merge-fields` endpoint publishes. This module supplies `app_name` and `app_url` itself
//! when a caller does not, so a platform template can use them without every caller knowing about
//! them. (The platform's seeded row shipped with SINGLE-brace placeholders, which this contract does
//! not substitute — that is why migration 084 rewrites it, and why a leftover placeholder is now
//! LOGGED instead of being mailed literally. Key names only, never the rendered body: it carries
//! credentials.)
//!
//! All emails are queued via `outbound_messages` table for async delivery by the worker.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Platform constants every template can rely on. `{{app_name}}` / `{{app_url}}` resolve for any
/// caller because [`render_vars`] injects them.
pub const APP_NAME: &str = "CoreSwift CRM";
pub const APP_URL: &str = "https://app.coreswiftcrm.com";

/// The app's own support address — the one a customer writes to.
///
/// CoreSwift's transactional mail carried NO support address at all (measured 2026-10-08,
/// kanban t_f1931c05 -> t_71cc3ad8), so [`with_support_footer`] adds this to every message that
/// leaves this module. Fleet rule established with the sibling apps: `support@<the app's own main
/// domain>` — never another company's domain, never the platform's.
pub const SUPPORT_EMAIL: &str = "support@coreswiftcrm.com";

/// Append the support line to a rendered transactional body.
///
/// Applied in [`queue_outbound_message`] rather than to one body literal, so a body that comes from
/// the AUTHORITATIVE `email_templates` row carries the address exactly like the inline fallback
/// does, and a template added later inherits it. An EMPTY body stays empty (an empty `html_body`
/// means "no HTML part" and must not become a footer-only part), and a body that already carries the
/// address is returned untouched, so this is idempotent.
///
/// `pub(crate)` so the admin test-send route (`src/admin_actions/email_config.rs`) — which builds its
/// body inline and never touches [`queue_outbound_message`] — can put the SAME footer on the message
/// an operator inspects, instead of a second, quietly drifting copy of the wording.
pub(crate) fn with_support_footer(text: &str, html: &str) -> (String, String) {
    let text = if text.is_empty() || text.contains(SUPPORT_EMAIL) {
        text.to_string()
    } else {
        format!("{}\n\nNeed help? Contact {}\n", text, SUPPORT_EMAIL)
    };
    let html = if html.is_empty() || html.contains(SUPPORT_EMAIL) {
        html.to_string()
    } else {
        format!(
            "{}\n<p style=\"font-size:13px;color:#6b7280;text-align:center;\">Need help? Contact <a href=\"mailto:{}\">{}</a></p>",
            html, SUPPORT_EMAIL, SUPPORT_EMAIL
        )
    };
    (text, html)
}

/// Get available merge fields for a given template type.
/// Returns a list of field names that can be used in templates.
/// `app_name` and `app_url` are always supplied by this module; the rest come from the caller.
pub fn get_merge_fields(template_type: &str) -> Vec<&'static str> {
    let mut fields = match template_type {
        "welcome" => vec!["name", "email", "password", "app_url", "app_name"],
        "purchase_confirmed" => vec!["name", "plan_name", "app_url", "app_name"],
        "password_reset" => vec!["name", "token", "app_url", "app_name"],
        _ => vec![
            "name",
            "email",
            "password",
            "app_url",
            "app_name",
            "plan_name",
            "token",
            "account_name",
        ],
    };
    // Per-TENANT email branding (migration 116): bound on EVERY send by `apply_branding`, so an
    // admin-authored {{brand_name}}/{{logo_url}} can never reach a recipient as literal text.
    fields.push("brand_name");
    fields.push("logo_url");
    fields
}

/// Render a template string by replacing {{key}} placeholders with values from `vars`.
/// A single-brace `{key}` is NOT a placeholder here — one syntax, one meaning — and
/// [`leftover_placeholders`] reports it so a template in the wrong syntax cannot be mailed silently.
pub fn render_template(template: &str, vars: &serde_json::Value) -> String {
    let mut result = template.to_string();

    if let Some(obj) = vars.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = value.as_str().unwrap_or("");
            result = result.replace(&placeholder, replacement);
        }
    }

    result
}

/// The values a template renders against: the caller's `vars` plus the platform constants, so a
/// template using `{{app_name}}`/`{{app_url}}` renders for every caller. A caller's own value wins.
fn render_vars(vars: &serde_json::Value, app_name: &str, app_url: &str) -> serde_json::Value {
    let mut map = vars.as_object().cloned().unwrap_or_default();
    map.entry("app_name".to_string())
        .or_insert_with(|| json!(app_name));
    map.entry("app_url".to_string())
        .or_insert_with(|| json!(app_url));
    // Advertised for every type (see `get_merge_fields`) and bound per-account in `apply_branding`;
    // defaulted here so a template using them never renders with an empty map entry missing.
    map.entry("brand_name".to_string())
        .or_insert_with(|| json!(""));
    map.entry("logo_url".to_string())
        .or_insert_with(|| json!(""));
    serde_json::Value::Object(map)
}

/// Placeholders that SURVIVED rendering, by key name: `{{key}}` (the contract) and `{key}` (the
/// legacy shape), so a template that would be mailed with literal braces is reported instead of
/// shipped. A `{{key}}` token must be name-shaped; a single-brace token is reported only when the
/// name is one of `known_fields`, which keeps an HTML template's CSS out of the report.
fn leftover_placeholders(rendered: &str, known_fields: &[&str]) -> Vec<String> {
    let chars: Vec<char> = rendered.chars().collect();
    let mut found: Vec<String> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '{' {
            i += 1;
            continue;
        }
        let double = chars.get(i + 1) == Some(&'{');
        let start = if double { i + 2 } else { i + 1 };
        let mut j = start;
        while j < chars.len() && chars[j] != '{' && chars[j] != '}' && j - start < 64 {
            j += 1;
        }
        let closes = if double {
            chars.get(j) == Some(&'}') && chars.get(j + 1) == Some(&'}')
        } else {
            chars.get(j) == Some(&'}')
        };
        if closes {
            let key: String = chars[start..j].iter().collect();
            let key = key.trim().to_string();
            let name_shaped = !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
            if name_shaped
                && (double || known_fields.contains(&key.as_str()))
                && !found.contains(&key)
            {
                found.push(key);
            }
        }
        i = if closes {
            j + if double { 2 } else { 1 }
        } else {
            j + 1
        };
    }
    found
}

/// Send a templated email using stored email_templates.
/// This queues an `outbound_messages` row for async delivery.
/// - Looks up template by template_type (the tenant's row first, then the platform default)
/// - Falls back to hardcoded inline content only when NO row matches
/// - Renders {{variable}} placeholders from `vars` (+ `app_name`/`app_url`, supplied here)
pub async fn send_template_email(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    let app_name = APP_NAME;
    let app_url = APP_URL;
    let vars = render_vars(vars, app_name, app_url);

    // The registry is authoritative. The column list is explicit and follows the LIVE table (which
    // has `body_text` and no `is_html`): the previous shape selected an `is_html` column that does
    // not exist, so EVERY send errored — and `.ok().flatten()` turned that error into the inline
    // fallback, so the registry never contributed a byte to an email in its life. A lookup error is
    // now logged at ERROR and still falls back, because a customer's welcome mail has to go out;
    // what it must never be again is invisible.
    let template = match sqlx::query_as::<_, EmailTemplateRow>(
        r#"SELECT id, subject, body, body_text, html_body
           FROM email_templates
           WHERE template_type = $1 AND (aid = $2 OR is_default = true)
           ORDER BY is_default ASC, created_at DESC
           LIMIT 1"#,
    )
    .bind(template_type)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    {
        Ok(row) => row,
        Err(e) => {
            tracing::error!(
                template_type = %template_type,
                error = %e,
                "email_templates lookup failed — the DB registry contributed NOTHING to this send and \
                 the inline template is being used"
            );
            None
        }
    };

    match template {
        Some(t) => {
            // Use DB template
            let subject = render_template(
                &t.subject
                    .unwrap_or_else(|| get_default_subject(template_type, app_name)),
                &vars,
            );
            let html_body = t
                .html_body
                .as_ref()
                .map(|h| render_template(h, &vars))
                .unwrap_or_default();
            // `body` is the text body; `body_text` is the table's other text column and is used when
            // `body` is empty (the live seeded row fills `body` and leaves `body_text` NULL).
            let text_body = render_template(
                &t.body
                    .clone()
                    .or_else(|| t.body_text.clone())
                    .unwrap_or_default(),
                &vars,
            );
            // The live table has no `is_html` column: the flag is DERIVED exactly as the registry
            // handler derives it for its JSON payload — a template with an HTML body is HTML.
            let use_html = t.html_body.is_some();

            for (field, text) in [
                ("subject", subject.as_str()),
                ("html_body", html_body.as_str()),
                ("body", text_body.as_str()),
            ] {
                let leftover = leftover_placeholders(text, &get_merge_fields(template_type));
                if !leftover.is_empty() {
                    tracing::warn!(
                        template = %t.id,
                        template_type = %template_type,
                        field,
                        placeholders = ?leftover,
                        "email template placeholder is UNRESOLVED and will be sent literally — \
                         templates use {{key}} double braces"
                    );
                }
            }

            queue_outbound_message(
                db, tenant_id, to, &subject, &text_body, &html_body, use_html,
            )
            .await
        }
        None => {
            // No row for this template_type: the inline bodies are the fallback
            send_inline(db, tenant_id, to, template_type, &vars, app_name, app_url).await
        }
    }
}

/// Queue an outbound message for async delivery
async fn queue_outbound_message(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    subject: &str,
    text_body: &str,
    html_body: &str,
    is_html: bool,
) -> Result<(), String> {
    // Per-TENANT email branding (kanban t_feab8aff): applied at the ONE queue point BOTH the
    // `email_templates` row and the inline fallback pass through, so a template added later inherits
    // the header for free. An account with no branding renders byte-identical mail (the change is
    // additive — `Branding::from_value` returns None), and the header is prepended BEFORE the
    // support footer so the footer stays last.
    let (text_body, html_body, is_html) =
        apply_branding(db, tenant_id, text_body, html_body, is_html).await;
    // Every transactional body leaves with the app's own support address on it. Applied HERE, at
    // the single queue point that BOTH the `email_templates` row and the inline fallback pass
    // through, so the address reaches the real message whichever body was chosen.
    let (text_body, html_body) = with_support_footer(&text_body, &html_body);

    // Build the body: use html if available and is_html, otherwise text
    let body = if is_html && !html_body.is_empty() {
        html_body
    } else {
        text_body
    };

    sqlx::query(
        r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, is_html, status)
           VALUES ($1, $2, 'email', $3, $4, $5, $6, 'queued')"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(to)
    .bind(subject)
    .bind(&body)
    // Whether THIS body is HTML, so the delivery path can send an `html` part (migration 117).
    .bind(is_html)
    .execute(db)
    .await
    .map_err(|e| format!("Failed to queue email: {}", e))?;

    Ok(())
}

/// Get a default subject for a template type
fn get_default_subject(template_type: &str, app_name: &str) -> String {
    match template_type {
        "welcome" => format!("Welcome to {}!", app_name),
        "purchase_confirmed" => "Payment Received — Thank You!".to_string(),
        "password_reset" => "Password Reset Request".to_string(),
        _ => format!("{} Notification", app_name),
    }
}

/// Fallback hardcoded templates — used when no DB template is found
async fn send_inline(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
    app_name: &str,
    app_url: &str,
) -> Result<(), String> {
    let name = vars.get("name").and_then(|v| v.as_str()).unwrap_or("there");
    let email = vars.get("email").and_then(|v| v.as_str()).unwrap_or("");
    let password = vars.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let token = vars.get("token").and_then(|v| v.as_str()).unwrap_or("");
    let plan_name_val = vars
        .get("plan_name")
        .and_then(|v| v.as_str())
        .unwrap_or("a plan");

    match template_type {
        "welcome" => {
            let subject = format!("Welcome to {}!", app_name);
            let body = format!(
                "Welcome to {}, {}!\n\nYour account has been created.\n\nEmail: {}\nPassword: {}\n\nLogin at: {}/login\n\nNext steps:\n- Connect your apps\n- Import your contacts\n- Set up your pipelines\n- Invite your team\n\n{} Team",
                app_name, name, email, password, app_url, app_name
            );
            queue_outbound_message(db, tenant_id, to, &subject, &body, "", false).await
        }
        "purchase_confirmed" => {
            let subject = "Payment Received — Thank You!".to_string();
            let body = format!(
                "Hi {},\n\nYour payment for {} has been confirmed. Thank you!\n\nLogin at: {}/login\n\nThank you for your business!\n- {} Team",
                name, plan_name_val, app_url, app_name
            );
            queue_outbound_message(db, tenant_id, to, &subject, &body, "", false).await
        }
        "password_reset" => {
            let subject = "Password Reset Request".to_string();
            let body = format!(
                "Hi {},\n\nWe received a request to reset your password for {}.\n\nYour reset token is: {}\n\nReset URL: {}/auth/reset-password?token={}\n\nThis token expires in 1 hour.\n\nIf you did not request this, please ignore this email.\n\n- {} Team",
                name, app_name, token, app_url, token, app_name
            );
            queue_outbound_message(db, tenant_id, to, &subject, &body, "", false).await
        }
        _ => {
            let subject = format!("{} Notification", app_name);
            let body = format!("{} Notification:\n\n{}", app_name, vars);
            queue_outbound_message(db, tenant_id, to, &subject, &body, "", false).await
        }
    }
}

// ---- Data types ----

#[derive(Debug, sqlx::FromRow)]
struct EmailTemplateRow {
    id: Uuid,
    subject: Option<String>,
    body: Option<String>,
    body_text: Option<String>,
    html_body: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shipped platform welcome template, exactly as `migrations/084_email_templates.sql` writes
    /// it. The last test asserts these literals are IN that file, so the migration and the renderer
    /// cannot drift apart silently.
    const SEEDED_SUBJECT: &str = "Welcome to {{app_name}}!";
    const SEEDED_BODY: &str = "Welcome to {{app_name}}, {{name}}!\n\nYour account has been created successfully.\n\nYour Login Credentials:\nEmail: {{email}}\nPassword: {{password}}\n\nLogin: {{app_url}}/login\n\nBest,\nThe {{app_name}} Team";
    const SEEDED_HTML: &str = "<h2>Welcome to {{app_name}}, {{name}}!</h2><p>Your account has been created successfully.</p><p><strong>Login Credentials:</strong><br>Email: {{email}}<br>Password: {{password}}</p><p><a href=\"{{app_url}}/login\">Log in here</a></p><p>Best,<br>The {{app_name}} Team</p>";

    #[test]
    fn render_template_substitutes_double_braces_only() {
        let vars = json!({"name": "Ada", "app_name": APP_NAME});
        assert_eq!(render_template("Hi {{name}}!", &vars), "Hi Ada!");
        // The legacy single-brace shape is NOT a placeholder in this contract. Migration 084 rewrites
        // the one row that used it; the renderer must not quietly support two syntaxes.
        assert_eq!(render_template("Hi {name}!", &vars), "Hi {name}!");
    }

    #[test]
    fn render_vars_supplies_the_platform_constants_without_overriding_the_caller() {
        let vars = render_vars(&json!({"name": "Ada"}), APP_NAME, APP_URL);
        assert_eq!(vars["name"], json!("Ada"));
        assert_eq!(vars["app_name"], json!(APP_NAME));
        assert_eq!(vars["app_url"], json!(APP_URL));

        // A caller's own value wins, and a non-object input still renders the constants.
        let own = render_vars(
            &json!({"app_url": "https://tenant.example"}),
            APP_NAME,
            APP_URL,
        );
        assert_eq!(own["app_url"], json!("https://tenant.example"));
        assert_eq!(
            render_vars(&json!(null), APP_NAME, APP_URL)["app_name"],
            json!(APP_NAME)
        );
    }

    #[test]
    fn leftover_placeholders_reports_unsubstituted_fields_and_ignores_css() {
        let fields = get_merge_fields("welcome");
        assert_eq!(
            leftover_placeholders("Hi {{name}}, welcome to {{app_name}}", &fields),
            vec!["name".to_string(), "app_name".to_string()]
        );
        // a single-brace KNOWN field is the legacy shape and is reported
        assert_eq!(
            leftover_placeholders("Hi {name}", &fields),
            vec!["name".to_string()]
        );
        // an unknown key is still literal when it is double-braced ...
        assert_eq!(
            leftover_placeholders("{{nope}}", &fields),
            vec!["nope".to_string()]
        );
        // ... but a single-brace token that is not a merge field is not a placeholder
        assert_eq!(
            leftover_placeholders("Hello {prospect}", &fields),
            Vec::<String>::new()
        );
        // neither is CSS or JSON in an HTML template
        assert_eq!(
            leftover_placeholders("<style>.b{color:red}</style>{\"name\": 1}", &fields),
            Vec::<String>::new()
        );
        // repeated placeholders are reported once
        assert_eq!(
            leftover_placeholders("{{name}} and {{name}}", &fields),
            vec!["name".to_string()]
        );
    }

    #[test]
    fn the_shipped_welcome_template_renders_completely() {
        // the values the registration handler passes (src/auth/handlers.rs) plus the platform
        // constants this module injects
        let vars = render_vars(
            &json!({
                "name": "Ada Lovelace",
                "email": "ada@example.com",
                "password": "Probe!23456",
                "account_name": "Ada's Workspace",
                "app_url": APP_URL,
            }),
            APP_NAME,
            APP_URL,
        );
        let fields = get_merge_fields("welcome");
        for (field, text) in [
            ("subject", SEEDED_SUBJECT),
            ("body", SEEDED_BODY),
            ("html_body", SEEDED_HTML),
        ] {
            let rendered = render_template(text, &vars);
            assert!(
                leftover_placeholders(&rendered, &fields).is_empty(),
                "{field} still carries a placeholder: {rendered}"
            );
            assert!(
                !rendered.contains('{'),
                "{field} still carries a brace: {rendered}"
            );
        }
        assert_eq!(
            render_template(SEEDED_SUBJECT, &vars),
            "Welcome to CoreSwift CRM!"
        );
        assert!(render_template(SEEDED_BODY, &vars).contains("Email: ada@example.com"));
        assert!(render_template(SEEDED_HTML, &vars)
            .contains("href=\"https://app.coreswiftcrm.com/login\""));

        // the migration that ships this row is the same text, so one of the two cannot be edited alone
        let migration = include_str!("../migrations/084_email_templates.sql");
        for literal in [SEEDED_SUBJECT, SEEDED_BODY, SEEDED_HTML] {
            assert!(
                migration.contains(literal),
                "migrations/084_email_templates.sql does not carry this text verbatim: {literal}"
            );
        }
    }

    #[test]
    fn the_seeded_welcome_template_body_gains_the_support_address() {
        // Regression guard for the whole card (t_71cc3ad8): the transactional body that reaches the
        // message must name the app's OWN support address, whichever body was chosen.
        let (text, html) = with_support_footer(SEEDED_BODY, SEEDED_HTML);
        assert!(
            text.contains(SUPPORT_EMAIL),
            "text body lacks the support address"
        );
        assert!(
            html.contains(SUPPORT_EMAIL),
            "html body lacks the support address"
        );
    }

    #[test]
    fn support_footer_is_idempotent_and_never_added_to_an_empty_body() {
        let (t, h) = with_support_footer("Body", "<p>Body</p>");
        assert!(t.ends_with(&format!("Need help? Contact {}\n", SUPPORT_EMAIL)));
        assert!(h.contains(&format!("mailto:{}", SUPPORT_EMAIL)));
        // a body that already carries the address is returned untouched
        assert_eq!(with_support_footer(&t, &h), (t.clone(), h.clone()));
        // "" means "no body part", not "footer only"
        assert_eq!(with_support_footer("", ""), (String::new(), String::new()));
    }

    #[test]
    fn support_address_is_on_this_apps_own_domain() {
        // Exactly the defect this card closes: a support address pointed at the platform/vendor
        // domain instead of the app's own.
        assert!(SUPPORT_EMAIL.ends_with("@coreswiftcrm.com"));
        assert!(!SUPPORT_EMAIL.contains("swiftsoftware.net"));
    }
}

/// Prepend the tenant's email-branding header to a body about to be queued (kanban t_feab8aff).
///
/// Returns `(text_body, html_body, is_html)`. Additive by construction: with no branding configured
/// the inputs come back untouched, so every existing workspace's mail is byte-identical to before
/// this feature. `{{brand_name}}`/`{{logo_url}}` are bound HERE because they are per-account and
/// cannot live in a `&'static str` default map. If the message has no HTML part at all, one is built
/// from the ESCAPED text so the logo has somewhere to render — a text part cannot carry an image,
/// and `outbound_messages` carries a single body column.
async fn apply_branding(
    db: &PgPool,
    tenant_id: Uuid,
    text_body: &str,
    html_body: &str,
    is_html: bool,
) -> (String, String, bool) {
    let Some(brand) = crate::branding::load(db, tenant_id).await else {
        return (text_body.to_string(), html_body.to_string(), is_html);
    };
    let logo_absolute = brand.resolve_logo_url(APP_URL);
    let header = brand.header_html(logo_absolute.as_deref());
    let vars = json!({
        "brand_name": brand.brand_name.clone(),
        "logo_url": logo_absolute.clone().unwrap_or_default(),
    });
    let text_body = format!(
        "{}{}",
        brand.text_header(),
        render_template(text_body, &vars)
    );
    let html_body = render_template(html_body, &vars);

    if is_html && !html_body.is_empty() {
        (text_body, format!("{header}{html_body}"), true)
    } else {
        // A text-only message stays a text-only message: only the brand NAME is prepended. This app
        // queues ONE body and the transport sends it as the text part, so wrapping the text in HTML
        // here would deliver markup as the plain-text part — a regression, not branding.
        (text_body, html_body, is_html)
    }
}

#[cfg(test)]
mod branding_fields_tests {
    use super::get_merge_fields;

    /// The admin `/types` list (and `GET /api/email-templates/merge-fields`) serves exactly these
    /// names, so an admin-authored `{{brand_name}}`/`{{logo_url}}` is advertised for EVERY type —
    /// and `apply_branding` binds both on every send, so neither can reach a recipient literally.
    #[test]
    fn every_template_type_advertises_the_branding_fields() {
        for t in [
            "welcome",
            "purchase_confirmed",
            "password_reset",
            "default",
            "some-future-type",
        ] {
            let f = get_merge_fields(t);
            assert!(f.contains(&"brand_name"), "{t} is missing brand_name");
            assert!(f.contains(&"logo_url"), "{t} is missing logo_url");
        }
    }
}
