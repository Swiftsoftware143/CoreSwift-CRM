//! The knowledge base: the answer to a question asked twice.
//!
//! Two audiences, two surfaces, and the difference between them is enforced here rather than in the UI:
//!
//!   * an AGENT sees every article in their account — drafts, internal notes, all of it;
//!   * a CUSTOMER sees published AND public articles of one account, and nothing else.
//!
//! The public query names its conditions explicitly instead of reusing the agent query with a flag,
//! because a flag is one refactor away from being passed the wrong way round. Search is Postgres
//! full-text (the generated `search` column), so there is no extension to install and no second index to
//! keep in step with the text.

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct KbArticle {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub title: String,
    pub slug: String,
    pub body_md: String,
    pub status: String,
    pub visibility: String,
    pub locale: String,
    pub category_id: Option<Uuid>,
    pub view_count: i32,
    pub helpful_yes: i32,
    pub helpful_no: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// What a customer is allowed to see: no body of internal text beyond the article itself, no author, no
/// internal counters. Kept as its own struct so a new column cannot leak into the public payload by
/// being added to `KbArticle`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct PublicArticle {
    pub title: String,
    pub slug: String,
    pub body_md: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ArticleBody {
    pub title: Option<String>,
    /// The section an article belongs to, by slug. Unknown or omitted means "uncategorised", which still
    /// publishes and still searches — refusing to save an article because its section does not exist yet
    /// would punish somebody for tidiness they had not got to.
    pub section: Option<String>,
    pub slug: Option<String>,
    pub body_md: Option<String>,
    pub status: Option<String>,
    pub visibility: Option<String>,
    pub locale: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KbQuery {
    pub q: Option<String>,
    /// Which article, for the single-article read.
    pub slug: Option<String>,
    /// Tenant slug, for the customer-facing surface only.
    pub w: Option<String>,
    pub limit: Option<i64>,
}

/// The same slug rule the article editor uses, for callers outside this module.
pub fn slugify_public(raw: &str) -> Option<String> {
    slugify(raw)
}

fn tenant_of(c: &Claims) -> Result<Uuid, AppError> {
    Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)
}

/// `Some body` -> a slug safe to appear in a URL; `None` when nothing usable is left.
fn slugify(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut last_dash = true;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.extend(ch.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() || trimmed.len() < 2 {
        None
    } else {
        Some(trimmed.chars().take(80).collect())
    }
}

// ── agent surface ───────────────────────────────────────────────────────────────────────────────

pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(q): Query<KbQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant_of(&c)?;
    let limit = q.limit.unwrap_or(100).clamp(1, 200);
    let needle = q.q.as_deref().map(str::trim).filter(|v| !v.is_empty());

    // The agent query deliberately has NO visibility filter: the team's own knowledge is the point.
    let rows: Vec<KbArticle> = sqlx::query_as(
        r#"SELECT id, tenant_id, title, slug, body_md, status, visibility, locale, category_id,
                  view_count, helpful_yes, helpful_no, created_at, updated_at
             FROM helpdesk_kb_articles
            WHERE tenant_id = $1
              AND ($2::text IS NULL OR search @@ plainto_tsquery('english', $2))
            ORDER BY
              CASE status WHEN 'published' THEN 0 WHEN 'draft' THEN 1 ELSE 2 END,
              updated_at DESC
            LIMIT $3"#,
    )
    .bind(tid)
    .bind(needle)
    .bind(limit)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(
        serde_json::json!({ "articles": rows, "count": rows.len() }),
    ))
}

