//! Support Widgets — multi-widget embeddable support forms.
//!
//! Each widget is named (e.g. "CoreSwift CRM", "FunnelSwift") and routes
//! submissions to a named inbox. Plan-gated twice, both from the module registry:
//! the `support_widgets` module gates the routes, and the `limit_max_widgets`
//! module feature is the ceiling on how many widgets an account may create.
//!
//! Public routes (NO auth) — mounted at the ROOT by `public_router`, which main.rs `merge`s into
//! the app (kanban t_f6bb2958). The `/s/...` spelling is the ONE canonical public surface for an
//! embed: it is what `POST /api/widgets` advertises to the operator, it is what both nginx vhosts
//! proxy to this app (`location /s/` in sites/coreswift.conf, on the marketing host and the app
//! host), and it is the same `location` the per-tenant ticket widget (`/s/:tenant_id/widget.js`)
//! and the 💬 My Support portal already use. `/s/**` is a SERVED surface, outside the credential
//! boundary (src/auth/route_policy.rs `is_guarded_path`), so nothing here is an allowlist entry:
//! - GET  /s/:tenant_slug/widgets/:widget_slug/embed.js — embeddable JS widget
//! - POST /s/:tenant_slug/widgets/:widget_slug/submit   — public form submit
//!
//! Nothing under `/api/widgets/**` answers an anonymous caller: the pre-t_f6bb2958 shape nested
//! this router at `/api/widgets` and hung the two public arms off it, so they were reachable only
//! at the DOUBLED `/api/widgets/widgets/...` and every real embed URL 404'd.
//!
//! Auth routes (all under the `/api/widgets` nest):
//! - GET    /api/widgets          — list my widgets
//! - POST   /api/widgets          — create widget
//! - PATCH  /api/widgets/:id      — update widget
//! - DELETE /api/widgets/:id      — delete widget
//! - GET    /api/widgets/:id/embed — get embed code
//! - GET    /api/widgets/inboxes  — list inboxes
//! - POST   /api/widgets/inboxes  — create inbox
//! - PATCH  /api/widgets/inboxes/:id — update inbox
//! - DELETE /api/widgets/inboxes/:id — delete inbox

use axum::{
    extract::{Extension, Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, patch, post},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The public host every widget embed points at (kanban t_f6bb2958).
///
/// ONE spelling for the public surface: the marketing vhost on this domain proxies `/s/` to this
/// app's port (`location /s/` in /opt/swift/nginx/sites/coreswift.conf — the same location the
/// per-tenant ticket widget `/s/:tenant_id/widget.js` and the 💬 My Support portal use), so an
/// operator pasting the advertised snippet on their own site loads the script and the visitor's
/// form post comes back to the app over a path that is actually served.
pub const PUBLIC_WIDGET_BASE: &str = "https://coreswiftcrm.com";

/// The canonical public path of one widget's embed script / submit arm. The JS is served from the
/// first; the script posts the visitor's form to the second on the host that served it.
pub fn widget_public_path(tenant_slug: &str, widget_slug: &str) -> String {
    format!("/s/{}/widgets/{}", tenant_slug, widget_slug)
}

/// Escape a value for a JS SINGLE-QUOTED string literal.
///
/// Everything the embed script interpolates goes through this. The widget's greeting and welcome
/// message are OPERATOR-AUTHORED text, and the DEFAULT welcome message
/// ("Thanks for reaching out! We'll get back to you shortly.") contains an apostrophe — which used
/// to close the JS string literal and kill the entire script with
/// `Uncaught SyntaxError: Unexpected identifier 'll'` before a single element was rendered, so the
/// widget never appeared on ANY page, for ANY widget (kanban t_f6bb2958, measured in Chromium).
fn js_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out
}

