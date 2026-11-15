//! Learning from the conversations that already happened.
//!
//! A solved ticket is the most honest source of help content there is: a real person asked a real question and
//! somebody answered it well enough that they stopped. Today that answer dies in a closed ticket and the next
//! customer asks the same thing.
//!
//! The loop is deliberately slow at one point and fast at another:
//!
//!   * FAST — the moment a ticket is solved it is queued, with no work done in the request path.
//!   * SLOW — a background pass turns the queue into DRAFTS. Nothing is published. A person reads it, edits it,
//!     and publishes it, or discards it.
//!
//! That pause is the whole safety argument. An automatically-written article reaches a customer only after a
//! human has read the sentence a customer will be told. The drafting itself is deterministic when no model key
//! exists: the customer's own question becomes the title, the team's answers become the body, and a warning line
//! says where it came from, so a small desk gets the benefit without buying a model.
//!
//! Two things are stripped before anything is stored, because this text may one day be public: email addresses,
//! and the customer's name. The article is written for the NEXT person, not about the last one.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Candidate {
    pub id: Uuid,
    pub ticket_id: Uuid,
    pub ticket_number: Option<i32>,
    pub ticket_subject: String,
    pub article_id: Option<Uuid>,
    pub article_title: Option<String>,
    pub article_slug: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Queue a solved ticket, if it is worth queueing at all.
///
/// Called from the ticket update path, so it does no work beyond one insert: a ticket with no agent reply has
/// nothing to teach, and an account that switched suggestions off gets nothing queued.
pub async fn enqueue_on_solve(
    pool: &PgPool,
    tenant_id: Uuid,
    ticket_id: Uuid,
) -> Result<bool, AppError> {
    let enabled: Option<bool> =
        sqlx::query_scalar("SELECT kb_autodraft FROM helpdesk_sites WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_optional(pool)
            .await
            .map_err(AppError::Database)?;
    if enabled == Some(false) {
        return Ok(false);
    }

    // only a conversation somebody actually answered can teach anybody
    let answered: Option<bool> = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM ticket_messages
                        WHERE ticket_id = $1 AND sender_type = 'agent' AND kind = 'reply')",
    )
    .bind(ticket_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    if answered != Some(true) {
        return Ok(false);
    }

    let done = sqlx::query(
        "INSERT INTO helpdesk_kb_learning (tenant_id, ticket_id, status)
         VALUES ($1, $2, 'queued')
         ON CONFLICT (ticket_id) DO NOTHING",
    )
    .bind(tenant_id)
    .bind(ticket_id)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;

    Ok(done.rows_affected() > 0)
}

/// What one pass over the queue did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LearnPass {
    pub drafted: usize,
    pub skipped: usize,
}

/// Turn queued tickets into draft articles.
pub async fn run_once(pool: &PgPool) -> LearnPass {
    let queued: Vec<(Uuid, Uuid, Uuid)> = match sqlx::query_as(
        "SELECT id, tenant_id, ticket_id FROM helpdesk_kb_learning
          WHERE status = 'queued' ORDER BY created_at LIMIT 25",
    )
    .fetch_all(pool)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "could not read the knowledge queue");
            return LearnPass::default();
        }
    };

    let mut pass = LearnPass::default();
    for (ledger_id, tenant_id, ticket_id) in queued {
        match draft_one(pool, tenant_id, ticket_id).await {
            Ok(Some(article_id)) => {
                let _ = sqlx::query(
                    "UPDATE helpdesk_kb_learning
                        SET status = 'drafted', article_id = $2, resolved_at = NOW()
                      WHERE id = $1",
                )
                .bind(ledger_id)
                .bind(article_id)
                .execute(pool)
                .await;
                pass.drafted += 1;
            }
            Ok(None) => {
                let _ = sqlx::query(
                    "UPDATE helpdesk_kb_learning
                        SET status = 'skipped', reason = 'there was nothing to learn from', resolved_at = NOW()
                      WHERE id = $1",
                )
                .bind(ledger_id)
                .execute(pool)
                .await;
                pass.skipped += 1;
            }
            Err(e) => {
                tracing::warn!(ledger = %ledger_id, error = %e, "could not draft from a solved ticket");
                let _ = sqlx::query(
                    "UPDATE helpdesk_kb_learning
                        SET status = 'skipped', reason = $2, resolved_at = NOW()
                      WHERE id = $1",
                )
                .bind(ledger_id)
                .bind(format!("{e}"))
                .execute(pool)
                .await;
                pass.skipped += 1;
            }
        }
    }
    if pass.drafted > 0 || pass.skipped > 0 {
        tracing::info!(
            drafted = pass.drafted,
            skipped = pass.skipped,
            "knowledge pass finished"
        );
    }
    pass
}

