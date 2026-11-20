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

/// Does this account's plan SELL self-learning?
///
/// Read from the module registry, the same table the admin panel edits, so the answer can change without a
/// redeploy and one account can be given it as an override. A tenant with no plan row resolves as allowed, which
/// is the registry's deliberate legacy tolerance for every other feature — an unplanned account is not silently
/// stripped of what it already had.
pub async fn entitled(pool: &PgPool, tenant_id: Uuid) -> Result<bool, AppError> {
    let ent = crate::module_registry::resolve(pool, tenant_id, "helpdesk_kb_learning").await?;
    Ok(ent.enabled)
}

/// The sentence a person reads when their plan does not include it. Names what IS included, so the refusal is
/// not a dead end.
fn upgrade_sentence() -> AppError {
    AppError::UpgradeRequired(
        "Learning from solved tickets is on the paid plans. Writing articles by hand, the help centre and the \
         widget search all keep working on yours."
            .into(),
    )
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
    if !entitled(pool, tenant_id).await? {
        return Ok(false);
    }

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

    let convo: Vec<Turn> = messages
        .into_iter()
        .map(|(who, body)| Turn {
            from_agent: who == "agent",
            text: body,
        })
        .collect();

    // A MODEL MAKES A BETTER ARTICLE, and the account's own key decides whether there is one. A failure here
    // (no key, a bad key, a provider outage, unreadable output) must never lose the candidate: it falls straight
    // back to the rules below, which need nothing.
    let draft =
        match distil_with_model(pool, tenant_id, &convo, &subject, contact_name.as_deref()).await {
            Some(d) => Some(d),
            None => distil(number, &subject, &convo, contact_name.as_deref()),
        };
    let Some(draft) = draft else {
        return Ok(None);
    };
    let (title, body) = (draft.title, draft.body);

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

/// Ask the account's model to write the article, or say nothing at all.
///
/// Returns `None` for every reason the caller should simply fall back: no key, the provider refused, the reply was
/// not the JSON we asked for. That is deliberate — the harvest must never fail because a key expired.
///
/// The rules given to the model are the same rules the deterministic path enforces, and the output is put through
/// the same `redact`, because a model will happily copy the customer's name back into a public article.
async fn distil_with_model(
    pool: &PgPool,
    tenant_id: Uuid,
    convo: &[Turn],
    subject: &str,
    contact_name: Option<&str>,
) -> Option<Draft> {
    let mk = crate::provider_keys::model_keys_with_source(pool, tenant_id).await;
    if mk.keys.is_empty() {
        return None;
    }

    let mut transcript = String::new();
    for t in convo {
        let who = if t.from_agent { "Support" } else { "Customer" };
        let text = clean_message(&t.text, contact_name);
        if says_nothing(&text) {
            continue;
        }
        transcript.push_str(&format!("{who}: {text}\n\n"));
        if transcript.chars().count() > 6000 {
            break;
        }
    }
    if transcript.trim().is_empty() {
        return None;
    }

    let system = "You turn a finished support conversation into one knowledge base article for the NEXT \
customer, who has the same problem. Reply with JSON only: {\"title\": \"...\", \"body\": \"...\"}.\n\nRULES:\n- Use ONLY facts present in the conversation. Never invent prices, refund windows, delivery times, legal terms, \
phone numbers, addresses or statistics.\n- If a fact a reader would need is missing, write [confirm: what to check] in its place.\n- Drop greetings, pleasantries, apologies, \"let me know if that helps\", signatures, and every attempt that did \
not resolve the issue.\n- Write general guidance for anyone with this problem, not the story of one customer: say \"your account\", \
never the person's name or details.\n- The title is the question in the customer's own words, under 90 characters, no marketing.\n- The body is markdown, 80 to 200 words, plain English, a short numbered list when there are steps.";

    let user = format!(
        "Ticket subject: {subject}\n\nConversation:\n{transcript}\nWrite the article as JSON."
    );

    let raw = match crate::ai::router::route_to_llm_tracked(
        pool,
        tenant_id,
        "ticket_learn",
        "deepseek",
        &mk,
        system,
        &user,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "the model could not tidy a solved ticket; falling back to the rules");
            return None;
        }
    };

    let (title, body) = parse_article_json(&raw)?;
    if title.trim().is_empty() || body.trim().is_empty() {
        return None;
    }
    let title = clean_title(subject, &title);
    let body = clean_message(&body, contact_name);
    if body.chars().count() < 40 {
        return None;
    }

    Some(Draft {
        title,
        body: format!(
            "> Drafted from a real conversation (tidied by your model). Read it before publishing: it should \
             read as general guidance, not as one customer's case.\n\n{body}\n\n[confirm: anything specific to \
             that one customer, and anything the model had to guess.]"
        ),
        dropped: 0,
    })
}