/// Escape a value for HTML TEXT or ATTRIBUTE context — the panel markup the script builds with
/// `innerHTML` (the greeting sits both in the header text and in the message placeholder).
fn html_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A CSS colour fit for the widget's own `<style>` element. Keeping only what a colour can contain
/// means an operator typo (or a pasted stylesheet) cannot inject CSS into the CUSTOMER's page
/// through the embed. An empty result falls back to the platform default.
fn css_color(s: &str) -> String {
    let kept: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "#(),.% -/".contains(*c))
        .collect();
    let kept = kept.trim();
    if kept.is_empty() {
        "#2563eb".to_string()
    } else {
        kept.to_string()
    }
}

// ── Models ──────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct SupportWidget {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub inbox_id: Uuid,
    pub name: String,
    pub slug: String,
    pub theme_color: String,
    pub greeting: String,
    pub welcome_msg: String,
    pub position: String,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, Deserialize, sqlx::FromRow)]
pub struct TicketInbox {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub email_fwd: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// True when THIS app minted the inbox for a widget (POST /api/widgets with no `inbox_id`),
    /// false when the operator created it by hand through POST /api/widgets/inboxes. Only an
    /// app-minted inbox may be retired when the widget that needed it goes away (t_a6784da5).
    pub auto_created: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateWidgetRequest {
    pub name: String,
    pub inbox_id: Option<Uuid>,
    pub theme_color: Option<String>,
    pub greeting: Option<String>,
    pub welcome_msg: Option<String>,
    pub position: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWidgetRequest {
    pub name: Option<String>,
    pub inbox_id: Option<Uuid>,
    pub theme_color: Option<String>,
    pub greeting: Option<String>,
    pub welcome_msg: Option<String>,
    pub position: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInboxRequest {
    pub name: String,
    pub email_fwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateInboxRequest {
    pub name: Option<String>,
    pub email_fwd: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WidgetSubmitRequest {
    pub name: String,
    pub email: String,
    pub subject: String,
    pub message: String,
}

// ── Widget CRUD ─────────────────────────────────────────

/// GET /api/widgets — list my widgets
pub async fn list_widgets(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let widgets = sqlx::query_as::<_, SupportWidget>(
        "SELECT * FROM support_widgets WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "widgets": widgets })))
}

/// POST /api/widgets — create a new widget
pub async fn create_widget(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<CreateWidgetRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Plan gate: the widget CEILING comes from the module registry — the very row the admin's
    // Features & Plans panel edits (`limit_max_widgets`, module `limits`). It used to read the
    // legacy `plans.features->>'max_widgets'` JSONB column directly, the last hardcoded entitlement
    // read left in this app, which meant the panel could change that number and this gate would not
    // move — and a plan with no `max_widgets` key read as UNLIMITED rather than as unset
    // (kanban t_6d09f02c). Semantics are the house ones (src/module_registry::resolve +
    // industries::handlers::industry_limit): `enabled = false` -> 0 = "not available on your plan",
    // a negative assignment is the documented unlimited sentinel, and an enabled feature with no
    // number has no ceiling. A tenant with no active plan keeps its modules (source = "no_plan").
    let ent = crate::module_registry::resolve(&s.db, tid, "limit_max_widgets").await?;
    let ceiling: Option<i64> = if !ent.enabled {
        Some(0)
    } else {
        match ent.limit_value {
            None => None,
            Some(v) if v < 0.0 => None,
            Some(v) => Some(v.floor() as i64),
        }
    };

    if let Some(limit) = ceiling {
        let current: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM support_widgets WHERE tenant_id = $1")
                .bind(tid)
                .fetch_one(&s.db)
                .await?;
        if current >= limit {
            // 402, like every other plan denial in this app: a ceiling is an upsell, not a
            // malformed request.
            return Err(AppError::UpgradeRequired(format!(
                "Widget limit reached ({}/{}). Upgrade your plan for more widgets.",
                current, limit
            )));
        }
    }

    // Auto-create inbox if none provided
    let inbox_id = if let Some(iid) = req.inbox_id {
        iid
    } else {
        let iid = Uuid::new_v4();
        // Create a default inbox named after the widget. `auto_created` records that WE minted it,
        // so DELETE /api/widgets/:id may retire it when the widget goes (card t_a6784da5); an inbox
        // the operator made by hand through POST /api/widgets/inboxes is never touched.
        sqlx::query(
            "INSERT INTO ticket_inboxes (id, tenant_id, name, auto_created) VALUES ($1, $2, $3, true)",
        )
            .bind(iid)
            .bind(tid)
            .bind(format!("{} Inbox", &req.name))
            .execute(&s.db)
            .await?;
        iid
    };

    // Generate slug from name
    let slug = req
        .name
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '-', "-")
        .trim_matches('-')
        .to_string();
    let slug = if slug.is_empty() {
        "widget".to_string()
    } else {
        slug
    };

    let id = Uuid::new_v4();
    let widget = sqlx::query_as::<_, SupportWidget>(
        r#"INSERT INTO support_widgets (id, tenant_id, inbox_id, name, slug, theme_color, greeting, welcome_msg, position)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING *"#,
    )
    .bind(id)
    .bind(tid)
    .bind(inbox_id)
    .bind(&req.name)
    .bind(&slug)
    .bind(req.theme_color.as_deref().unwrap_or("#2563eb"))
    .bind(req.greeting.as_deref().unwrap_or("How can we help?"))
    .bind(req.welcome_msg.as_deref().unwrap_or("Thanks for reaching out! We'll get back to you shortly."))
    .bind(req.position.as_deref().unwrap_or("bottom-right"))
    .fetch_one(&s.db)
    .await?;

    // Get tenant slug for embed URL
    let tenant_slug: String = sqlx::query_scalar("SELECT slug FROM tenants WHERE id = $1")
        .bind(tid)
        .fetch_one(&s.db)
        .await?;

    // The operator pastes this on their own site. Both the snippet and the bare URL are the
    // CANONICAL public surface (/s/...), mounted by `public_router` and proxied by nginx — the
    // pre-t_f6bb2958 code advertised this path while serving it under the doubled
    // /api/widgets/widgets/... nest, so every advertised embed URL answered 404 live.
    let public_path = widget_public_path(&tenant_slug, &slug);
    Ok(Json(json!({
        "widget": widget,
        "embed_code": format!("<script src=\"{}{}/embed.js\" defer></script>", PUBLIC_WIDGET_BASE, public_path),
        "embed_url": format!("{}{}/embed.js", PUBLIC_WIDGET_BASE, public_path),
        "submit_url": format!("{}{}/submit", PUBLIC_WIDGET_BASE, public_path),
    })))
}

/// PATCH /api/widgets/:id — update widget
pub async fn update_widget(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateWidgetRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, SupportWidget>(
        "SELECT * FROM support_widgets WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Widget not found".into()))?;

    let widget = sqlx::query_as::<_, SupportWidget>(
        r#"UPDATE support_widgets SET
           name = $1, inbox_id = $2, theme_color = $3, greeting = $4,
           welcome_msg = $5, position = $6, is_active = $7
           WHERE id = $8 AND tenant_id = $9 RETURNING *"#,
    )
    .bind(req.name.as_deref().unwrap_or(&existing.name))
    .bind(req.inbox_id.unwrap_or(existing.inbox_id))
    .bind(req.theme_color.as_deref().unwrap_or(&existing.theme_color))
    .bind(req.greeting.as_deref().unwrap_or(&existing.greeting))
    .bind(req.welcome_msg.as_deref().unwrap_or(&existing.welcome_msg))
    .bind(req.position.as_deref().unwrap_or(&existing.position))
    .bind(req.is_active.unwrap_or(existing.is_active))
    .bind(id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;

    // Repointing a widget away from the inbox this app minted for it leaks the very same orphan
    // the widget delete used to leave behind (card t_a6784da5), so retire the old one here too.
    if widget.inbox_id != existing.inbox_id {
        retire_auto_inbox(&s, tid, existing.inbox_id, 0).await?;
    }

    Ok(Json(json!({ "widget": widget })))
}

/// Retire an inbox this app minted for a widget, once nothing needs it any more.
///
/// Card t_a6784da5: POST /api/widgets mints a "<widget name> Inbox" row and points the widget at
/// it, while the widget delete used to remove the widget row only, leaving that inbox in the
/// operator's own Inboxes list (GET /api/widgets/inboxes) forever, named after a widget that no
/// longer exists. Retiring it is safe only when all three hold:
///   * `ticket_inboxes.auto_created` -- the APP minted it for a widget; an inbox the operator
///     created through POST /api/widgets/inboxes is never touched;
///   * no widget in the tenant points at it any more (widgets may share one inbox);
///   * it carries no tickets. `carried_tickets` is counted by the caller BEFORE it drops the widget
///     row, because `tickets.widget_id` is ON DELETE SET NULL and afterwards nothing can say which
///     inbox a ticket arrived through: a non-empty inbox is KEPT as the tenant's support history.
///
/// Returns the retired inbox's name, or `None` when the inbox was kept.
async fn retire_auto_inbox(
    s: &AppState,
    tid: Uuid,
    inbox_id: Uuid,
    carried_tickets: i64,
) -> ApiResult<Option<String>> {
    if carried_tickets > 0 {
        return Ok(None);
    }

    let found = sqlx::query_as::<_, TicketInbox>(
        "SELECT * FROM ticket_inboxes WHERE id = $1 AND tenant_id = $2",
    )
    .bind(inbox_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    let inbox = match found {
        Some(inbox) => inbox,
        None => return Ok(None),
    };
    if !inbox.auto_created {
        return Ok(None);
    }

    let still_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM support_widgets WHERE tenant_id = $1 AND inbox_id = $2",
    )
    .bind(tid)
    .bind(inbox_id)
    .fetch_one(&s.db)
    .await?;
    if still_used > 0 {
        return Ok(None);
    }

    sqlx::query("DELETE FROM ticket_inboxes WHERE id = $1 AND tenant_id = $2")
        .bind(inbox_id)
        .bind(tid)
        .execute(&s.db)
        .await?;

    Ok(Some(inbox.name))
}

/// DELETE /api/widgets/:id -- delete widget, and the inbox it minted when that inbox would
/// otherwise be left behind as clutter in the operator's own Inboxes list (card t_a6784da5;
/// see `retire_auto_inbox` for the three conditions).
pub async fn delete_widget(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, SupportWidget>(
        "SELECT * FROM support_widgets WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Widget not found".into()))?;

    // Count this widget's tickets BEFORE it goes: `tickets.widget_id` is ON DELETE SET NULL, so
    // afterwards nothing can tell which inbox the customer wrote through.
    let carried_tickets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM tickets WHERE widget_id = $1")
            .bind(id)
            .fetch_one(&s.db)
            .await?;

    sqlx::query("DELETE FROM support_widgets WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;

    let retired = retire_auto_inbox(&s, tid, existing.inbox_id, carried_tickets).await?;

    Ok(Json(json!({
        "deleted": true,
        "inbox_deleted": retired.is_some(),
        "inbox_name": retired,
    })))
}

// ── Inboxes CRUD ────────────────────────────────────────

/// GET /api/widgets/inboxes — list my inboxes
pub async fn list_inboxes(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let inboxes = sqlx::query_as::<_, TicketInbox>(
        "SELECT * FROM ticket_inboxes WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({ "inboxes": inboxes })))
}

/// POST /api/widgets/inboxes — create inbox
pub async fn create_inbox(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<CreateInboxRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let id = Uuid::new_v4();
    let inbox = sqlx::query_as::<_, TicketInbox>(
        r#"INSERT INTO ticket_inboxes (id, tenant_id, name, email_fwd) VALUES ($1, $2, $3, $4) RETURNING *"#,
    )
    .bind(id)
    .bind(tid)
    .bind(&req.name)
    .bind(&req.email_fwd)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!({ "inbox": inbox })))
}

/// PATCH /api/widgets/inboxes/:id — update inbox
pub async fn update_inbox(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateInboxRequest>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let existing = sqlx::query_as::<_, TicketInbox>(
        "SELECT * FROM ticket_inboxes WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Inbox not found".into()))?;

    let inbox = sqlx::query_as::<_, TicketInbox>(
        "UPDATE ticket_inboxes SET name = $1, email_fwd = $2 WHERE id = $3 AND tenant_id = $4 RETURNING *",
    )
    .bind(req.name.as_deref().unwrap_or(&existing.name))
    .bind(req.email_fwd.as_deref().unwrap_or(existing.email_fwd.as_deref().unwrap_or("")))
    .bind(id)
    .bind(tid)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!({ "inbox": inbox })))
}

/// DELETE /api/widgets/inboxes/:id — delete inbox
pub async fn delete_inbox(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM ticket_inboxes WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Inbox not found".into()));
    }