async fn draft_one(
    pool: &PgPool,
    tenant_id: Uuid,
    ticket_id: Uuid,
) -> Result<Option<Uuid>, AppError> {
    let head: Option<(i32, String, Option<String>)> = sqlx::query_as(
        "SELECT number, subject, contact_name FROM tickets WHERE id = $1 AND tenant_id = $2",
    )
    .bind(ticket_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    let Some((number, subject, contact_name)) = head else {
        return Ok(None);
    };

    let messages: Vec<(String, String)> = sqlx::query_as(
        "SELECT sender_type, COALESCE(NULLIF(body_text, ''), message)
           FROM ticket_messages
          WHERE ticket_id = $1 AND kind = 'reply'
          ORDER BY created_at",
    )
    .bind(ticket_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    let question = messages
        .iter()
        .find(|(who, _)| who == "contact")
        .map(|(_, body)| body.clone())
        .unwrap_or_default();
    let answers: Vec<String> = messages
        .iter()
        .filter(|(who, _)| who == "agent")
        .map(|(_, body)| body.clone())
        .collect();

    if question.trim().is_empty() || answers.is_empty() {
        return Ok(None);
    }

    let (title, body) = compose_draft(
        number,
        &subject,
        &question,
        &answers,
        contact_name.as_deref(),
    );

    // a title that already exists as an article means somebody has already written this up
    let slug = crate::helpdesk::kb::slugify_public(&title)
        .unwrap_or_else(|| format!("from-ticket-{number}"));
    let slug = match unique_slug(pool, tenant_id, &slug).await? {
        Some(s) => s,
        None => return Ok(None),
    };

    let article_id: Option<Uuid> = sqlx::query_scalar(
        r#"INSERT INTO helpdesk_kb_articles
             (tenant_id, title, slug, body_md, status, visibility, source_ticket_id, auto_drafted)
           VALUES ($1, $2, $3, $4, 'draft', 'public', $5, true)
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(&title)
    .bind(&slug)
    .bind(&body)
    .bind(ticket_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;

    Ok(article_id)
}

/// The words of the draft.
///
/// Deterministic on purpose: no model is required for the loop to work, and an owner who has not bought a key
/// still gets a useful starting point. The customer's own question becomes the title because that is how the
/// next customer will search; the team's replies become the answer.
pub fn compose_draft(
    number: i32,
    subject: &str,
    question: &str,
    answers: &[String],
    contact_name: Option<&str>,
) -> (String, String) {
    let title = clean_title(subject, question);
    let mut body = String::new();
    body.push_str(&format!(
        "> Drafted from a real conversation (ticket CS-{number}). Read it before publishing: it should read as \
         general guidance, not as one customer's case.\n\n"
    ));
    body.push_str("**The question**\n\n");
    body.push_str(&redact(question, contact_name));
    body.push_str("\n\n**What we told them**\n\n");
    for (i, a) in answers.iter().enumerate() {
        if i > 0 {
            body.push_str("\n\n");
        }
        body.push_str(&redact(a, contact_name));
    }
    body.push_str("\n\n[confirm: anything specific to that one customer — dates, amounts, account details — so this reads as an answer to the question, not to the person.]");
    (title, body)
}

fn clean_title(subject: &str, question: &str) -> String {
    let base = {
        let s = subject.trim();
        if s.is_empty() || s == "(no subject)" {
            question.trim()
        } else {
            s
        }
    };
    let mut t: String = base.chars().take(90).collect();
    if let Some(cut) = t.find('\n') {
        t.truncate(cut);
    }
    // a question mark is part of the question, and the title IS the customer's question: only the trailing
    // punctuation that reads as a full stop is removed
    let t = t.trim().trim_end_matches(['.', ',']).to_string();
    if t.is_empty() {
        "An answer from a support conversation".to_string()
    } else {
        t
    }
}

/// Take out anything that belongs to one person: their address, and their name if we were told it.
///
/// This text may become public, so the bar is "would this embarrass us if the next customer read it".
fn redact(text: &str, contact_name: Option<&str>) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(char::is_whitespace) {
        let core = token.trim();
        if core.contains('@') && core.contains('.') {
            out.push_str("[email removed]");
            if token.ends_with(' ') || token.ends_with('\n') {
                out.push(' ');
            }
            continue;
        }
        out.push_str(token);
    }
    if let Some(name) = contact_name.map(str::trim).filter(|n| n.len() > 2) {
        // the whole name, and its first word: "Dana Whitfield" and "Dana" both identify somebody
        out = out.replace(name, "the customer");
        if let Some(first) = name.split_whitespace().next() {
            if first.len() > 2 {
                out = out.replace(first, "the customer");
            }
        }
    }
    out.trim().to_string()
}

/// `None` when an article with this address already exists — a duplicate title is a sign the answer is already
/// written, and the operator should edit that one rather than publish a second copy.
async fn unique_slug(
    pool: &PgPool,
    tenant_id: Uuid,
    base: &str,
) -> Result<Option<String>, AppError> {
    let taken: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM helpdesk_kb_articles WHERE tenant_id = $1 AND slug = $2",
    )
    .bind(tenant_id)
    .bind(base)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    if taken.is_some() {
        return Ok(None);
    }
    Ok(Some(base.to_string()))
}

/// Everything waiting for a person: drafted, publishable, and reviewable in one place.
pub async fn candidates(pool: &PgPool, tenant_id: Uuid) -> Result<Vec<Candidate>, AppError> {
    sqlx::query_as(
        r#"SELECT l.id, l.ticket_id, t.number AS ticket_number, t.subject AS ticket_subject,
                  l.article_id, a.title AS article_title, a.slug AS article_slug,
                  l.status, l.reason, l.created_at
             FROM helpdesk_kb_learning l
             JOIN tickets t ON t.id = l.ticket_id
             LEFT JOIN helpdesk_kb_articles a ON a.id = l.article_id
            WHERE l.tenant_id = $1
              AND l.status IN ('queued', 'drafted')
            ORDER BY l.created_at DESC
            LIMIT 50"#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)
}

/// Record that a candidate was thrown away, so the ledger says so rather than forgetting it happened.
pub async fn discard(pool: &PgPool, tenant_id: Uuid, ledger_id: Uuid) -> Result<(), AppError> {
    let row: Option<(Option<Uuid>,)> = sqlx::query_as(
        "SELECT article_id FROM helpdesk_kb_learning WHERE id = $1 AND tenant_id = $2",
    )
    .bind(ledger_id)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .map_err(AppError::Database)?;
    let Some((article_id,)) = row else {
        return Err(AppError::NotFound(
            "That suggestion is not in your account.".into(),
        ));
    };

    if let Some(a) = article_id {
        // the draft goes too: a discarded suggestion that stays in the article list is clutter, and its
        // status is 'draft' so nobody has read it to a customer
        let _ = sqlx::query("DELETE FROM helpdesk_kb_articles WHERE id = $1 AND tenant_id = $2 AND status = 'draft' AND auto_drafted")
            .bind(a)
            .bind(tenant_id)
            .execute(pool)
            .await;
    }
    sqlx::query(
        "UPDATE helpdesk_kb_learning SET status = 'discarded', resolved_at = NOW()
          WHERE id = $1 AND tenant_id = $2",
    )
    .bind(ledger_id)
    .bind(tenant_id)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

/// Mark a candidate published once its article is live — the ledger's own record of a loop that paid off.
pub async fn mark_published(
    pool: &PgPool,
    tenant_id: Uuid,
    article_id: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE helpdesk_kb_learning SET status = 'published', resolved_at = NOW()
          WHERE tenant_id = $1 AND article_id = $2 AND status = 'drafted'",
    )
    .bind(tenant_id)
    .bind(article_id)
    .execute(pool)
    .await
    .map_err(AppError::Database)?;
    Ok(())
}

// ── HTTP ──────────────────────────────────────────────────────────────────────────────────────

use crate::auth::models::Claims;
use crate::errors::ApiResult;
use crate::AppState;
use axum::{extract::Path, extract::State, Extension, Json};

pub async fn list_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = candidates(&s.db, tid).await?;
    let stats: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status IN ('queued','drafted')),
                count(*) FILTER (WHERE status = 'published'),
                count(*) FILTER (WHERE status = 'discarded')
           FROM helpdesk_kb_learning WHERE tenant_id = $1",
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;
    let (waiting, published, discarded) = stats.unwrap_or((0, 0, 0));
    Ok(Json(serde_json::json!({
        "candidates": rows,
        "count": rows.len(),
        "waiting": waiting,
        "published": published,
        "discarded": discarded,
    })))
}

pub async fn discard_handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    discard(&s.db, tid, id).await?;
    Ok(Json(serde_json::json!({ "discarded": true })))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route("/kb/learning", get(list_handler))
        .route("/kb/learning/:id/discard", post(discard_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_says_where_it_came_from_and_what_to_check() {
        let (title, body) = compose_draft(
            42,
            "How do I export my contacts?",
            "Hi, I need all my contacts in a spreadsheet.",
            &["Open Lists, press Export, and you get a CSV.".to_string()],
            Some("Dana Whitfield"),
        );
        assert_eq!(title, "How do I export my contacts?");
        assert!(body.contains("CS-42"), "the source ticket is named");
        assert!(
            body.contains("[confirm:"),
            "the owner is told what to check"
        );
        assert!(body.contains("Open Lists"), "the answer is kept");
    }

    #[test]
    fn nothing_that_identifies_one_customer_survives() {
        let (_, body) = compose_draft(
            7,
            "Card payment failing",
            "This is Dana Whitfield, my email is dana.w@acme.com and it fails.",
            &["Thanks Dana Whitfield, I have fixed the card on dana.w@acme.com.".to_string()],
            Some("Dana Whitfield"),
        );
        assert!(
            !body.contains("dana.w@acme.com"),
            "an address must never reach a public article"
        );
        assert!(!body.contains("Dana Whitfield"), "the name must not either");
        assert!(body.contains("[email removed]"));
        assert!(body.contains("the customer"));
    }

    #[test]
    fn a_subject_less_ticket_falls_back_to_the_question() {
        let (title, _) = compose_draft(
            3,
            "(no subject)",
            "how do I change my plan?\n\nthanks",
            &["Settings, then Billing.".to_string()],
            None,
        );
        assert_eq!(title, "how do I change my plan?");
    }

    #[test]
    fn a_nameless_question_still_makes_a_title() {
        let (title, _) = compose_draft(1, "", "", &["An answer.".to_string()], None);
        assert_eq!(title, "An answer from a support conversation");
    }
}
