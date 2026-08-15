//! Support Widgets — multi-widget embeddable support forms.
//!
//! Each widget is named (e.g. "CoreSwift CRM", "FunnelSwift") and routes
//! submissions to a named inbox. Gated by plan feature "max_widgets".
//!
//! Public routes (no auth):
//! - GET  /api/widgets/:tenant_slug/:widget_slug/embed.js — embeddable JS widget
//! - POST /api/widgets/:tenant_slug/:widget_slug/submit   — public form submit
//!
//! Auth routes:
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
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

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

    // Plan gate: check max_widgets
    let plan_limit: Option<i64> = sqlx::query_scalar(
        "SELECT (p.features->>'max_widgets')::bigint FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id WHERE tp.tenant_id = $1 AND tp.status = 'active'",
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .flatten();

    if let Some(limit) = plan_limit {
        let current: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM support_widgets WHERE tenant_id = $1",
        )
        .bind(tid)
        .fetch_one(&s.db)
        .await?;
        if current >= limit {
            return Err(AppError::BadRequest(format!(
                "Widget limit reached ({}). Upgrade your plan for more widgets.",
                limit
            )));
        }
    }

    // Auto-create inbox if none provided
    let inbox_id = if let Some(iid) = req.inbox_id {
        iid
    } else {
        let iid = Uuid::new_v4();
        // Create a default inbox named after the widget
        sqlx::query(
            "INSERT INTO ticket_inboxes (id, tenant_id, name) VALUES ($1, $2, $3)",
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

    Ok(Json(json!({
        "widget": widget,
        "embed_code": format!("<script src=\"https://coreswiftcrm.com/s/{}/widgets/{}/embed.js\" defer></script>", tenant_slug, slug),
        "embed_url": format!("https://coreswiftcrm.com/s/{}/widgets/{}/embed.js", tenant_slug, slug),
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

    Ok(Json(json!({ "widget": widget })))
}

/// DELETE /api/widgets/:id — delete widget
pub async fn delete_widget(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM support_widgets WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Widget not found".into()));
    }

    Ok(Json(json!({ "deleted": true })))
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

    let endpoint = format!("/api/widgets/{}/{}/submit", tenant_slug, widget_slug);
    let js = format!(
        r#"(function(){{
    var d=document;
    if(d.getElementById('coreswift-widget-{}'))return;
    var s=d.createElement('style');
    s.textContent='.csw-btn{{position:fixed;{}:20px;bottom:20px;width:56px;height:56px;border-radius:50%;background:{};color:#fff;border:none;cursor:pointer;box-shadow:0 4px 16px rgba(0,0,0,.25);font-size:24px;z-index:9999;display:flex;align-items:center;justify-content:center;transition:transform .2s}}.csw-btn:hover{{transform:scale(1.1)}}.csw-panel{{display:none;position:fixed;{}:20px;bottom:88px;width:340px;max-height:70vh;background:#fff;border-radius:16px;box-shadow:0 8px 32px rgba(0,0,0,.2);z-index:9998;overflow:hidden;font-family:-apple-system,system-ui,sans-serif}}.csw-panel.open{{display:flex;flex-direction:column}}.csw-panel-header{{background:{};color:#fff;padding:16px 20px;font-weight:700;font-size:15px}}.csw-panel-body{{padding:16px;overflow-y:auto;flex:1}}.csw-panel-body input,.csw-panel-body textarea{{width:100%;padding:10px 12px;margin-bottom:10px;border:1px solid #e2e8f0;border-radius:10px;font-size:14px;font-family:inherit;outline:0}}.csw-panel-body input:focus,.csw-panel-body textarea:focus{{border-color:{};box-shadow:0 0 0 3px {}22}}.csw-panel-body textarea{{resize:none;height:100px}}.csw-submit{{width:100%;padding:12px;background:{};color:#fff;border:none;border-radius:10px;font-size:14px;font-weight:600;cursor:pointer}}.csw-submit:hover{{filter:brightness(1.1)}}.csw-msg{{padding:10px;border-radius:8px;font-size:13px;margin-top:10px;display:none}}';
    d.head.appendChild(s);
    var btn=d.createElement('button');
    btn.className='csw-btn';btn.id='coreswift-widget-{}';btn.textContent='💬';
    d.body.appendChild(btn);
    var panel=d.createElement('div');
    panel.className='csw-panel';panel.id='csw-panel-{}';
    panel.innerHTML='<div class="csw-panel-header">{}&nbsp;&nbsp;<span style="cursor:pointer;float:right" onclick="document.getElementById(\\'csw-panel-{}\\').classList.remove(\\'open\\')">✕</span></div><div class="csw-panel-body"><input type="text" id="csw-name" placeholder="Your name"><input type="email" id="csw-email" placeholder="Your email"><input type="text" id="csw-subject" placeholder="Subject"><textarea id="csw-message" placeholder="{}"></textarea><button class="csw-submit" onclick="coreswiftWidgetSubmit(\\'csw-panel-{}\\')">Send Message</button><div class="csw-msg" id="csw-msg-{}"></div></div>';
    d.body.appendChild(panel);
    btn.onclick=function(){{var p=d.getElementById('csw-panel-{}');p.classList.toggle('open')}};
    window.coreswiftWidgetSubmit=function(pid){{
        var p=d.getElementById(pid),name=d.getElementById('csw-name').value.trim(),email=d.getElementById('csw-email').value.trim(),subject=d.getElementById('csw-subject').value.trim(),msg=d.getElementById('csw-message').value.trim(),el=d.getElementById('csw-msg-{}');
        if(!name||!email||!subject||!msg){{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent='Please fill in all fields';return}}
        el.style.display='none';
        fetch('{}',{{method:'POST',headers:{{'Content-Type':'application/json'}},body:JSON.stringify({{name:name,email:email,subject:subject,message:msg}})}})
        .then(function(r){{return r.json()}})
        .then(function(d){{
            if(d.success){{el.style.display='block';el.style.background='#f0fdf4';el.style.color='#166534';el.textContent='{}';d.getElementById('csw-message').value='';d.getElementById('csw-subject').value=''}}
            else{{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent=d.message||'Something went wrong'}}
        }}).catch(function(){{el.style.display='block';el.style.background='#fef2f2';el.style.color='#dc2626';el.textContent='Network error. Try again.'}});
    }};
}})();"#,
        widget.slug,
        if widget.position == "bottom-left" { "left" } else { "right" },
        widget.theme_color,
        if widget.position == "bottom-left" { "left" } else { "right" },
        widget.theme_color,
        widget.theme_color,
        widget.theme_color,
        widget.theme_color,
        widget.slug,
        widget.slug,
        widget.greeting,
        widget.slug,
        widget.greeting,
        widget.slug,
        widget.slug,
        widget.slug,
        widget.slug,
        endpoint,
        widget.welcome_msg,
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

pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, patch, post};
    use axum::middleware;

    // Public routes — no auth
    let public = axum::Router::new()
        .route(
            "/widgets/:tenant_slug/:widget_slug/embed.js",
            get(widget_embed_js),
        )
        .route(
            "/widgets/:tenant_slug/:widget_slug/submit",
            post(widget_submit),
        );

    // Protected routes
    let protected = axum::Router::new()
        .route("/", get(list_widgets).post(create_widget))
        .route(
            "/:id",
            patch(update_widget).delete(delete_widget),
        )
        .route("/inboxes", get(list_inboxes).post(create_inbox))
        .route(
            "/inboxes/:id",
            patch(update_inbox).delete(delete_inbox),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ));

    public.merge(protected)
}

/// Alias for public router — used in main.rs alongside tickets public router
pub fn public_router(_state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/s/:tenant_slug/widgets/:widget_slug/embed.js",
            get(widget_embed_js),
        )
        .route(
            "/s/:tenant_slug/widgets/:widget_slug/submit",
            post(widget_submit),
        )
}
