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
/// The mail client's own front door (B211): the storage reminder links the owner to the screen where
/// the messages and the meter actually are, not to the CRM dashboard.
pub const APP_URL_MAIL: &str = "https://inbox.coreswiftcrm.com";

/// The app's OWN support channel — a PAGE, not a mailbox (measured 2026-11-07 on dns.google and
/// 8.8.8.8): every address this product printed lived on a domain publishing **MX = 0**, with
/// nothing on this host listening on port 25, so a customer's message BOUNCED. The app serves a
/// contact form on its own host, `POST /api/public/support-request`, that files the message into
/// the operator's own support queue — so [`with_support_footer`] names that page.
///
/// History: the footer first carried no address at all (kanban t_71cc3ad8), then
/// `support@coreswiftcrm.com` (apex publishes no MX — a bounce), then
/// `support@swiftsoftware.net` (kanban t_6a779fb6, on the claim that it published Cloudflare
/// MX; that MX was later removed and the address bounces too — measured 2026-11-07 with
/// dns.google and 8.8.8.8, NOERROR/NODATA, SOA only). A domain with no mail exchanger is not an
/// address, it is a bounce, so the fleet's rule is now a page on a host this app actually serves.
/// ADASwift, WorkflowSwift, IncentiveSwift, FunnelSwift and MissedCall all name their own contact
/// page the same way.
pub const SUPPORT_CONTACT: &str = "https://app.coreswiftcrm.com/contact.html";

