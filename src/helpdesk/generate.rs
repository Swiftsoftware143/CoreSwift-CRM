//! Drafting a knowledge base from what the owner already knows.
//!
//! The slow part of a help centre is not the writing, it is that nobody knows where to start. So an owner
//! describes the business in a sentence or two — what they sell, who buys it, what people ask — and the model
//! drafts articles from that. They edit, and they publish.
//!
//! Three rules hold this together, and every one of them is about the difference between a draft and a fact:
//!
//!   1. Everything generated lands as a DRAFT. Nothing is published, and nothing is shown to a customer, until
//!      a person reads it. A model that invents a refund window must not be able to change what a customer is
//!      promised.
//!   2. The prompt forbids inventing numbers, prices, legal terms and guarantees, and asks for a
//!      `[confirm: …]` marker where the truth was not given. An article that says "[confirm: how long refunds
//!      take]" is useful — it names the blank an owner has to fill.
//!   3. Without a model key the endpoint says so, in a sentence, and changes nothing. Every account is its own
//!      bill, so "no key" is a normal state, not an error to hide.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

#[derive(Debug, Deserialize, Clone)]
pub struct GenerateRequest {
    /// What the business does, in the owner's own words.
    pub about: String,
    pub industry: Option<String>,
    pub audience: Option<String>,
    /// What customers ask most, if they know.
    pub questions: Option<String>,
    /// How many articles to draft. Clamped: a hundred drafts nobody reads is worse than six they do.
    pub count: Option<i32>,
    /// The section to file them under. Created if it does not exist.
    pub section: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct GeneratedArticle {
    pub title: String,
    pub body_md: String,
}

/// The instructions the model works from.
///
/// Written as a contract rather than a wish: the JSON shape is exact, the honesty rules are explicit, and the
/// failure mode ("say nothing rather than invent") is spelled out, because a model asked politely will still
/// fill a gap if it thinks a plausible number is better than a blank.
pub fn build_prompt(req: &GenerateRequest) -> (String, String) {
    let count = req.count.unwrap_or(6).clamp(1, 12);
    let system = "You write help-centre articles for a small business. \
        Return ONLY a JSON array, no prose, no code fences. Each item is \
        {\"title\": string, \"body_md\": string}. \
        Rules you must not break: never invent prices, refund windows, legal terms, delivery times, \
        guarantees or statistics. If a fact the article needs was not supplied, write \
        \"[confirm: what to check]\" in its place instead of guessing. \
        Write in the second person, in short paragraphs, with steps as numbered lists. \
        Never mention that you are an AI."
        .to_string();

    let mut user = String::new();
    user.push_str("Business description from the owner:\n");
    user.push_str(req.about.trim());
    user.push('\n');
    if let Some(v) = req
        .industry
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        user.push_str(&format!("\nIndustry: {v}\n"));
    }
    if let Some(v) = req
        .audience
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        user.push_str(&format!("\nWho the customers are: {v}\n"));
    }
    if let Some(v) = req
        .questions
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        user.push_str(&format!("\nQuestions customers ask most:\n{v}\n"));
    }
    user.push_str(&format!(
        "\nWrite {count} articles covering the questions this business most likely has to answer. \
         Use the section name \"{}\" only as inspiration for topics, not inside the text.\n",
        req.section.as_deref().unwrap_or("Getting started")
    ));
    (system, user)
}

/// Pull articles out of whatever the model returned.
///
/// Models wrap JSON in code fences, prepend a cheerful sentence, or both. This finds the array and parses it,
/// and refuses politely when there is nothing usable — a half-parsed list would put half-written articles in
/// front of an owner who then has to work out which ones failed.
pub fn parse_articles(raw: &str) -> Result<Vec<GeneratedArticle>, String> {
    let text = raw.trim();
    let text = strip_fences(text);

    let start = text.find('[');
    let end = text.rfind(']');
    let (Some(s), Some(e)) = (start, end) else {
        return Err("the model did not return a list of articles".to_string());
    };
    if e <= s {
        return Err("the model did not return a list of articles".to_string());
    }
    let slice = &text[s..=e];

    let parsed: Vec<GeneratedArticle> = serde_json::from_str(slice)
        .map_err(|e| format!("the articles came back in a shape we could not read ({e})"))?;

    let cleaned: Vec<GeneratedArticle> = parsed
        .into_iter()
        .filter(|a| !a.title.trim().is_empty() && !a.body_md.trim().is_empty())
        .map(|a| GeneratedArticle {
            title: a.title.trim().chars().take(160).collect(),
            body_md: a.body_md.trim().to_string(),
        })
        .collect();

    if cleaned.is_empty() {
        return Err("the model returned no usable articles".to_string());
    }
    Ok(cleaned)
}

