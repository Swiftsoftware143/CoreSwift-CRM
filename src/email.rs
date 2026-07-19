//! Email module — sends transactional emails using database-stored templates.
//!
//! Templates are stored in `email_templates` and support {{variable}} replacement.
//! Falls back to inline hardcoded templates when DB template not found.
//!
//! All emails are queued via `outbound_messages` table for async delivery by the worker.

use sqlx::PgPool;
use uuid::Uuid;

use crate::AppState;

/// Get available merge fields for a given template type.
/// Returns a list of field names that can be used in templates.
pub fn get_merge_fields(template_type: &str) -> Vec<&'static str> {
    match template_type {
        "welcome" => vec!["name", "email", "password", "app_url"],
        "purchase_confirmed" => vec!["name", "plan_name", "app_url"],
        "password_reset" => vec!["name", "token", "app_url"],
        _ => vec!["name", "email", "password", "app_url", "plan_name", "token", "account_name"],
    }
}

/// Render a template string by replacing {{key}} placeholders with values from `vars`.
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

/// Send a templated email using stored email_templates.
///
/// This queues an `outbound_messages` row for async delivery.
/// - Looks up template by template_type (account-scoped or default)
/// - Falls back to hardcoded inline content if no DB template found
/// - Renders {{variable}} placeholders from `vars`
pub async fn send_template_email(
    db: &PgPool,
    tenant_id: Uuid,
    to: &str,
    template_type: &str,
    vars: &serde_json::Value,
) -> Result<(), String> {
    let app_name = "CoreSwift CRM";
    let app_url = "https://app.coreswiftcrm.com";

    // Try to load template from DB — prefer tenant-specific, fallback to default
    let template = sqlx::query_as::<_, EmailTemplateRow>(
        r#"SELECT id, name, subject, body, html_body, is_html, is_default
           FROM email_templates
           WHERE template_type = $1 AND (aid = $2 OR is_default = true)
           ORDER BY is_default ASC, created_at DESC
           LIMIT 1"#
    )
    .bind(template_type)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();

    match template {
        Some(t) => {
            // Use DB template
            let subject = render_template(
                &t.subject.unwrap_or_else(|| get_default_subject(template_type, app_name)),
                vars,
            );
            let html_body = t.html_body.as_ref()
                .map(|h| render_template(h, vars))
                .unwrap_or_default();
            let text_body = render_template(&t.body.unwrap_or_default(), vars);
            let use_html = t.is_html.unwrap_or(true);

            queue_outbound_message(
                db,
                tenant_id,
                to,
                &subject,
                &text_body,
                &html_body,
                use_html,
            )
            .await
        }
        None => {
            // Fallback to hardcoded inline templates
            send_inline(db, tenant_id, to, template_type, vars, app_name, app_url).await
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
    // Build the body: use html if available and is_html, otherwise text
    let body = if is_html && !html_body.is_empty() {
        html_body.to_string()
    } else {
        text_body.to_string()
    };

    sqlx::query(
        r#"INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status)
           VALUES ($1, $2, 'email', $3, $4, $5, 'queued')"#
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(to)
    .bind(subject)
    .bind(&body)
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
    let plan_name_val = vars.get("plan_name").and_then(|v| v.as_str()).unwrap_or("a plan");

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
            let body = format!("{} Notification:\n\n{}", app_name, vars.to_string());
            queue_outbound_message(db, tenant_id, to, &subject, &body, "", false).await
        }
    }
}

// ---- Data types ----

#[derive(Debug, sqlx::FromRow)]
struct EmailTemplateRow {
    id: Uuid,
    name: String,
    subject: Option<String>,
    body: Option<String>,
    html_body: Option<String>,
    is_html: Option<bool>,
    is_default: Option<bool>,
}