/// Append the support line to a rendered transactional body.
///
/// Applied in [`queue_outbound_message`] rather than to one body literal, so a body that comes from
/// the AUTHORITATIVE `email_templates` row carries the contact page exactly like the inline
/// fallback does, and a template added later inherits it. An EMPTY body stays empty (an empty
/// `html_body` means "no HTML part" and must not become a footer-only part), and a body that
/// already carries the page is returned untouched, so this is idempotent.
///
/// `pub(crate)` so the admin test-send route (`src/admin_actions/email_config.rs`) — which builds its
/// body inline and never touches [`queue_outbound_message`] — can put the SAME footer on the message
/// an operator inspects, instead of a second, quietly drifting copy of the wording.
pub(crate) fn with_support_footer(text: &str, html: &str) -> (String, String) {
    let text = if text.is_empty() || text.contains(SUPPORT_CONTACT) {
        text.to_string()
    } else {
        format!("{}\n\nNeed help? {}\n", text, SUPPORT_CONTACT)
    };
    let html = if html.is_empty() || html.contains(SUPPORT_CONTACT) {
        html.to_string()
    } else {
        format!(
            "{}\n<p style=\"font-size:13px;color:#6b7280;text-align:center;\">Need help? <a href=\"{}\">Send us a message</a> — no sign-in needed.</p>",
            html, SUPPORT_CONTACT
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
        // The sign-in address change (kanban t_bd31809e). `link` is the whole point of the message —
        // the customer clicks it — and `new_email` is printed back so a typo is visible before they
        // act on it. Registry-authored rows can use these; the inline arm below is what is emitted
        // while `email_templates` has no `email_change` row.
        "email_change" => vec!["name", "new_email", "link", "token", "app_url", "app_name"],
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
    let rendered = render_email(db, tenant_id, template_type, vars).await?;
    queue_rendered(db, tenant_id, to, &rendered).await
}

/// A transactional email exactly as this app QUEUES it: the subject plus the ONE body the transport
/// delivers, after the workspace's branded header and the support footer have been applied.
///
/// Produced by [`render_email`], which stores nothing — so the console's branding preview can show
/// the bytes the mailer really produces instead of drawing its own copy of
/// `Branding::header_html` in JavaScript, which is a SECOND implementation of the renderer and can
/// therefore describe a message this app never sends (kanban t_c5bec49c).
#[derive(Debug, Clone)]
pub struct RenderedEmail {
    /// The subject line, merge fields resolved.
    pub subject: String,
    /// The plain-text part, with the brand name in front of it.
    pub text: String,
    /// The HTML part with the branded header, or empty when the message has no HTML body.
    pub html: String,
    /// Whether the body being delivered is `html` (one body column, migration 117).
    pub is_html: bool,
    /// Whether this workspace has branding of its OWN configured (a name or a logo).
    pub branded: bool,
    /// Whether that branding carries a logo this workspace uploaded.
    pub own_logo: bool,
}

/// Render a templated email through the ONE path a real send uses, and RETURN it instead of
/// queueing it.
///
/// Every step of a send happens here — template selection, merge fields, the branded header, the
/// support footer — up to but excluding the `outbound_messages` INSERT. A caller that only wants to
/// SEE the message (the branding preview) therefore cannot drift from the message a customer
/// receives. Nothing is mailed and no row is written.
pub async fn render_email(
    db: &PgPool,
    tenant_id: Uuid,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<RenderedEmail, String> {
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

    let (template_id, subject, text_body, html_body, use_html) = match template {
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
            (Some(t.id), subject, text_body, html_body, use_html)
        }
        None => {
            let (subject, text_body, html_body, use_html) =
                inline_bodies(template_type, &vars, app_name, app_url);
            (None, subject, text_body, html_body, use_html)
        }
    };

    for (field, text) in [
        ("subject", subject.as_str()),
        ("html_body", html_body.as_str()),
        ("body", text_body.as_str()),
    ] {
        let leftover = leftover_placeholders(text, &get_merge_fields(template_type));
        if !leftover.is_empty() {
            tracing::warn!(
                template = ?template_id,
                template_type = %template_type,
                field,
                placeholders = ?leftover,
                "email template placeholder is UNRESOLVED and will be sent literally — \
                 templates use {{key}} double braces"
            );
        }
    }

    let (text, html, is_html, branded, own_logo) =
        compose_body(db, tenant_id, &text_body, &html_body, use_html).await;

    Ok(RenderedEmail {
        subject,
        text,
        html,
        is_html,
        branded,
        own_logo,
    })
}

/// The branding + support-footer pass, applied at the ONE place a body becomes a message.
///
/// Returns `(text, html, is_html, branded, own_logo)`. BOTH the queueing path and the preview path
/// go through it, so the header on screen is the header in the mail.
///
/// Per-workspace email branding (kanban t_feab8aff) is applied at the ONE point both the
/// `email_templates` row and the inline fallback pass through, so a template added later inherits the
/// header for free. A workspace that has configured NONE still sends a header — the app's own name
/// and wordmark (kanban t_5054a5c9), because the first mail a brand-new customer receives must say
/// who it is from. Every transactional body also leaves with the app's own support address on it,
/// applied HERE for the same one-place reason and AFTER the header so the footer stays last.
async fn compose_body(
    db: &PgPool,
    tenant_id: Uuid,
    text_body: &str,
    html_body: &str,
    is_html: bool,
) -> (String, String, bool, bool, bool) {
    let stored = crate::branding::load(db, tenant_id).await;
    let branded = stored.is_some();
    let own_logo = stored
        .as_ref()
        .and_then(|b| b.resolve_logo_url(APP_URL))
        .is_some();
    let branding = stored.unwrap_or_else(app_default_branding);
    let (text_body, html_body, is_html) =
        apply_branding(Some(&branding), text_body, html_body, is_html);
    let (text_body, html_body) = with_support_footer(&text_body, &html_body);
    (text_body, html_body, is_html, branded, own_logo)
}

/// Queue a rendered email for async delivery
async fn queue_rendered(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    rendered: &RenderedEmail,
) -> Result<(), String> {
    // Build the body: use html if available and is_html, otherwise text
    let body = if rendered.is_html && !rendered.html.is_empty() {
        rendered.html.as_str()
    } else {
        rendered.text.as_str()
    };

    sqlx::query(
        r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, is_html, status)
           VALUES ($1, $2, 'email', $3, $4, $5, $6, 'queued')"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(to)
    .bind(&rendered.subject)
    .bind(body)
    // Whether THIS body is HTML, so the delivery path can send an `html` part (migration 117).
    .bind(rendered.is_html)
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
        "email_change" => "Confirm your new sign-in email".to_string(),
        _ => format!("{} Notification", app_name),
    }
}

/// Fallback hardcoded templates — used when no DB template is found.
///
/// Returns `(subject, text_body, html_body, use_html)` instead of queueing, so the preview surface
/// renders the same message a send would produce. Every arm is plain text (`use_html` false),
/// exactly as the inline path has always been: wrapping these in HTML would change what a customer
/// receives today.
fn inline_bodies(
    template_type: &str,
    vars: &serde_json::Value,
    app_name: &str,
    app_url: &str,
) -> (String, String, String, bool) {
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
            (subject, body, String::new(), false)
        }
        "purchase_confirmed" => {
            let subject = "Payment Received — Thank You!".to_string();
            let body = format!(
                "Hi {},\n\nYour payment for {} has been confirmed. Thank you!\n\nLogin at: {}/login\n\nThank you for your business!\n- {} Team",
                name, plan_name_val, app_url, app_name
            );
            (subject, body, String::new(), false)
        }
        "password_reset" => {
            let subject = "Password Reset Request".to_string();
            let body = format!(
                "Hi {},\n\nWe received a request to reset your password for {}.\n\nYour reset token is: {}\n\nReset URL: {}/auth/reset-password?token={}\n\nThis token expires in 1 hour.\n\nIf you did not request this, please ignore this email.\n\n- {} Team",
                name, app_name, token, app_url, token, app_name
            );
            (subject, body, String::new(), false)
        }
        // The sign-in address change (kanban t_bd31809e). Plain text on purpose: the arm below is what
        // is emitted while this workspace has no `email_change` row, and a merge field the customer
        // typed (`new_email`) must never reach an HTML part unescaped. Two things the message has to
        // say — the reset mail's history taught both: WHERE to click (a link, not a code) and that
        // NOTHING HAS CHANGED YET (the old address keeps working until the link is opened).
        "email_change" => {
            let new_email_val = vars
                .get("new_email")
                .and_then(|v| v.as_str())
                .unwrap_or("(the new address)");
            let link = vars.get("link").and_then(|v| v.as_str()).unwrap_or("");
            let subject = "Confirm your new sign-in email".to_string();
            let body = format!(
                "Hi {name},\n\n\
                 We received a request to change the email address you sign in to {app_name} with, to:\n\n    {new_email_val}\n\n\
                 To confirm, open this link:\n\n    {link}\n\n\
                 The link works once and expires in 2 hours. Until you open it, nothing changes: your \
                 current address still works and your password stays the same. If you did not ask for \
                 this, ignore this email and nothing will happen.\n\n\
                 - The {app_name} Team"
            );
            (subject, body, String::new(), false)
        }
        // The storage rung reminder (B206). Plain text on purpose: it carries numbers and a plain
        // instruction, and it must read the same in every mail client. It NEVER says mail will be
        // deleted to make room — that is the promise the whole rule exists to keep. The subject
        // states the actual figures because a customer reads the subject first.
        "storage_warning" => {
            let used = vars.get("used").and_then(|v| v.as_str()).unwrap_or("some");
            let quota = vars
                .get("quota")
                .and_then(|v| v.as_str())
                .unwrap_or("your plan");
            let percent = vars.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
            let threshold = vars.get("threshold").and_then(|v| v.as_i64()).unwrap_or(50);
            let inbox_url = vars
                .get("inbox_url")
                .and_then(|v| v.as_str())
                .unwrap_or(app_url);
            let plans_url = vars
                .get("plans_url")
                .and_then(|v| v.as_str())
                .unwrap_or(app_url);
            let retention = vars.get("retention").and_then(|v| v.as_str()).unwrap_or("");
            let subject = format!(
                "Your mailbox is {}% full — {} of {} used",
                percent, used, quota
            );
            let action = if percent >= 100 {
                "Your mailbox is FULL. Mail is still arriving and nothing has been deleted, but new attachments \
                 and files are paused until you clear space or upgrade."
            } else if threshold >= 99 {
                "Your mailbox is almost full. Plain mail still arrives and nothing is deleted, but once the \
                 allowance is reached new attachments and files are paused."
            } else {
                "You have room for now. Clearing out mail you no longer need (Trash and Spam count too) keeps \
                 the meter down, and a bigger plan raises the allowance."
            };
            let body = format!(
                "Hi {name},\n\n\
                 This is a storage update for your mailbox: you are using {used} of {quota} ({percent}%).\n\n\
                 {action}\n\n\
                 Two things that are ALWAYS true here: mail sent to you keeps arriving, and nothing is deleted \
                 to make room for new mail. The only thing a full mailbox pauses is adding new attachments or \
                 files.\n\n\
                 Open your mail and clear some space:\n    {inbox_url}\n\n\
                 See the plans with more storage:\n    {plans_url}\n\n\
                 {retention}\n\n\
                 You will get at most one of these messages per storage level per month.\n\n\
                 - The {app_name} Team"
            );
            (subject, body, String::new(), false)
        }
        _ => {
            let subject = format!("{} Notification", app_name);
            let body = format!("{} Notification:\n\n{}", app_name, vars);
            (subject, body, String::new(), false)
        }
    }
}