    Ok(Json(json!({ "deleted": true })))
}

// ── Public embed / submit ───────────────────────────────

/// GET /api/widgets/:tenant_slug/:widget_slug/embed.js — widget embed script
pub async fn widget_embed_js(
    State(s): State<AppState>,
    Path((tenant_slug, widget_slug)): Path<(String, String)>,
) -> ApiResult<impl IntoResponse> {
    let widget = sqlx::query_as::<_, SupportWidget>(
        r#"SELECT w.* FROM support_widgets w
           JOIN tenants t ON t.id = w.tenant_id
           WHERE t.slug = $1 AND w.slug = $2 AND w.is_active = true"#,
    )
    .bind(&tenant_slug)
    .bind(&widget_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Widget not found".into()))?;

    // The visitor's post has to come back to THIS app from whatever page the snippet is pasted
    // into — including a customer's own site on a different origin. So the script derives its own
    // origin from the <script> element that loaded it (the snippet carries `defer`, so
    // `document.currentScript` is null by then and the element is found in the DOM instead) and
    // falls back to the canonical public host. It is ABSOLUTE either way: the pre-t_f6bb2958 script
    // posted a bare `/api/widgets/{tenant}/{widget}/submit`, which resolved against the CUSTOMER's
    // host, so from a real site the POST never reached CoreSwift-CRM at all — and had it somehow
    // arrived, that single-prefix path is not the mounted one (it is refused 401).
    let canonical_path = widget_public_path(&tenant_slug, &widget_slug);
    let js_endpoint_path = format!("{}/submit", canonical_path);
    let js_endpoint_absolute = format!("{}{}", PUBLIC_WIDGET_BASE, js_endpoint_path);
    let js_embed_marker = format!("/{}/embed.js", widget_slug);
    let js = format!(
        r#"(function(){{
    var d=document;
    if(d.getElementById('coreswift-widget-{}'))return;
    var endpoint='{}';
    try{{var sc=d.currentScript;if(!sc){{var ss=d.getElementsByTagName('script');for(var i=ss.length-1;i>=0;i--){{if(ss[i].src&&ss[i].src.indexOf('{}')>=0){{sc=ss[i];break}}}}}}if(sc&&sc.src){{var u=new URL(sc.src);endpoint=u.protocol+'//'+u.host+'{}'}}}}catch(e){{}}
    var s=d.createElement('style');
    s.textContent='.csw-btn{{position:fixed;{}:20px;bottom:20px;width:56px;height:56px;border-radius:50%;background:{};color:#fff;border:none;cursor:pointer;box-shadow:0 4px 16px rgba(0,0,0,.25);font-size:24px;z-index:9999;display:flex;align-items:center;justify-content:center;transition:transform .2s}}.csw-btn:hover{{transform:scale(1.1)}}.csw-panel{{display:none;position:fixed;{}:20px;bottom:88px;width:340px;max-height:70vh;background:#fff;border-radius:16px;box-shadow:0 8px 32px rgba(0,0,0,.2);z-index:9998;overflow:hidden;font-family:-apple-system,system-ui,sans-serif}}.csw-panel.open{{display:flex;flex-direction:column}}.csw-panel-header{{background:{};color:#fff;padding:16px 20px;font-weight:700;font-size:15px}}.csw-panel-body{{padding:16px;overflow-y:auto;flex:1}}.csw-panel-body input,.csw-panel-body textarea{{width:100%;padding:10px 12px;margin-bottom:10px;border:1px solid #e2e8f0;border-radius:10px;font-size:14px;font-family:inherit;outline:0}}.csw-panel-body input:focus,.csw-panel-body textarea:focus{{border-color:{};box-shadow:0 0 0 3px {}22}}.csw-panel-body textarea{{resize:none;height:100px}}.csw-submit{{width:100%;padding:12px;background:{};color:#fff;border:none;border-radius:10px;font-size:14px;font-weight:600;cursor:pointer}}.csw-submit:hover{{filter:brightness(1.1)}}.csw-msg{{padding:10px;border-radius:8px;font-size:13px;margin-top:10px;display:none}}';
    d.head.appendChild(s);
    var btn=d.createElement('button');
    btn.className='csw-btn';btn.id='coreswift-widget-{}';btn.textContent='💬';
    d.body.appendChild(btn);
    var panel=d.createElement('div');
    panel.className='csw-panel';panel.id='csw-panel-{}';
    panel.innerHTML='<div class="csw-panel-header">{}&nbsp;&nbsp;<span style="cursor:pointer;float:right" onclick="document.getElementById(\'csw-panel-{}\').classList.remove(\'open\')">✕</span></div><div class="csw-panel-body"><input type="text" id="csw-name" placeholder="Your name"><input type="email" id="csw-email" placeholder="Your email"><input type="text" id="csw-subject" placeholder="Subject"><textarea id="csw-message" placeholder="{}"></textarea><button class="csw-submit" onclick="coreswiftWidgetSubmit(\'csw-panel-{}\')">Send Message</button><div class="csw-msg" id="csw-msg-{}"></div></div>';
    d.body.appendChild(panel);
    btn.onclick=function(){{var p=d.getElementById('csw-panel-{}');p.classList.toggle('open')}};
    window.coreswiftWidgetSubmit=function(pid){{
        var p=d.getElementById(pid),name=d.getElementById('csw-name').value.trim(),email=d.getElementById('csw-email').value.trim(),subject=d.getElementById('csw-subject').value.trim(),msg=d.getElementById('csw-message').value.trim(),el=d.getElementById('csw-msg-{}');
        if(!name||!email||!subject||!msg){{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent='Please fill in all fields';return}}
        el.style.display='none';
        fetch(endpoint,{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{name:name,email:email,subject:subject,message:msg}})}})
        .then(function(r){{return r.json()}})
        .then(function(res){{
            if(res.success){{el.style.display='block';el.style.background='#f0fdf4';el.style.color='#166534';el.textContent='{}';d.getElementById('csw-message').value='';d.getElementById('csw-subject').value=''}}
            else{{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent=res.message||'Something went wrong'}}
        }}).catch(function(){{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent='Network error. Try again.'}});
    }};
}})();"#,
        js_str(&widget.slug),
        js_endpoint_absolute,
        js_embed_marker,
        js_endpoint_path,
        if widget.position == "bottom-left" {
            "left"
        } else {
            "right"
        },
        js_str(&css_color(&widget.theme_color)),
        if widget.position == "bottom-left" {
            "left"
        } else {
            "right"
        },
        js_str(&css_color(&widget.theme_color)),
        js_str(&css_color(&widget.theme_color)),
        js_str(&css_color(&widget.theme_color)),
        js_str(&css_color(&widget.theme_color)),
        js_str(&widget.slug),
        js_str(&widget.slug),
        js_str(&html_text(&widget.greeting)),
        js_str(&widget.slug),
        js_str(&html_text(&widget.greeting)),
        js_str(&widget.slug),
        js_str(&widget.slug),
        js_str(&widget.slug),
        js_str(&widget.slug),
        js_str(&widget.welcome_msg),
    );

    Ok((
        StatusCode::OK,
        [("content-type", "application/javascript")],
        js,
    ))
}