/// Read the JSON the model was asked for, tolerating a code fence around it — the shape every provider invents
/// when it feels helpful.
fn parse_article_json(raw: &str) -> Option<(String, String)> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&raw[start..=end]).ok()?;
    let title = v.get("title")?.as_str()?.to_string();
    let body = v.get("body")?.as_str()?.to_string();
    Some((title, body))
}

/// One message in a conversation, cleaned and judged.
pub struct Turn {
    pub from_agent: bool,
    pub text: String,
}

/// What a person will read after somebody has stripped the conversation down to its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub body: String,
    /// how many messages of the conversation are not repeated in the article — the honest measure of how much
    /// was left out, whether it was noise or an attempt that did not resolve the issue
    pub dropped: usize,
}

/// Words that carry no knowledge on their own. Used to decide whether a message says anything at all, never to
/// rewrite a sentence somebody wrote.
const SMALL_WORDS: &[&str] = &[
    "the", "and", "for", "you", "your", "with", "was", "were", "are", "is", "it", "its", "my",
    "me", "we", "our", "have", "has", "had", "will", "just", "now", "but", "not", "this", "that",
    "these", "those", "from", "can", "could", "would", "should", "please", "im", "ive", "dont",
    "didnt", "doesnt", "thanks", "thank", "ok", "okay", "great", "perfect", "good", "fine",
    "worked", "work", "fixed", "sorted", "solved", "done", "yes", "yeah", "no", "nope", "sure",
    "hi", "hello", "hey", "cheers", "awesome", "cool", "noted", "np", "there", "here", "all",
    "any", "update", "news", "bump", "got", "get", "much", "very", "again", "still", "help", "let",
    "know",
];

/// Turn one conversation into a draft article.
///
/// A real thread is mostly not knowledge: greetings, "thanks", "any update?", and a run of attempts that did not
/// work before the one that did. Repeating all of it would produce an article nobody would publish. So the
/// conversation is distilled rather than quoted:
///
///   * each message is cleaned — quoted history, signatures and greeting/closing lines removed;
///   * messages that say nothing (acknowledgements, "any update", one-word replies) are dropped entirely;
///   * the QUESTION is the customer's first message that says something;
///   * the ANSWER is the team's last message that says something, because that is the one that ended the thread
///     — with the one before it only when the last is very short, since "glad that's sorted, refunded the fee"
///     means nothing without the sentence before it.
///
/// With no model key this is all the distillation there is, and it is honest about it: the draft states how many
/// messages were left out and asks the reviewer to check the rest.
pub fn distil(
    number: i32,
    subject: &str,
    convo: &[Turn],
    contact_name: Option<&str>,
) -> Option<Draft> {
    let cleaned: Vec<Turn> = convo
        .iter()
        .map(|t| Turn {
            from_agent: t.from_agent,
            text: clean_message(&t.text, contact_name),
        })
        .collect();

    let substantive: Vec<&Turn> = cleaned.iter().filter(|t| !says_nothing(&t.text)).collect();

    let question = substantive
        .iter()
        .find(|t| !t.from_agent)
        .map(|t| t.text.clone())?;
    let answers: Vec<&&Turn> = substantive.iter().filter(|t| t.from_agent).collect();
    if answers.is_empty() {
        return None;
    }

    let last = answers[answers.len() - 1];
    // 60, not 160: a failed attempt was being pulled in beside a complete answer simply because the
    // answer was a short paragraph. Only a genuine one-liner needs the sentence before it.
    let joined = last.text.chars().count() < 60 && answers.len() >= 2;
    let answer = if joined {
        format!("{}\n\n{}", answers[answers.len() - 2].text, last.text)
    } else {
        last.text.clone()
    };
    let joined = last.text.chars().count() < 60 && answers.len() >= 2;
    let answer = cap(&answer, 2000);
    // what the reader is not seeing: everything except the question and the answer that closes it
    let used = 1 + if joined { 2 } else { 1 };
    let dropped = cleaned.len().saturating_sub(used);

    let title = clean_title(subject, &question);

    let mut body = String::new();
    body.push_str(&format!(
        "> Drafted from a real conversation (ticket CS-{number}). Read it before publishing: it should read as \
         general guidance, not as one customer's case.\n\n"
    ));
    body.push_str("**The question**\n\n");
    body.push_str(&question);
    body.push_str("\n\n**What resolved it**\n\n");
    body.push_str(&answer);
    if dropped > 0 {
        body.push_str(&format!(
            "\n\n*This conversation ran to {} messages. The {} that were greetings, acknowledgements, or \
             attempts that did not resolve it are not repeated here — read the ticket if you want them.*",
            cleaned.len(),
            dropped
        ));
    }
    body.push_str(
        "\n\n[confirm: anything specific to that one customer — dates, amounts, account details — so this reads \
         as an answer to the question, not to the person.]",
    );
    Some(Draft {
        title,
        body,
        dropped,
    })
}

