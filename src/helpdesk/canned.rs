//! Saved answers, and the small set of placeholders they may contain.
//!
//! The placeholders are filled HERE, once, at the moment the text goes into a reply — not in the browser and
//! not when the reply is stored. An agent who sees `Hi {{contact.first_name}}` in their reply box and sends
//! it has sent braces to a customer; filling on the server means the text that lands is the text that was
//! shown to the agent, and a test pins each substitution.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct CannedReply {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: String,
    pub shortcut: Option<String>,
    pub body_md: String,
    pub shared: bool,
    pub use_count: i32,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, serde::Deserialize)]
pub struct CannedBody {
    pub title: Option<String>,
    pub shortcut: Option<String>,
    pub body_md: Option<String>,
    pub shared: Option<bool>,
}

/// What a placeholder is allowed to know about.
pub struct RenderContext<'a> {
    pub contact_first_name: Option<&'a str>,
    pub ticket_number: Option<i32>,
    pub agent_first_name: Option<&'a str>,
    pub site_name: Option<&'a str>,
}

/// Fill the placeholders. An unknown placeholder is left as it was rather than blanked: silently deleting an
/// agent's words is worse than leaving something they will notice and fix.
pub fn render(body: &str, ctx: &RenderContext<'_>) -> String {
    let first = ctx
        .contact_first_name
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("there");
    let agent = ctx
        .agent_first_name
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("the team");
    let number = ctx
        .ticket_number
        .map(|n| format!("CS-{n}"))
        .unwrap_or_else(|| "your ticket".to_string());
    let site = ctx.site_name.map(str::trim).filter(|v| !v.is_empty());

    let mut out = body
        .replace("{{contact.first_name}}", first)
        .replace("{{ticket.number}}", &number)
        .replace("{{agent.first_name}}", agent);
    if let Some(site) = site {
        out = out.replace("{{site.name}}", site);
    }
    out
}

/// The account's replies, most-used first: an agent's own habit is the best ordering there is.
pub async fn list(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<CannedReply>, AppError> {
    sqlx::query_as(
        r#"SELECT id, tenant_id, title, shortcut, body_md, shared, use_count, updated_at
             FROM helpdesk_canned_replies
            WHERE tenant_id = $1
            ORDER BY use_count DESC, title ASC"#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)
}

pub async fn create(
    pool: &PgPool,
    tenant_id: Uuid,
    owner: Option<Uuid>,
    body: CannedBody,
) -> Result<CannedReply, AppError> {
    // A title or answer pasted out of an email, a spreadsheet or another desk can carry an
    // INVISIBLE NUL byte, which Postgres cannot store in a text column: the save answered
    // `500 {"code":500,...,"message":"Database error"}` with nothing written (measured live
    // 2026-11-26 on /api/helpdesk/canned). The shared hygiene helper strips the NUL, refuses a
    // blank title and any OTHER control character in the title BY NAME, and leaves the answer
    // body's own line breaks alone - only the invisible byte goes.
    let title = crate::security::free_text::single_line(
        &body.title.unwrap_or_default(),
        "Saved answer title",
        200,
    )?;
    let text = crate::security::free_text::strip_nul(&body.body_md.unwrap_or_default());
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(AppError::BadRequest(
            "A saved answer needs some text.".into(),
        ));
    }
    let shortcut = body
        .shortcut
        .map(|s| s.trim().trim_start_matches('/').to_ascii_lowercase())
        .filter(|s| !s.is_empty());

    sqlx::query_as(
        r#"INSERT INTO helpdesk_canned_replies (tenant_id, title, shortcut, body_md, owner_id, shared)
           VALUES ($1, $2, $3, $4, $5, COALESCE($6, true))
           RETURNING id, tenant_id, title, shortcut, body_md, shared, use_count, updated_at"#,
    )
    .bind(tenant_id)
    .bind(&title)
    .bind(shortcut)
    .bind(&text)
    .bind(owner)
    .bind(body.shared)
    .fetch_one(pool)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.code().as_deref() == Some("23505") => AppError::BadRequest(
            "Another saved answer already uses that shortcut.".into(),
        ),
        other => AppError::Database(other),
    })
}

pub async fn update(
    pool: &PgPool,
    tenant_id: Uuid,
    id: Uuid,
    body: CannedBody,
) -> Result<CannedReply, AppError> {
    let shortcut = body
        .shortcut
        .map(|s| s.trim().trim_start_matches('/').to_ascii_lowercase())
        .filter(|s| !s.is_empty());

    sqlx::query_as(
        r#"UPDATE helpdesk_canned_replies
              SET title = COALESCE($3, title),
                  body_md = COALESCE($4, body_md),
                  shortcut = COALESCE($5, shortcut),
                  shared = COALESCE($6, shared),
                  updated_at = NOW()
            WHERE id = $1 AND tenant_id = $2
            RETURNING id, tenant_id, title, shortcut, body_md, shared, use_count, updated_at"#,
    )
    .bind(id)
    .bind(tenant_id)
    // Same hygiene on the edit path: a NUL in a renamed title or answer 500'd here too.
    .bind(crate::security::free_text::optional_line(
        body.title.as_deref(),
        "Saved answer title",
        200,
    )?)
    .bind(
        body.body_md
            .map(|t| crate::security::free_text::strip_nul(&t).trim().to_string())
            .filter(|t| !t.is_empty()),
    )
    .bind(shortcut)
    .bind(body.shared)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Saved answer not found".into()))
}