/// POST /api/widgets/:tenant_slug/:widget_slug/submit — public ticket submission
pub async fn widget_submit(
    State(s): State<AppState>,
    Path((tenant_slug, widget_slug)): Path<(String, String)>,
    Json(req): Json<WidgetSubmitRequest>,
) -> ApiResult<impl IntoResponse> {
    let (tenant_id, widget_id, _inbox_id): (Uuid, Uuid, Uuid) = sqlx::query_as(
        r#"SELECT w.tenant_id, w.id, w.inbox_id FROM support_widgets w
           JOIN tenants t ON t.id = w.tenant_id
           WHERE t.slug = $1 AND w.slug = $2 AND w.is_active = true"#,
    )
    .bind(&tenant_slug)
    .bind(&widget_slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Widget not found".into()))?;

    let ticket_id = Uuid::new_v4();
    sqlx::query(
        r#"INSERT INTO tickets (id, tenant_id, widget_id, subject, description, status, priority, source, contact_name, contact_email)
           VALUES ($1, $2, $3, $4, $5, 'open', 'medium', 'form', $6, $7)"#,
    )
    .bind(ticket_id)
    .bind(tenant_id)
    .bind(widget_id)
    .bind(&req.subject)
    .bind(&req.message)
    .bind(&req.name)
    .bind(&req.email)
    .execute(&s.db)
    .await?;

    // Add first message
    sqlx::query(
        "INSERT INTO ticket_messages (id, ticket_id, sender_type, message) VALUES ($1, $2, 'contact', $3)",
    )
    .bind(Uuid::new_v4())
    .bind(ticket_id)
    .bind(&req.message)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({
        "success": true,
        "ticket_id": ticket_id.to_string(),
        "message": "Ticket created successfully"
    })))
}