/// The template type the branding preview renders: the FIRST mail a new customer receives, which is
/// the one a customer judges this workspace's branding on.
pub const PREVIEW_TEMPLATE_TYPE: &str = "welcome";

/// What the preview prints where the real mail prints the customer's password.
///
/// The welcome mail IS the credentials mail, so it carries `{{password}}`. A preview must never show
/// a plausible credential: masking it keeps the pane honest and makes the rendered message useless
/// as a credential, since nothing is read from the row and nothing is mailed.
pub const PREVIEW_CREDENTIAL_MASK: &str = "••••••••";

/// Render the message this workspace's branding actually produces, for the console's preview pane.
///
/// `name`/`to` describe the CALLER — the handler reads them from the signed-in user's own row — so
/// nothing here can address a third party. Returns the bytes; sends nothing.
pub async fn render_branding_preview(
    db: &PgPool,
    tenant_id: Uuid,
    name: &str,
    to: &str,
) -> Result<RenderedEmail, String> {
    let vars = branding_preview_vars(name, to);
    render_email(db, tenant_id, PREVIEW_TEMPLATE_TYPE, &vars).await
}

/// The merge fields the branding sample is built from — ONE builder for the pane and the mail, so
/// the rendered sample and the mailed sample cannot describe different messages.
fn branding_preview_vars(name: &str, to: &str) -> serde_json::Value {
    json!({
        "name": name,
        "email": to,
        "password": PREVIEW_CREDENTIAL_MASK,
    })
}