/// Strip what is not the message: quoted history, a signature, and the greeting or closing line around the content.
///
/// Anything inside a reply chain is somebody else's words already counted as their own message, so repeating it
/// would double the same sentence in the article.
fn clean_message(raw: &str, contact_name: Option<&str>) -> String {
    let mut kept: Vec<&str> = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if t.is_empty() {
            kept.push("");
            continue;
        }
        // quoted history: ">", "On 12 Nov, Dana wrote:", "-----Original Message-----", a forwarded header
        if t.starts_with('>')
            || t.starts_with("On ") && t.contains(" wrote:")
            || t.starts_with("-----Original")
            || t.starts_with("From: ")
            || t.starts_with("Sent: ")
            || t.starts_with("____")
            || t.starts_with("-- ")
            || t.starts_with("Sent from my")
            || t.starts_with("Get Outlook")
            || t.eq_ignore_ascii_case("thanks,")
            || t.eq_ignore_ascii_case("thank you,")
            || t.eq_ignore_ascii_case("regards,")
            || t.eq_ignore_ascii_case("best,")
            || t.eq_ignore_ascii_case("best regards,")
            || t.eq_ignore_ascii_case("cheers,")
            || t.to_ascii_lowercase().starts_with("on support we wrote")
        {
            // the line itself goes, and so does everything under a signature marker
            if t.starts_with("-- ") || t.starts_with("Sent from my") {
                break;
            }
            continue;
        }
        kept.push(line);
    }

    let mut text = kept.join("\n");
    text = drop_politeness(&text);
    text = redact(&text, contact_name);
    collapse_blank_lines(&text)
}

/// Greetings and closing pleasantries, removed as whole sentences because they carry no knowledge and every
/// article is better without "Hope this helps!".
fn drop_politeness(text: &str) -> String {
    const NOISE: &[&str] = &[
        "hope this helps",
        "hope that helps",
        "let me know if",
        "let me know how it goes",
        "feel free to reach out",
        "dont hesitate to",
        "have a great",
        "have a good",
        "thanks for reaching out",
        "thank you for reaching out",
        "thanks for getting in touch",
        "happy to help",
        "sorry for the delay",
        "apologies for the delay",
        "sorry for the trouble",
        "kind regards",
        "best regards",
    ];
    let mut out = String::with_capacity(text.len());
    for sentence in split_sentences(text) {
        let low = sentence.to_ascii_lowercase();
        if NOISE.iter().any(|n| low.contains(n)) {
            continue;
        }
        out.push_str(&sentence);
    }
    out.trim().to_string()
}