// ── Router ──────────────────────────────────────────────

/// The AUTHENTICATED router, mounted by main.rs at `/api/widgets` (kanban t_f6bb2958).
///
/// It holds NO public arm: hanging `get(widget_embed_js)`/`post(widget_submit)` off this nest is
/// what made the only reachable public spelling `/api/widgets/widgets/...` (doubled) while the
/// advertised `/s/...` URL 404'd. The public pair lives in [`public_router`] and is mounted at the
/// ROOT. Everything here is behind the module's auth + plan gate.
pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::middleware;

    axum::Router::new()
        .route("/", get(list_widgets).post(create_widget))
        .route("/:id", patch(update_widget).delete(delete_widget))
        .route("/inboxes", get(list_inboxes).post(create_inbox))
        .route("/inboxes/:id", patch(update_inbox).delete(delete_inbox))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "support_widgets",
                "Support widgets",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// The ONE public widget surface, mounted at the ROOT of the app (main.rs `merge`s it next to the
/// other `/s/...` support routes, which is what both nginx vhosts proxy — kanban t_f6bb2958).
///
/// `/s/:tenant_slug/widgets/:widget_slug/embed.js` is what `POST /api/widgets` advertises in
/// `embed_url`/`embed_code`, and `/submit` is where the served script posts the visitor's form. The
/// body-read deadline goes on this chain because `/submit` is a `Json` extractor a stranger can post
/// to (kanban t_59745689). No auth here, so there is no ordering question.
pub fn public_router(state: AppState) -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/s/:tenant_slug/widgets/:widget_slug/embed.js",
            get(widget_embed_js),
        )
        .route(
            "/s/:tenant_slug/widgets/:widget_slug/submit",
            post(widget_submit),
        )
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
}