/// Mail the branding sample to the CALLER'S OWN address (kanban t_90845bc1).
///
/// The send twin of [`render_branding_preview`]: same template type, same merge fields, and it goes
/// through [`send_template_email`] — the ONE funnel that renders, applies the workspace's branded
/// header plus the support footer and queues an `outbound_messages` row. So the mail a workspace
/// operator receives carries exactly the bytes `GET /api/account/branding/preview-body` shows them,
/// which is the only way to confirm what a customer's mail client really renders.
///
/// `to` is the caller's own `users.email`, read by the handler from the signed-in row — this function
/// has no caller-supplied recipient, so it can never be used as a relay.
pub async fn send_branding_preview_email(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    name: &str,
) -> Result<(), String> {
    let vars = branding_preview_vars(name, to);
    send_template_email(db, tenant_id, to, PREVIEW_TEMPLATE_TYPE, &vars).await
}

/// The operator alert's two bodies, built from the asker's own words.
///
/// Split out so the escaping is testable without a database. The TEXT part keeps the words exactly
/// as typed (a human reads them); the HTML part escapes `& < > " '`, so no word of a stranger's
/// message can inject markup into the operator's inbox.
fn support_alert_bodies(
    who: &str,
    reply_to: &str,
    subject_line: &str,
    message: &str,
) -> (String, String) {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    let text = format!(
        "{who} wrote to support.\n\n\
         Subject: {subject_line}\n\
         Reply to: {reply_to}\n\n\
         {message}\n\n\
         Answer it in the admin console (Support -> Tickets) and the asker is mailed your reply.\n"
    );
    let html = format!(
        "<html><body style=\"font-family:Arial,sans-serif;line-height:1.6;color:#333\">\
         <p><strong>{}</strong> wrote to support.</p>\
         <p>Subject: <strong>{}</strong></p>\
         <p>Reply to: <strong>{}</strong></p>\
         <pre style=\"white-space:pre-wrap;font-family:inherit\">{}</pre>\
         <p>Answer it in the admin console (Support &rarr; Tickets) and the asker is mailed your reply.</p>\
         </body></html>",
        esc(who),
        esc(subject_line),
        esc(reply_to),
        esc(message),
    );
    (text, html)
}