fn strip_fences(text: &str) -> &str {
    let t = text.trim();
    if let Some(rest) = t.strip_prefix("```json") {
        return rest.trim_end().trim_end_matches("```").trim();
    }
    if let Some(rest) = t.strip_prefix("```") {
        return rest.trim_end().trim_end_matches("```").trim();
    }
    t
}

/// Ask the account's own model for drafts, then file them as DRAFTS.
pub async fn generate(
    pool: &PgPool,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    req: &GenerateRequest,
    default_section: Option<&str>,
) -> Result<Vec<(Uuid, String, String)>, AppError> {
    if req.about.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Tell me a little about the business and I will draft from that.".into(),
        ));
    }

    // the account's own keys: every account is its own bill, so no key is a normal state
    let keys = crate::provider_keys::model_keys(pool, tenant_id).await;
    if keys.is_empty() {
        return Err(AppError::BadRequest(
            "Add a model key in Integration → Provider keys, and I can draft these for you. \
             Until then the knowledge base works exactly as it does now: you write the answers."
                .into(),
        ));
    }

    let (system, user) = build_prompt(req);
    let raw = crate::ai::router::route_to_llm("deepseek", &keys, &system, &user)
        .await
        .map_err(|e| {
            // BadRequest rather than Internal ON PURPOSE: the generic 500 body hides the message from the
            // person who needs it, and this is theirs to act on (a bad key, a provider outage), not a fault
            // in the app. Nothing was written either way.
            AppError::BadRequest(format!(
                "The model could not be reached, so nothing was drafted. Nothing was changed — check the key \
                 in Integration → Provider keys and try again. ({e})"
            ))
        })?;

    let articles = parse_articles(&raw).map_err(AppError::Internal)?;

    // the section these drafts belong to (created if named and missing)
    let section = req
        .section
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or(default_section);
    let category_id = match section {
        Some(name) => Some(ensure_section(pool, tenant_id, name).await?),
        None => None,
    };

    let mut created = Vec::new();
    for a in &articles {
        let slug = crate::helpdesk::kb::slugify_public(&a.title)
            .unwrap_or_else(|| format!("draft-{}", &Uuid::new_v4().to_string()[..8]));
        // a title can repeat; the address must not
        let slug = unique_slug(pool, tenant_id, &slug).await?;
        let row: Option<(Uuid, String, String)> = sqlx::query_as(
            r#"INSERT INTO helpdesk_kb_articles
                 (tenant_id, title, slug, body_md, status, visibility, category_id, author_id)
               VALUES ($1, $2, $3, $4, 'draft', 'public', $5, $6)
               RETURNING id, title, slug"#,
        )
        .bind(tenant_id)
        .bind(&a.title)
        .bind(&slug)
        .bind(&a.body_md)
        .bind(category_id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Database)?;
        if let Some(row) = row {
            created.push(row);
        }
    }

    // what was asked for, and what came back — so a bad batch is a matter of record, not a rumour
    let _ = sqlx::query(
        "INSERT INTO helpdesk_kb_generations (tenant_id, asked_by, about, industry, audience, count, drafted, provider)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'auto')",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(req.about.trim())
    .bind(req.industry.as_deref())
    .bind(req.audience.as_deref())
    .bind(created.len() as i32)
    .bind(articles.len() as i32)
    .execute(pool)
    .await;

    Ok(created)
}