#[cfg(test)]
mod embed_escaping_tests {
    use super::{css_color, html_text, js_str, widget_public_path, PUBLIC_WIDGET_BASE};

    /// The defect this closes, measured live 2026-10-14 (kanban t_f6bb2958): the DEFAULT welcome
    /// message went into the served script raw, its apostrophe closed the single-quoted JS string
    /// literal and Chromium raised `SyntaxError: Unexpected identifier 'll'` — the widget never
    /// rendered anywhere. Operator-authored text must therefore be neutral for BOTH contexts.
    #[test]
    fn the_default_welcome_message_survives_as_a_js_literal() {
        let welcome = "Thanks for reaching out! We'll get back to you shortly.";
        let escaped = js_str(welcome);
        assert!(escaped.contains("We\\'ll"), "{escaped}");
        assert!(
            !escaped.contains("'")
                || escaped.matches("\\'").count() == escaped.matches('\'').count(),
            "every apostrophe must be escaped: {escaped}"
        );
        assert!(!escaped.contains('\n') && !escaped.contains('\r'));
    }

    #[test]
    fn js_str_neutralises_the_string_terminators_and_the_backslash() {
        assert_eq!(js_str("a'b"), "a\\'b");
        assert_eq!(js_str("a\"b"), "a\\\"b");
        assert_eq!(js_str("a\\b"), "a\\\\b");
        assert_eq!(js_str("a\nb"), "a\\nb");
        assert_eq!(js_str("a\rb"), "a\\rb");
        assert_eq!(js_str("keep-it_simple #2563eb"), "keep-it_simple #2563eb");
    }