/// Tell the OPERATOR that somebody wrote to support (2026-11-09).
///
/// WHY THIS EXISTS (measured live on the sibling apps the same day): `POST /api/public/support-request`
/// — the contact page's form, the door a LOCKED-OUT customer has — validated the input, filed ONE
/// ticket and answered 201 "Your message is with our team" while queueing ZERO mail. The request
/// waited in a console nobody had been told to open, so the person who cannot sign in was the person
/// nobody answered. IncentiveSwift, WorkflowSwift, ADASwift, MissedCall and FunnelSwift closed the
/// identical gap on the same day; this is CoreSwift's half.
///
/// Destination = the operator's own `operator_alert_email` (`admin_settings.email`, set from the
/// console's Email Provider card). An unset address sends nothing: this code never invents a
/// mailbox. Sent through the PLATFORM mail transport explicitly, never a workspace's own identity,
/// so the alert still arrives when a workspace's mail server is the broken thing. The asker's words
/// are HTML-escaped before they enter the operator's inbox, and the attempt is recorded in the
/// app's one send log (`outbound_messages`, with NO workspace) so the panel can answer "did the
/// provider take it?" instead of this function's opinion.
///
/// Best effort by design: the ticket row is already the record, so a failure is logged by the caller
/// and never turns a filed request into an error page.
pub async fn send_support_request_operator_alert(
    db: &PgPool,
    who: &str,
    reply_to: &str,
    subject_line: &str,
    message: &str,
) -> Result<(), String> {
    // ── A HARNESS MUST NEVER PAGE A HUMAN (kanban t_a27fd2a4, fleet parity t_f17b5802) ────────
    // The recipient of this alert is the OPERATOR whatever address the requester typed, so the
    // requester is the only thing that can tell a customer from a proof run. Measured on the
    // siblings 2026-11-10: one day of audit walks produced 49 operator alerts, every probe row on
    // a reserved address. The ticket is still filed (it is the record and it stays in the queue);
    // only the mail to a person is withheld — BEFORE any row is queued, so no `outbound_messages`
    // row is written either.
    if let Some(reason) = crate::security::probe_addr::synthetic_requester(reply_to, who) {
        tracing::info!(
            reason = reason,
            requester = %reply_to,
            "operator alert WITHHELD: the requester is a synthetic (harness) identity"
        );
        return Ok(());
    }

    let Some(to) = crate::communications::platform_mail_config::operator_alert_email(db).await
    else {
        // Nothing configured is a normal state, not a failure: no mailbox is invented.
        return Ok(());
    };
    let Some(transport) = crate::communications::platform_mail_config::resolved(db).await else {
        return Err(
            "the platform mail transport is not configured, so the operator alert was not sent"
                .to_string(),
        );
    };

    let who = if who.trim().is_empty() {
        "A customer"
    } else {
        who.trim()
    };
    let subject = format!("A support request needs an answer: {}", subject_line.trim());
    let (text, html) = support_alert_bodies(who, reply_to, subject_line, message);

    // Recorded with `tenant_id` NULL: this alert belongs to no workspace. The row is what the
    // Communications Log shows, so the send is auditable like every other mail this app sends.
    let msg_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, is_html, status)
         VALUES ($1, NULL, 'email', $2, $3, $4, false, 'queued')",
    )
    .bind(msg_id)
    .bind(&to)
    .bind(&subject)
    .bind(&text)
    .execute(db)
    .await
    .map_err(|e| format!("could not record the operator alert: {e}"))?;

    // The platform transport is named explicitly (never resolved per workspace). `Uuid::nil()` is
    // bookkeeping only: the message belongs to no workspace.
    let cfg = crate::communications::providers::platform_send_config(
        Uuid::nil(),
        msg_id,
        &to,
        &subject,
        &text,
        Some(&html),
        &transport.mail(),
    );
    let outcome = crate::communications::providers::deliver(&cfg).await;
    if let Err(e) = crate::communications::providers::record_attempt(db, msg_id, &outcome).await {
        tracing::error!(msg = %msg_id, error = %e, "failed to record the support-request operator alert");
    }
    if outcome.ok {
        tracing::info!(
            msg = %msg_id,
            to = %to,
            "support-request operator alert accepted by the provider"
        );
        return Ok(());
    }

    // No workspace owns this row, so the queue can never retry it (the worker retires a NULL-tenant
    // row): close it out here with the provider's own words rather than leave a retry that cannot run.
    if let Err(e) = sqlx::query(
        "UPDATE outbound_messages SET status = 'failed', scheduled_at = NULL \
          WHERE id = $1 AND status = 'queued'",
    )
    .bind(msg_id)
    .execute(db)
    .await
    {
        tracing::error!(msg = %msg_id, error = %e, "failed to close out the support-request operator alert row");
    }
    Err(outcome
        .error
        .unwrap_or_else(|| "the provider refused the operator alert".to_string()))
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
    fn the_seeded_welcome_template_body_gains_the_support_contact_page() {
        // Regression guard for the whole class (t_71cc3ad8, t_17277fd5): the transactional body
        // that reaches the message must name a way to reach the operator, whichever body was
        // chosen — and it must be a PAGE, because every address this app has printed bounced.
        let (text, html) = with_support_footer(SEEDED_BODY, SEEDED_HTML);
        assert!(
            text.contains(SUPPORT_CONTACT),
            "text body lacks the support contact page"
        );
        assert!(
            html.contains(SUPPORT_CONTACT),
            "html body lacks the support contact page"
        );
    }

    #[test]
    fn support_footer_is_idempotent_and_never_added_to_an_empty_body() {
        let (t, h) = with_support_footer("Body", "<p>Body</p>");
        assert!(t.ends_with(&format!("Need help? {}\n", SUPPORT_CONTACT)));
        // the HTML arm links the page, rather than pasting it as bare text
        assert!(h.contains(&format!("<a href=\"{}\">", SUPPORT_CONTACT)));
        // a body that already carries the page is returned untouched
        assert_eq!(with_support_footer(&t, &h), (t.clone(), h.clone()));
        // "" means "no body part", not "footer only"
        assert_eq!(with_support_footer("", ""), (String::new(), String::new()));
    }

    #[test]
    fn support_footer_names_no_mailbox_that_bounces() {
        // The defect this card closes (kanban t_17277fd5): the footer named
        // `support@swiftsoftware.net`, whose MX was removed, so the customer who wrote to the one
        // address its welcome mail gave them bounced. Neither of this app's own domains publishes a
        // mail exchanger, so the footer must name a served page and carry no `@` at all.
        assert_eq!(SUPPORT_CONTACT, "https://app.coreswiftcrm.com/contact.html");
        assert!(
            !SUPPORT_CONTACT.contains('@'),
            "a mailbox here is a bounce: no address on this product's domains can receive"
        );
        assert!(SUPPORT_CONTACT.starts_with(APP_URL));
    }
}