/// Sentences, keeping their punctuation. Not a parser: enough to drop a whole pleasantry without touching the
/// sentence next to it.
fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in text.chars() {
        cur.push(ch);
        if matches!(ch, '.' | '!' | '?') {
            let next_is_space = true;
            if next_is_space {
                out.push(std::mem::take(&mut cur));
            }
        } else if ch == '\n' && cur.trim().is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn collapse_blank_lines(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut blank = 0;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push(line);
    }
    out.join("\n").trim().to_string()
}

/// Does this message carry any knowledge at all?
///
/// Deliberately conservative: a message is dropped only when it is short AND every word in it is a pleasantry, a
/// pronoun or a filler. A genuine one-line answer ("Refunds take five days.") keeps its nouns and survives.
fn says_nothing(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    if t.chars().count() < 12 {
        return true;
    }
    let words: Vec<String> = t
        .split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    if words.len() > 10 {
        return false;
    }
    let meaningful = words
        .iter()
        .filter(|w| w.len() > 2 && !SMALL_WORDS.contains(&w.as_str()))
        .count();
    meaningful == 0
}

/// Cut a very long answer at a sentence boundary and say so, rather than shipping an article nobody scrolls.
fn cap(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    let at = cut.rfind(['.', '!', '?']).unwrap_or(limit);
    let head: String = cut.chars().take(at + 1).collect();
    format!("{head}\n\n[confirm: the answer was longer than this — the rest is in the ticket.]")
}