    /// The greeting lands in the panel's innerHTML (header text AND the message placeholder), so it
    /// is HTML-escaped; the whole markup is then embedded in a JS literal by `js_str`.
    #[test]
    fn the_greeting_is_html_escaped_then_js_escaped() {
        let raw = "EmbedProof's <UI> \"greeting\"";
        let html = html_text(raw);
        assert_eq!(html, "EmbedProof&#39;s &lt;UI&gt; &quot;greeting&quot;");
        let js = js_str(&html);
        assert!(
            !js.contains('\'') && !js.contains('"'),
            "no bare quote may survive: {js}"
        );
        assert!(js.contains("&lt;UI&gt;"));
    }

    /// A colour only reaches the widget's own `<style>`; anything a colour cannot contain is dropped
    /// so a typo cannot inject CSS into the customer's page, and an empty value falls back.
    #[test]
    fn css_color_keeps_only_colour_characters() {
        assert_eq!(css_color("#2563eb"), "#2563eb");
        assert_eq!(css_color("rgb(37, 99, 235)"), "rgb(37, 99, 235)");
        // A pasted stylesheet loses every character a colour cannot contain, so it cannot open a
        // rule in the CUSTOMER's page: `;`, braces and `:` never survive the filter.
        let injected = css_color("red;}body{display:none}");
        assert_eq!(injected, "redbodydisplaynone");
        assert!(!injected.contains('{') && !injected.contains('}'));
        assert!(!injected.contains(';') && !injected.contains(':'));
        assert_eq!(css_color(""), "#2563eb");
        assert_eq!(css_color("   "), "#2563eb");
        assert_eq!(css_color("<script>"), "script");
    }