/// The app's OWN wordmark, used for the header of every message sent for a workspace that has
/// configured no email branding of its own (kanban t_5054a5c9).
///
/// A mail client fetches it over the network with no session, so it is an absolute URL on a host
/// that answers publicly — and it is a PNG, because Gmail and Outlook do not render the SVG the
/// pages use. `alt` carries the name, so a client that blocks images still says whose mail this is.
const APP_LOGO_PATH: &str = "/logo.png";

/// The identity every message falls back to when its workspace has configured none: the app's own
/// name and wordmark, painted in the fleet accent (`branding::DEFAULT_BRAND_COLOR`).
///
/// Before this the header block only appeared once a workspace had uploaded a logo of its own, so
/// the credentials mail a BRAND-NEW customer receives — the first thing this product ever shows
/// them — arrived with no wordmark at all (measured live 2026-10-24 on the pre-fix container, from
/// the app's real send path: no header div, no `<img>`, no colour rule on the captured message,
/// while it carried a full HTML part the header could have lived in). A workspace's own branding
/// still wins field by field, because `apply_branding` is handed THIS only when
/// `branding::load` found nothing configured.
fn app_default_branding() -> crate::branding::Branding {
    crate::branding::Branding {
        brand_name: APP_NAME.to_string(),
        brand_color: String::new(),
        logo_url: APP_LOGO_PATH.to_string(),
    }
}