async fn ensure_section(pool: &PgPool, tenant_id: Uuid, name: &str) -> Result<Uuid, AppError> {
    let slug = crate::helpdesk::kb::slugify_public(name).ok_or_else(|| {
        AppError::BadRequest("That section name does not make a usable address.".into())
    })?;
    if let Some(id) = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM helpdesk_kb_categories WHERE tenant_id = $1 AND slug = $2",
    )
    .bind(tenant_id)
    .bind(&slug)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?
    {
        return Ok(id);
    }
    sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO helpdesk_kb_categories (tenant_id, name, slug, position)
         VALUES ($1, $2, $3, 100) RETURNING id",
    )
    .bind(tenant_id)
    .bind(name.trim())
    .bind(&slug)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)
}

async fn unique_slug(pool: &PgPool, tenant_id: Uuid, base: &str) -> Result<String, AppError> {
    for n in 1..50 {
        let candidate = if n == 1 {
            base.to_string()
        } else {
            format!("{base}-{n}")
        };
        let taken: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM helpdesk_kb_articles WHERE tenant_id = $1 AND slug = $2",
        )
        .bind(tenant_id)
        .bind(&candidate)
        .fetch_optional(pool)
        .await
        .map_err(AppError::Database)?;
        if taken.is_none() {
            return Ok(candidate);
        }
    }
    Ok(format!("{base}-{}", &Uuid::new_v4().to_string()[..6]))
}

// ── HTTP ──────────────────────────────────────────────────────────────────────────────────────

use crate::auth::models::Claims;
use crate::errors::ApiResult;
use crate::AppState;
use axum::{extract::State, Extension, Json};

pub async fn handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<GenerateRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let user = Uuid::parse_str(&c.sub).ok();
    // the account's own site name is a reasonable default section name when none is given
    let site: Option<String> =
        sqlx::query_scalar("SELECT name FROM helpdesk_sites WHERE tenant_id = $1")
            .bind(tid)
            .fetch_optional(&s.db)
            .await
            .ok()
            .flatten();
    let created = generate(&s.db, tid, user, &req, site.as_deref()).await?;
    Ok(Json(serde_json::json!({
        "drafted": created.len(),
        "articles": created.iter().map(|(id, title, slug)| serde_json::json!({
            "id": id, "title": title, "slug": slug, "status": "draft"
        })).collect::<Vec<_>>(),
        // said plainly, because it is the thing that matters: nothing is live yet
        "note": "These are drafts. Read them, fix anything the model had to guess, then publish.",
    })))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    axum::Router::new().route("/kb/generate", axum::routing::post(handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_forbids_inventing_facts() {
        let (system, user) = build_prompt(&GenerateRequest {
            about: "We sell weekly newsletter sponsorship in Palm Bay.".into(),
            industry: Some("local media".into()),
            audience: Some("local businesses".into()),
            questions: Some("How much is a slot? When does it go out?".into()),
            count: Some(4),
            section: Some("Sponsorship".into()),
        });
        assert!(
            system.contains("never invent prices"),
            "the honesty rule must be explicit"
        );
        assert!(
            system.contains("[confirm:"),
            "the model needs a way to mark what it does not know"
        );
        assert!(
            system.contains("JSON array"),
            "the shape must be asked for exactly"
        );
        assert!(user.contains("Palm Bay"), "the owner's words go in");
        assert!(user.contains("4 articles"), "the count is requested");
    }

    #[test]
    fn a_fenced_answer_is_read() {
        let raw = "```json\n[{\"title\":\"How to change your card\",\"body_md\":\"1. Open Billing.\"}]\n```";
        let got = parse_articles(raw).expect("fenced json must parse");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "How to change your card");
    }

    #[test]
    fn a_chatty_answer_is_read() {
        let raw = "Sure! Here are your articles:\n[{\"title\":\"Refunds\",\"body_md\":\"Ask us.\"}]\nHope that helps.";
        assert_eq!(
            parse_articles(raw)
                .expect("prose around json must parse")
                .len(),
            1
        );
    }

    #[test]
    fn rubbish_is_refused_rather_than_half_read() {
        assert!(parse_articles("I could not do that.").is_err());
        assert!(parse_articles("[]").is_err());
        assert!(parse_articles("[{\"title\":\"\"}]").is_err());
    }
}