pub fn compose_draft(
    number: i32,
    subject: &str,
    question: &str,
    answers: &[String],
    contact_name: Option<&str>,
) -> (String, String) {
    let mut convo = vec![Turn {
        from_agent: false,
        text: question.to_string(),
    }];
    for a in answers {
        convo.push(Turn {
            from_agent: true,
            text: a.clone(),
        });
    }
    match distil(number, subject, &convo, contact_name) {
        Some(d) => (d.title, d.body),
        None => (clean_title(subject, question), String::new()),
    }
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
    if !entitled(&s.db, tid).await? {
        return Err(upgrade_sentence());
    }
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
    if !entitled(&s.db, tid).await? {
        return Err(upgrade_sentence());
    }
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

    fn turn(from_agent: bool, text: &str) -> Turn {
        Turn {
            from_agent,
            text: text.to_string(),
        }
    }

    /// The case David asked about: a thread that went back and forth for a while must not become an article
    /// full of "thanks" and failed attempts.
    fn messy_thread() -> Vec<Turn> {
        vec![
            turn(false, "Hi there"),
            turn(
                false,
                "How do I export my contacts? I need them in a spreadsheet.",
            ),
            turn(true, "Hi Dana! Thanks for reaching out, hope you are well."),
            turn(
                true,
                "Can you try clearing your browser cache and logging back in?",
            ),
            turn(false, "ok trying now"),
            turn(false, "did that, still nothing"),
            turn(true, "Let me know if that helps!"),
            turn(
                true,
                "Open Lists, choose the list and press Export. You get a CSV you can open in any \
spreadsheet. Each list exports separately, so pick the one you need.",
            ),
            turn(false, "that worked, thanks!"),
            turn(false, "Also, is there a limit?"),
            turn(true, "Glad I could help — have a great week!"),
        ]
    }

    #[test]
    fn a_long_thread_is_distilled_not_quoted() {
        let d = distil(
            42,
            "How do I export my contacts? 9377f8",
            &messy_thread(),
            Some("Dana Learner"),
        )
        .unwrap();
        // the answer that resolved it, and nothing about clearing a cache
        assert!(d.body.contains("Open Lists"), "the real answer is kept");
        assert!(
            !d.body
                .to_lowercase()
                .contains("clearing your browser cache"),
            "a failed attempt is gone"
        );
        assert!(!d.body.contains("trying now"), "the back and forth is gone");
        assert!(
            !d.body.to_lowercase().contains("let me know if that helps"),
            "pleasantries are gone"
        );
        assert!(
            !d.body.to_lowercase().contains("have a great week"),
            "sign-off is gone"
        );
        assert!(!d.body.contains("Hi there"), "greeting messages are gone");
        assert!(!d.body.contains("Dana"), "the customer's name is gone");
        assert!(
            d.dropped >= 5,
            "it says how much was left out: {} dropped",
            d.dropped
        );
        assert!(d.body.contains("CS-42"));
    }

    #[test]
    fn the_question_is_the_first_message_that_says_something() {
        let d = distil(1, "", &messy_thread(), None).unwrap();
        assert!(
            d.title.starts_with("How do I export my contacts"),
            "title: {}",
            d.title
        );
        assert!(
            d.body.contains("**The question**\n\nHow do I export"),
            "the greeting is not the question"
        );
    }

    #[test]
    fn a_one_word_reply_is_noise_not_an_answer() {
        // "Refunded." adds nothing a reader can use; the substance is the sentence it followed
        let convo = vec![
            turn(false, "I was charged twice for the same invoice this month."),
            turn(
                true,
                "I can see the duplicate. I have voided the second invoice so you are not charged twice; it \
will show as void in your billing history within a day.",
            ),
            turn(false, "thanks"),
            turn(true, "Refunded."),
        ];
        let d = distil(9, "Charged twice", &convo, None).unwrap();
        assert!(
            d.body.contains("voided the second invoice"),
            "the substance is kept: {}",
            d.body
        );
        assert!(
            !d.body.contains("Refunded."),
            "the one-word reply is not dressed up as the answer"
        );
        assert!(
            !d.body.contains("thanks"),
            "and the customer's pleasantry is not quoted"
        );
    }

    #[test]
    fn a_quoted_reply_chain_is_not_repeated() {
        let convo = vec![
            turn(false, "How do I change my plan?"),
            turn(true, "Settings, then Billing, then Change plan.\n\n> How do I change my plan?\n> Sent from my \
iPhone\n\nLet me know if that helps!"),
        ];
        let d = distil(3, "Change plan", &convo, None).unwrap();
        assert_eq!(
            d.body.matches("How do I change my plan?").count(),
            1,
            "asked once, not twice: {}",
            d.body
        );
        assert!(!d.body.contains("Sent from my"), "the signature is gone");
        assert!(!d.body.contains("Let me know if"), "the pleasantry is gone");
    }

    #[test]
    fn nothing_that_identifies_one_customer_survives() {
        let convo = vec![
            turn(
                false,
                "This is Dana Whitfield, my email is dana.w@acme.com and the card fails.",
            ),
            turn(
                true,
                "Thanks Dana Whitfield, I fixed the card on dana.w@acme.com.",
            ),
        ];
        let d = distil(7, "Card payment failing", &convo, Some("Dana Whitfield")).unwrap();
        assert!(!d.body.contains("dana.w@acme.com"));
        assert!(!d.body.contains("Dana Whitfield"));
        assert!(d.body.contains("[email removed]"));
    }

    #[test]
    fn a_question_mark_stays_on_a_question() {
        let d = distil(
            4,
            "",
            &[
                turn(false, "how do I change my plan?"),
                turn(true, "Settings, then Billing."),
            ],
            None,
        )
        .unwrap();
        assert_eq!(d.title, "how do I change my plan?");
    }

    #[test]
    fn a_very_long_answer_is_capped_and_says_so() {
        let long = "This is a sentence. ".repeat(400);
        let d = distil(
            5,
            "Long answer",
            &[turn(false, "what happens?"), turn(true, &long)],
            None,
        )
        .unwrap();
        assert!(
            d.body.chars().count() < 2600,
            "capped: {}",
            d.body.chars().count()
        );
        assert!(d.body.contains("the answer was longer than this"));
    }

    #[test]
    fn a_conversation_with_no_answer_makes_no_article() {
        let convo = vec![turn(false, "hello"), turn(true, "thanks")];
        assert!(distil(6, "Nothing", &convo, None).is_none());
    }

    #[test]
    fn the_model_json_is_read_even_inside_a_code_fence() {
        let raw = "```json\n{\"title\": \"How to export\", \"body\": \"Press Export.\"}\n```";
        let (t, b) = parse_article_json(raw).unwrap();
        assert_eq!(t, "How to export");
        assert_eq!(b, "Press Export.");
        assert!(parse_article_json("I cannot help with that").is_none());
    }
}