/// Prepend the workspace's email-branding header to a body about to be queued (kanban t_feab8aff).
///
/// Returns `(text_body, html_body, is_html)`. `branding` is `Option` so this function stays a pure,
/// testable statement of the render rule: `None` changes NOTHING (the additivity the port pinned —
/// the inputs come back byte-identical), and every SEND resolves to `Some` because
/// [`queue_outbound_message`] passes the workspace's own branding or [`app_default_branding`].
///
/// `{{brand_name}}`/`{{logo_url}}` are bound HERE because they are per-account and cannot live in
/// a `&'static str` default map. If the message has no HTML part at all it STAYS that way: this app
/// queues ONE body and the transport sends it as the text part, so wrapping text in HTML here would
/// deliver markup as the plain-text part — a regression, not branding.
fn apply_branding(
    branding: Option<&crate::branding::Branding>,
    text_body: &str,
    html_body: &str,
    is_html: bool,
) -> (String, String, bool) {
    let Some(brand) = branding else {
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
mod branding_header_tests {
    use super::{app_default_branding, apply_branding, APP_LOGO_PATH, APP_NAME, APP_URL};
    use crate::branding::{Branding, DEFAULT_BRAND_COLOR};

    /// The defect this card closes (kanban t_5054a5c9): a workspace with no branding of its own
    /// used to send mail with NO header at all, so the very first message a new customer receives —
    /// the credentials mail — carried no wordmark (measured live on the pre-fix container: no
    /// header div, no `<img>`, no colour rule, on a message that DID carry an HTML part).
    #[test]
    fn an_unbranded_workspace_gets_the_apps_own_wordmark() {
        let b = app_default_branding();
        assert_eq!(b.brand_name, APP_NAME);
        assert_eq!(b.logo_url, APP_LOGO_PATH);
        let url = b
            .resolve_logo_url(APP_URL)
            .expect("the app wordmark resolves");
        // A mail client resolves nothing relative: the PNG must be absolute, on the app's own host.
        assert_eq!(url, "https://app.coreswiftcrm.com/logo.png");
        let header = b.header_html(Some(&url));
        assert!(header.contains("<img"), "{header}");
        assert!(header.contains("alt=\"CoreSwift CRM\""), "{header}");
        assert!(
            header.contains(&format!("border-top:3px solid {DEFAULT_BRAND_COLOR}")),
            "the app's own header is painted in the fleet accent: {header}"
        );
    }

    /// An HTML message opens with the header block, and its text part opens with the app's name.
    #[test]
    fn an_html_send_carries_the_header_and_the_apps_name() {
        let b = app_default_branding();
        let (text, html, is_html) = apply_branding(Some(&b), "Hi there", "<p>Hi there</p>", true);
        assert!(text.starts_with("CoreSwift CRM\n\n"), "{text}");
        assert!(
            html.starts_with("<div style=\"text-align:center;padding:18px 0 6px\""),
            "{html}"
        );
        assert!(html.contains("<img"), "{html}");
        assert!(
            html.ends_with("<p>Hi there</p>"),
            "the body survives under the header: {html}"
        );
        assert!(is_html);
    }

    /// THE ONE-PART RULE: this app queues a single body and the transport sends it as the text part,
    /// so a text-only message must NOT be wrapped in HTML to give the logo somewhere to render.
    #[test]
    fn a_text_only_send_stays_text_only() {
        let b = app_default_branding();
        let (text, html, is_html) = apply_branding(Some(&b), "Hi there", "", false);
        assert!(text.starts_with("CoreSwift CRM\n\n"), "{text}");
        assert_eq!(html, "", "no HTML part may be invented");
        assert!(!is_html);
    }

    /// A workspace's own branding wins field by field — name, colour and logo — over the app default.
    #[test]
    fn the_workspaces_own_branding_wins_field_by_field() {
        let own = Branding {
            brand_name: "Acme Plumbing".into(),
            brand_color: "#0ea5e9".into(),
            logo_url: "/api/branding/logo/abc?v=7".into(),
        };
        let (text, html, _) = apply_branding(Some(&own), "Hi there", "<p>Hi there</p>", true);
        assert!(text.starts_with("Acme Plumbing\n\n"), "{text}");
        assert!(html.contains("border-top:3px solid #0ea5e9"), "{html}");
        assert!(
            html.contains("https://app.coreswiftcrm.com/api/branding/logo/abc?v=7"),
            "{html}"
        );
        assert!(
            !html.contains("/logo.png"),
            "the app wordmark must not appear: {html}"
        );
    }

    /// `None` stays a no-op — the additivity the original port pinned. Every SEND resolves to
    /// `Some` (the workspace's own branding or the app's identity), so this arm is only reachable
    /// from a test now.
    #[test]
    fn none_still_changes_nothing() {
        let (text, html, is_html) = apply_branding(None, "Hi there", "<p>Hi there</p>", true);
        assert_eq!(text, "Hi there");
        assert_eq!(html, "<p>Hi there</p>");
        assert!(is_html);
        let (text, html, is_html) = apply_branding(None, "Hi there", "", false);
        assert_eq!(
            (text.as_str(), html.as_str(), is_html),
            ("Hi there", "", false)
        );
    }

    /// The binding half: an admin-authored `{{brand_name}}`/`{{logo_url}}` is substituted with the
    /// resolved identity rather than reaching a recipient as literal text.
    #[test]
    fn the_merge_fields_are_bound_on_the_fallback_arm() {
        let b = app_default_branding();
        let (_, html, _) = apply_branding(
            Some(&b),
            "",
            "<p>{{brand_name}} &mdash; {{logo_url}}</p>",
            true,
        );
        assert!(html.contains("CoreSwift CRM"), "{html}");
        assert!(
            html.contains("https://app.coreswiftcrm.com/logo.png"),
            "{html}"
        );
        assert!(!html.contains("{{"), "{html}");
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

#[cfg(test)]
mod support_alert_tests {
    use super::support_alert_bodies;

    /// The asker is a STRANGER writing to a public form: nothing they type may become markup in
    /// the operator's inbox, while the text part still reads exactly as they wrote it.
    #[test]
    fn the_askers_words_are_escaped_in_the_html_part_only() {
        let message = "Hi <script>alert('x')</script> & \"quoted\"";
        let (text, html) = support_alert_bodies(
            "Probe <b>",
            "probe@example.invalid",
            "Cannot <sign> in",
            message,
        );
        assert!(
            text.contains("<script>"),
            "the text part keeps the words as typed: {text}"
        );
        assert!(
            !html.contains("<script>"),
            "no injected tag in the html part: {html}"
        );
        assert!(html.contains("&lt;script&gt;"), "{html}");
        assert!(html.contains("&amp;"), "{html}");
        assert!(html.contains("&#39;"), "{html}");
        assert!(!html.contains("Probe <b>"), "{html}");
    }
}