    /// One spelling for the public surface: the path the embed script is served from and the path
    /// the snippet posts to come from the same helper, so they cannot drift apart.
    #[test]
    fn the_public_path_shape_is_the_advertised_one() {
        assert_eq!(
            widget_public_path("acme", "website-chat"),
            "/s/acme/widgets/website-chat"
        );
        assert_eq!(PUBLIC_WIDGET_BASE, "https://coreswiftcrm.com");
    }

    /// Regression guard for a shadowed `document` (kanban t_f6bb2958). The response callback took
    /// its payload as `d`, which shadowed the module's `var d = document`, so the SUCCESS branch
    /// threw `d.getElementById is not a function` AFTER the fetch had already created the ticket —
    /// the promise's `catch` then replaced the confirmation with "Network error. Try again.",
    /// so every visitor who successfully submitted a ticket was told it failed (measured in
    /// Chromium against the deployed app). The needle is assembled at runtime so this test's own
    /// text cannot satisfy it.
    #[test]
    fn the_response_callback_does_not_shadow_document() {
        const SRC: &str = include_str!("support_widgets.rs");
        let shadowing = ["then(function(", "d)"].concat();
        assert!(
            !SRC.contains(&shadowing),
            "the response callback must not reuse `d`, it shadows document: {shadowing}"
        );
        let named = ["then(function(", "res)"].concat();
        assert!(
            SRC.contains(&named),
            "the response callback should bind {named}"
        );
    }
}