pub async fn get_one(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<KbArticle>> {
    let tid = tenant_of(&c)?;
    let row: KbArticle = sqlx::query_as(
        r#"SELECT id, tenant_id, title, slug, body_md, status, visibility, locale, category_id,
                  view_count, helpful_yes, helpful_no, created_at, updated_at
             FROM helpdesk_kb_articles WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Article not found".into()))?;
    Ok(Json(row))
}

/// Which section an article belongs to, from a slug or an id. `None` means uncategorised, and an unknown
/// value is treated the same way rather than failing the save: a section can be renamed while an article is
/// being written, and losing the article because of it would be absurd.
async fn resolve_section(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    section: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    let Some(raw) = section.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    if let Ok(id) = Uuid::parse_str(raw) {
        return Ok(sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM helpdesk_kb_categories WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(db)
        .await
        .map_err(AppError::Database)?);
    }
    Ok(sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM helpdesk_kb_categories WHERE slug = $1 AND tenant_id = $2",
    )
    .bind(raw)
    .bind(tenant_id)
    .fetch_optional(db)
    .await
    .map_err(AppError::Database)?)
}

pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<ArticleBody>,
) -> ApiResult<(StatusCode, Json<KbArticle>)> {
    let tid = tenant_of(&c)?;
    // A pasted title or body can carry an INVISIBLE NUL byte, which Postgres cannot store: the
    // save answered `500 {"code":500,...,"message":"Database error"}` with nothing written
    // (measured live 2026-11-26 on /api/helpdesk/kb). Strip the NUL, refuse a blank title or
    // another control character in the title BY NAME, and keep the body's own line breaks.
    let title = crate::security::free_text::single_line(
        &body.title.unwrap_or_default(),
        "Article title",
        200,
    )?;
    let slug = body
        .slug
        .as_deref()
        .and_then(slugify)
        .or_else(|| slugify(&title))
        .ok_or_else(|| {
            AppError::BadRequest("That title does not produce a usable address; add a slug.".into())
        })?;
    let status = body.status.unwrap_or_else(|| "draft".into());
    let visibility = body.visibility.unwrap_or_else(|| "public".into());
    if !matches!(status.as_str(), "draft" | "published" | "archived") {
        return Err(AppError::BadRequest(
            "An article is a draft, published or archived.".into(),
        ));
    }
    if !matches!(visibility.as_str(), "public" | "internal") {
        return Err(AppError::BadRequest(
            "An article is for customers (public) or for the team (internal).".into(),
        ));
    }
    let author = Uuid::parse_str(&c.sub).ok();
    let category_id = resolve_section(&s.db, tid, body.section.as_deref()).await?;

    let row: KbArticle = sqlx::query_as(
        r#"INSERT INTO helpdesk_kb_articles
             (tenant_id, title, slug, body_md, status, visibility, locale, author_id, category_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
           RETURNING id, tenant_id, title, slug, body_md, status, visibility, locale, category_id,
                     view_count, helpful_yes, helpful_no, created_at, updated_at"#,
    )
    .bind(tid)
    .bind(&title)
    .bind(&slug)
    .bind(crate::security::free_text::nul_free(body.body_md).unwrap_or_default())
    .bind(&status)
    .bind(&visibility)
    .bind(body.locale.unwrap_or_else(|| "en".into()))
    .bind(author)
    .bind(category_id)
    .fetch_one(&s.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.code().as_deref() == Some("23505") => {
            AppError::BadRequest(format!(
                "There is already an article at `{slug}` — edit that one or pick another title."
            ))
        }
        other => AppError::Database(other),
    })?;

    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn update(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<ArticleBody>,
) -> ApiResult<Json<KbArticle>> {
    let tid = tenant_of(&c)?;
    if let Some(v) = body.visibility.as_deref() {
        if !matches!(v, "public" | "internal") {
            return Err(AppError::BadRequest(
                "An article is for customers (public) or for the team (internal).".into(),
            ));
        }
    }
    if let Some(v) = body.status.as_deref() {
        if !matches!(v, "draft" | "published" | "archived") {
            return Err(AppError::BadRequest(
                "An article is a draft, published or archived.".into(),
            ));
        }
    }

    let category_id = match body.section.as_deref() {
        // an explicit empty string means "move it out of its section"
        Some(v) if v.trim().is_empty() => Some(None),
        Some(v) => Some(resolve_section(&s.db, tid, Some(v)).await?),
        None => None,
    };

    let row: KbArticle = sqlx::query_as(
        r#"UPDATE helpdesk_kb_articles
              SET title      = COALESCE($3, title),
                  body_md    = COALESCE($4, body_md),
                  status     = COALESCE($5, status),
                  visibility = COALESCE($6, visibility),
                  locale     = COALESCE($7, locale),
                  category_id = COALESCE($8, category_id),
                  updated_at = NOW()
            WHERE id = $1 AND tenant_id = $2
            RETURNING id, tenant_id, title, slug, body_md, status, visibility, locale, category_id,
                      view_count, helpful_yes, helpful_no, created_at, updated_at"#,
    )
    .bind(id)
    .bind(tid)
    // Same hygiene on the edit path: a NUL in a renamed title or edited body 500'd here too.
    .bind(crate::security::free_text::optional_line(
        body.title.as_deref(),
        "Article title",
        200,
    )?)
    .bind(crate::security::free_text::nul_free(body.body_md))
    .bind(body.status)
    .bind(body.visibility)
    .bind(body.locale)
    .bind(category_id.flatten())
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Article not found".into()))?;

    // an article that came from a conversation and is now live closes that loop in the ledger, so the ratio of
    // suggestions that turned into published answers is a fact rather than a feeling
    if row.status == "published" {
        let _ = crate::helpdesk::learn::mark_published(&s.db, tid, row.id).await;
    }
    Ok(Json(row))
}

pub async fn delete_article(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let tid = tenant_of(&c)?;
    let done = sqlx::query("DELETE FROM helpdesk_kb_articles WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("Article not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── customer surface ───────────────────────────────────────────────────────────────────────────

/// Resolve the account a widget belongs to.
///
/// Accepts an account id (what the embed script already holds) or a workspace slug (what a person can
/// type). An id is tried first because the widget path is the hot one and it is exact.
async fn tenant_by_ref(s: &AppState, who: &str) -> Result<Option<Uuid>, AppError> {
    if let Ok(id) = Uuid::parse_str(who.trim()) {
        return sqlx::query_scalar::<_, Uuid>("SELECT id FROM tenants WHERE id = $1 AND is_active")
            .bind(id)
            .fetch_optional(&s.db)
            .await
            .map_err(AppError::Database);
    }
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM tenants WHERE slug = $1 AND is_active")
        .bind(who.trim())
        .fetch_optional(&s.db)
        .await
        .map_err(AppError::Database)
}

/// What a customer may read: published AND public, for one account.
///
/// The two conditions are written out in this query rather than passed in, so this endpoint cannot be
/// talked into returning an internal article by a caller, a default, or a later edit to shared code.
pub async fn public_list(
    State(s): State<AppState>,
    Query(q): Query<KbQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let slug =
        q.w.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("Which business is this for? `w` is required.".into())
            })?;
    let Some(tid) = tenant_by_ref(&s, slug).await? else {
        return Ok(Json(serde_json::json!({ "articles": [], "count": 0 })));
    };

    let limit = q.limit.unwrap_or(10).clamp(1, 25);
    let needle = q.q.as_deref().map(str::trim).filter(|v| !v.is_empty());

    let rows: Vec<PublicArticle> = sqlx::query_as(
        r#"SELECT title, slug, body_md, updated_at
             FROM helpdesk_kb_articles
            WHERE tenant_id = $1
              AND status = 'published'
              AND visibility = 'public'
              AND ($2::text IS NULL OR search @@ plainto_tsquery('english', $2))
            ORDER BY
              CASE WHEN $2::text IS NULL THEN view_count ELSE 0 END DESC,
              ts_rank(search, plainto_tsquery('english', COALESCE($2, ''))) DESC,
              updated_at DESC
            LIMIT $3"#,
    )
    .bind(tid)
    .bind(needle)
    .bind(limit)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(serde_json::json!({
        "articles": rows.iter().map(|a| serde_json::json!({
            "title": a.title,
            "slug": a.slug,
            "body_md": a.body_md,
            "updated_at": a.updated_at,
        })).collect::<Vec<_>>(),
        "count": rows.len()
    })))
}