pub async fn delete(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<(), AppError> {
    let done = sqlx::query("DELETE FROM helpdesk_canned_replies WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(pool)
        .await
        .map_err(AppError::Database)?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("Saved answer not found".into()));
    }
    Ok(())
}

/// Count a use, so the picker learns which answers an agent actually reaches for.
pub async fn count_use(pool: &PgPool, tenant_id: Uuid, id: Uuid) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE helpdesk_canned_replies SET use_count = use_count + 1 WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

// ── HTTP ──────────────────────────────────────────────────────────────────────────────────────

use crate::auth::models::Claims;
use crate::errors::ApiResult;
use crate::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    Extension, Json,
};

fn tenant_of(c: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)
}

pub async fn list_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant_of(&c)?;
    let rows = list(&s.db, tid).await?;
    Ok(Json(
        serde_json::json!({ "replies": rows, "count": rows.len() }),
    ))
}

pub async fn create_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CannedBody>,
) -> ApiResult<(StatusCode, Json<CannedReply>)> {
    let tid = tenant_of(&c)?;
    let owner = Uuid::parse_str(&c.sub).ok();
    let row = create(&s.db, tid, owner, body).await?;
    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn update_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<CannedBody>,
) -> ApiResult<Json<CannedReply>> {
    let tid = tenant_of(&c)?;
    Ok(Json(update(&s.db, tid, id, body).await?))
}

pub async fn delete_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let tid = tenant_of(&c)?;
    delete(&s.db, tid, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
pub struct RenderBody {
    pub ticket_id: Uuid,
}

/// The text an agent will actually send, with the placeholders already filled.
///
/// Filled on the SERVER because the agent has to see exactly what the customer will receive: if the browser
/// did the substitution, a change here and a change there could disagree, and the disagreement would reach a
/// customer's inbox.
pub async fn render_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<RenderBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant_of(&c)?;

    let reply: Option<(String,)> = sqlx::query_as(
        "SELECT body_md FROM helpdesk_canned_replies WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;
    let Some((body_md,)) = reply else {
        return Err(AppError::NotFound("Saved answer not found".into()));
    };

    // the context this reply is being sent into
    let ctx_row: Option<(Option<String>, Option<i32>, Option<String>)> = sqlx::query_as(
        r#"SELECT NULLIF(TRIM(COALESCE(contact_name, '')), ''),
                  number,
                  NULLIF(TRIM(COALESCE(contact_email, '')), '')
             FROM tickets WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(body.ticket_id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;
    let Some((contact_name, number, _email)) = ctx_row else {
        return Err(AppError::NotFound("Ticket not found".into()));
    };

    let agent_name: Option<String> =
        sqlx::query_scalar::<_, String>("SELECT COALESCE(name, '') FROM users WHERE id = $1")
            .bind(Uuid::parse_str(&c.sub).unwrap_or(Uuid::nil()))
            .fetch_optional(&s.db)
            .await
            .ok()
            .flatten();
    let agent_first = agent_name
        .filter(|n| !n.trim().is_empty())
        .and_then(|n| n.split_whitespace().next().map(str::to_string));

    let site_name =
        sqlx::query_scalar::<_, String>("SELECT name FROM helpdesk_sites WHERE tenant_id = $1")
            .bind(tid)
            .fetch_optional(&s.db)
            .await
            .ok()
            .flatten();

    let contact_first = contact_name
        .as_deref()
        .and_then(|n| n.split_whitespace().next())
        .map(str::to_string);
    let ctx = RenderContext {
        contact_first_name: contact_first.as_deref(),
        ticket_number: number,
        agent_first_name: agent_first.as_deref(),
        site_name: site_name.as_deref(),
    };
    let text = render(&body_md, &ctx);
    count_use(&s.db, tid, id).await?;

    Ok(Json(serde_json::json!({ "text": text })))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/canned", get(list_handler).post(create_handler))
        .route(
            "/canned/:id",
            axum::routing::patch(update_handler).delete(delete_handler),
        )
        .route("/canned/:id/render", post(render_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>() -> RenderContext<'a> {
        RenderContext {
            contact_first_name: Some("Dana"),
            ticket_number: Some(1042),
            agent_first_name: Some("David"),
            site_name: Some("Palm Bay Pulse"),
        }
    }

    #[test]
    fn every_documented_placeholder_is_filled() {
        let out = render(
            "Hi {{contact.first_name}}, this is {{agent.first_name}} at {{site.name}}. \
             Your reference is {{ticket.number}}, but you can just reply.",
            &ctx(),
        );
        assert_eq!(
            out,
            "Hi Dana, this is David at Palm Bay Pulse. Your reference is CS-1042, but you can just reply."
        );
        assert!(
            !out.contains("{{"),
            "no placeholder may survive into a customer's inbox"
        );
    }

    #[test]
    fn a_missing_name_does_not_send_braces_or_an_empty_line() {
        let out = render(
            "Hi {{contact.first_name}}",
            &RenderContext {
                contact_first_name: None,
                ticket_number: None,
                agent_first_name: None,
                site_name: None,
            },
        );
        assert_eq!(out, "Hi there");
    }

    #[test]
    fn an_unknown_placeholder_is_left_alone_rather_than_erased() {
        let out = render("Hi {{contact.first_name}} — see {{something.else}}", &ctx());
        assert_eq!(out, "Hi Dana — see {{something.else}}");
    }
}
