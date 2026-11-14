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
    pub slug: Option<String>,
    pub body_md: Option<String>,
    pub status: Option<String>,
    pub visibility: Option<String>,
    pub locale: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct KbQuery {
    pub q: Option<String>,
    /// Tenant slug, for the customer-facing surface only.
    pub w: Option<String>,
    pub limit: Option<i64>,
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
        r#"SELECT id, tenant_id, title, slug, body_md, status, visibility, locale,
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
        r#"SELECT id, tenant_id, title, slug, body_md, status, visibility, locale,
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

pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<ArticleBody>,
) -> ApiResult<(StatusCode, Json<KbArticle>)> {
    let tid = tenant_of(&c)?;
    let title = body.title.unwrap_or_default().trim().to_string();
    if title.is_empty() {
        return Err(AppError::BadRequest("An article needs a title.".into()));
    }
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

    let row: KbArticle = sqlx::query_as(
        r#"INSERT INTO helpdesk_kb_articles
             (tenant_id, title, slug, body_md, status, visibility, locale, author_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING id, tenant_id, title, slug, body_md, status, visibility, locale,
                     view_count, helpful_yes, helpful_no, created_at, updated_at"#,
    )
    .bind(tid)
    .bind(&title)
    .bind(&slug)
    .bind(body.body_md.unwrap_or_default())
    .bind(&status)
    .bind(&visibility)
    .bind(body.locale.unwrap_or_else(|| "en".into()))
    .bind(author)
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

    let row: KbArticle = sqlx::query_as(
        r#"UPDATE helpdesk_kb_articles
              SET title      = COALESCE($3, title),
                  body_md    = COALESCE($4, body_md),
                  status     = COALESCE($5, status),
                  visibility = COALESCE($6, visibility),
                  locale     = COALESCE($7, locale),
                  updated_at = NOW()
            WHERE id = $1 AND tenant_id = $2
            RETURNING id, tenant_id, title, slug, body_md, status, visibility, locale,
                      view_count, helpful_yes, helpful_no, created_at, updated_at"#,
    )
    .bind(id)
    .bind(tid)
    .bind(
        body.title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty()),
    )
    .bind(body.body_md)
    .bind(body.status)
    .bind(body.visibility)
    .bind(body.locale)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound("Article not found".into()))?;
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

/// Resolve the account from the workspace slug a widget was configured with.
async fn tenant_by_slug(s: &AppState, slug: &str) -> Result<Option<Uuid>, AppError> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM tenants WHERE slug = $1 AND is_active")
        .bind(slug)
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
    let Some(tid) = tenant_by_slug(&s, slug).await? else {
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

pub fn router(_state: AppState) -> axum::Router<AppState> {
    use axum::routing::get;
    axum::Router::new()
        .route("/kb", get(list).post(create))
        .route("/kb/:id", get(get_one).patch(update).delete(delete_article))
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