// ── sections ──────────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct KbSection {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub position: i32,
}

#[derive(Debug, Deserialize)]
pub struct SectionBody {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub description: Option<String>,
    pub position: Option<i32>,
}

pub async fn list_sections(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = tenant_of(&c)?;
    let rows: Vec<KbSection> = sqlx::query_as(
        "SELECT id, name, slug, description, position FROM helpdesk_kb_categories
          WHERE tenant_id = $1 ORDER BY position, name",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        serde_json::json!({ "sections": rows, "count": rows.len() }),
    ))
}

pub async fn create_section(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<SectionBody>,
) -> ApiResult<(StatusCode, Json<KbSection>)> {
    let tid = tenant_of(&c)?;
    // A pasted section name can carry an INVISIBLE NUL byte (measured live 2026-11-26: the
    // section save answered `500 ... "Database error"`). Strip it, refuse a blank name or another
    // control character BY NAME, and NUL-free the description without touching its line breaks.
    let name = crate::security::free_text::single_line(
        &body.name.unwrap_or_default(),
        "Section name",
        200,
    )?;
    let slug = body
        .slug
        .as_deref()
        .and_then(slugify)
        .or_else(|| slugify(&name))
        .ok_or_else(|| AppError::BadRequest("That name does not make a usable address.".into()))?;
    let row: KbSection = sqlx::query_as(
        "INSERT INTO helpdesk_kb_categories (tenant_id, name, slug, description, position)
         VALUES ($1, $2, $3, $4, COALESCE($5, 100))
         RETURNING id, name, slug, description, position",
    )
    .bind(tid)
    .bind(&name)
    .bind(&slug)
    .bind(crate::security::free_text::nul_free(body.description))
    .bind(body.position)
    .fetch_one(&s.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(ref d) if d.code().as_deref() == Some("23505") => {
            AppError::BadRequest(format!("There is already a section at `{slug}`."))
        }
        other => AppError::Database(other),
    })?;
    Ok((StatusCode::CREATED, Json(row)))
}

pub async fn update_section(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(body): Json<SectionBody>,
) -> ApiResult<Json<KbSection>> {
    let tid = tenant_of(&c)?;
    let row: KbSection = sqlx::query_as(
        "UPDATE helpdesk_kb_categories
            SET name = COALESCE($3, name),
                description = COALESCE($4, description),
                position = COALESCE($5, position),
                updated_at = NOW()
          WHERE id = $1 AND tenant_id = $2
          RETURNING id, name, slug, description, position",
    )
    .bind(id)
    .bind(tid)
    // Same hygiene on the section rename/description edit (a NUL 500'd here too).
    .bind(crate::security::free_text::optional_line(
        body.name.as_deref(),
        "Section name",
        200,
    )?)
    .bind(crate::security::free_text::nul_free(body.description))
    .bind(body.position)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Section not found".into()))?;
    Ok(Json(row))
}

pub async fn delete_section(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let tid = tenant_of(&c)?;
    // Articles are not deleted with their section (the FK is ON DELETE SET NULL): a section is a shelf, and
    // taking the shelf away must not throw the books out.
    let done = sqlx::query("DELETE FROM helpdesk_kb_categories WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound("Section not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ── what a stranger can browse: the front page of a help centre ───────────────────────────────

/// GET /api/public/helpdesk/kb/index?w=<account>
///
/// The shape a help centre needs and nothing more: the business's name, its sections with their published
/// articles, and the most-read articles as a way in for somebody who does not know where to look. Written as
/// one response because a stranger on a slow connection should not need four round trips to see a menu.
pub async fn public_index(
    State(s): State<AppState>,
    Query(q): Query<KbQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let slug =
        q.w.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest("Which business is this for? `w` is required.".into())
            })?;
    let Some(tid) = tenant_by_ref(&s, slug).await? else {
        return Ok(Json(serde_json::json!({
            "site": null, "sections": [], "popular": [], "count": 0
        })));
    };

    let site_name: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(NULLIF(brand->>'name', ''), name) FROM helpdesk_sites WHERE tenant_id = $1",
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await
    .ok()
    .flatten();

    let sections: Vec<(Uuid, String, String, Option<String>, i32)> = sqlx::query_as(
        "SELECT id, name, slug, description, position FROM helpdesk_kb_categories
          WHERE tenant_id = $1 ORDER BY position, name",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let articles: Vec<(Option<Uuid>, String, String, i32)> = sqlx::query_as(
        "SELECT category_id, title, slug, view_count FROM helpdesk_kb_articles
          WHERE tenant_id = $1 AND status = 'published' AND visibility = 'public'
          ORDER BY title",
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    let grouped: Vec<serde_json::Value> = sections
        .iter()
        .map(|(id, name, sslug, desc, _pos)| {
            let items: Vec<serde_json::Value> = articles
                .iter()
                .filter(|(cat, _, _, _)| cat.as_ref() == Some(id))
                .map(|(_, title, aslug, _)| serde_json::json!({ "title": title, "slug": aslug }))
                .collect();
            serde_json::json!({
                "name": name, "slug": sslug, "description": desc, "articles": items,
                "count": items.len(),
            })
        })
        .collect();

    let loose: Vec<serde_json::Value> = articles
        .iter()
        .filter(|(cat, _, _, _)| cat.is_none())
        .map(|(_, title, aslug, _)| serde_json::json!({ "title": title, "slug": aslug }))
        .collect();

    let mut popular: Vec<(String, String, i32)> = articles
        .iter()
        .map(|(_, title, aslug, views)| (title.clone(), aslug.clone(), *views))
        .collect();
    popular.sort_by(|a, b| b.2.cmp(&a.2));
    popular.truncate(5);

    Ok(Json(serde_json::json!({
        "site": site_name,
        // the account id is what the embeddable widget is addressed by; it is public by construction (it is
        // the value already in every customer's script tag), so the help centre can offer the same chat.
        "tenant_id": tid,
        "sections": grouped,
        "uncategorised": loose,
        "popular": popular.iter().map(|(t, s, _)| serde_json::json!({ "title": t, "slug": s })).collect::<Vec<_>>(),
        "count": articles.len(),
    })))
}

/// GET /api/public/helpdesk/kb/article?w=<account>&slug=<article>
///
/// One article, for a reader. Published AND public only — the same two conditions as the search, spelled out
/// again rather than shared, because a shared helper is one edit away from being wanted for both audiences.
pub async fn public_article(
    State(s): State<AppState>,
    Query(q): Query<KbQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let who = q.w.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let slug = q.slug.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let (Some(who), Some(slug)) = (who, slug) else {
        return Err(AppError::BadRequest(
            "Which business and which article? `w` and `slug` are required.".into(),
        ));
    };
    let Some(tid) = tenant_by_ref(&s, who).await? else {
        return Err(AppError::NotFound("Article not found".into()));
    };

    let row: Option<(String, String, i32, i32, Option<String>, Option<String>)> = sqlx::query_as(
        r#"SELECT a.title, a.body_md, a.helpful_yes, a.helpful_no, c.name, c.slug
             FROM helpdesk_kb_articles a
             LEFT JOIN helpdesk_kb_categories c ON c.id = a.category_id
            WHERE a.tenant_id = $1 AND a.slug = $2
              AND a.status = 'published' AND a.visibility = 'public'"#,
    )
    .bind(tid)
    .bind(slug)
    .fetch_optional(&s.db)
    .await?;

    let Some((title, body_md, yes, no, section, section_slug)) = row else {
        return Err(AppError::NotFound("Article not found".into()));
    };

    // counting a read is how "most read" stops being a guess; it is deliberately not part of the answer
    let _ = sqlx::query("UPDATE helpdesk_kb_articles SET view_count = view_count + 1 WHERE tenant_id = $1 AND slug = $2")
        .bind(tid)
        .bind(slug)
        .execute(&s.db)
        .await;
    let _ = sqlx::query(
        "INSERT INTO helpdesk_kb_views (tenant_id, article_id) SELECT $1, id FROM helpdesk_kb_articles WHERE tenant_id = $1 AND slug = $2",
    )
    .bind(tid)
    .bind(slug)
    .execute(&s.db)
    .await;

    Ok(Json(serde_json::json!({
        "title": title,
        "body_md": body_md,
        "section": section.map(|n| serde_json::json!({ "name": n, "slug": section_slug })),
        "helpful_yes": yes,
        "helpful_no": no,
    })))
}

#[derive(Debug, Deserialize)]
pub struct VoteBody {
    pub w: String,
    pub slug: String,
    pub helpful: bool,
}

/// POST /api/public/helpdesk/kb/vote — "was this helpful?" with no login.
///
/// The counters an article shows are bumped here, and the vote itself is stored, so the number can always be
/// traced to votes rather than being a number somebody once typed. Nothing identifies the voter.
pub async fn public_vote(
    State(s): State<AppState>,
    Json(body): Json<VoteBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let Some(tid) = tenant_by_ref(&s, &body.w).await? else {
        return Err(AppError::NotFound("Article not found".into()));
    };
    let updated = sqlx::query(
        r#"UPDATE helpdesk_kb_articles
              SET helpful_yes = CASE WHEN $3 THEN helpful_yes + 1 ELSE helpful_yes END,
                  helpful_no  = CASE WHEN $3 THEN helpful_no ELSE helpful_no + 1 END
            WHERE tenant_id = $1 AND slug = $2
              AND status = 'published' AND visibility = 'public'
            RETURNING id"#,
    )
    .bind(tid)
    .bind(&body.slug)
    .bind(body.helpful)
    .fetch_optional(&s.db)
    .await?;
    let Some(row) = updated else {
        return Err(AppError::NotFound("Article not found".into()));
    };
    let article_id: Uuid = row.get("id");

    sqlx::query(
        "INSERT INTO helpdesk_kb_votes (tenant_id, article_id, helpful) VALUES ($1, $2, $3)",
    )
    .bind(tid)
    .bind(article_id)
    .bind(body.helpful)
    .execute(&s.db)
    .await?;

    Ok(Json(serde_json::json!({ "recorded": true })))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/kb", get(list).post(create))
        .route("/kb/:id", get(get_one).patch(update).delete(delete_article))
        .route("/kb/sections", get(list_sections).post(create_section))
        .route(
            "/kb/sections/:id",
            axum::routing::patch(update_section).delete(delete_section),
        )
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugs_are_url_safe_and_stable() {
        assert_eq!(
            slugify("How to export a list").as_deref(),
            Some("how-to-export-a-list")
        );
        assert_eq!(
            slugify("  Pricing!! (2026) ").as_deref(),
            Some("pricing-2026")
        );
        assert_eq!(slugify("???"), None);
        assert_eq!(slugify(""), None);
    }
}
