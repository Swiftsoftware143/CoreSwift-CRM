//! ACCOUNT MATCHING — the engine.
//!
//! One implementation of the question *"does this inbound identity already belong to a contact or a
//! company of this tenant?"*. Before this module the answer was written out inline, four times, in
//! four different spellings:
//!
//! * `contacts::handlers::create`      — `SELECT * FROM contacts WHERE tenant_id=$1 AND email=$2`
//! * `inbound::handlers`               — `SELECT id  FROM contacts WHERE tenant_id=$1 AND email=$2`
//! * `external_api`                    — `SELECT id  FROM contacts WHERE tenant_id=$1 AND email=$2`
//! * `contacts_internal`               — `SELECT id  FROM contacts WHERE tenant_id=$1 AND email=$2`
//!
//! (plus two more in `webhooks::cross_app_tag_sync`, which compare `LOWER(email)`.) Each copy is a
//! chance for the "who is this?" rule to drift from its siblings, and none of them can answer the
//! two questions the pipeline actually asks: *which* of my records does this identity match, and
//! which of my records look like duplicates of one another.
//!
//! This module answers both, tenant-scoped, in one place:
//!
//! * [`resolve`] — cascade match against the caller's OWN tenant. Highest-confidence arm first:
//!   normalized email → normalized phone → first+last name with a matching company. The tenant id
//!   is a bind on every statement; there is no cross-tenant read anywhere in this file and no
//!   statement that can see another tenant's rows.
//! * [`duplicate_groups`] — the pipeline-cleanliness report: contacts of one tenant that share a
//!   normalized phone number or a normalized name. Email is deliberately NOT a key here: the live
//!   schema carries `idx_contacts_tenant_email` (UNIQUE on `(tenant_id, email) WHERE email IS NOT
//!   NULL`), so an email collision is impossible inside a tenant and a report keyed on it could
//!   only ever return the empty set.
//! * [`find_contact_by_email_exact`] — the pre-existing lookup the contact-create path depends on,
//!   moved here verbatim (same SQL, same `LIMIT 1`, deliberately NOT filtered on `is_active`, which
//!   is what lets an existing inactive row be merged into rather than duplicated) so that the module
//!   is the single owner of identity lookup rather than a new copy of it.
//!
//! ## Tenancy
//!
//! `tenant_id` comes from the caller's JWT (`Claims::aid`) and is bound into every statement. The
//! external callers of [`find_contact_by_email_exact`] pass the tenant they already resolved a
//! credential to; the HTTP surface passes the tenant of the session. A match can therefore never be
//! a foreign row: `contacts`/`companies` reads are ALWAYS `WHERE tenant_id = $1 AND …`.
//!
//! ## Plan gating
//!
//! The module has its own registry row (`modules.key = 'account_match'`, migration 113) and its own
//! plan assignment row per plan, exactly like every other module — so `crate::features::gate_mw`
//! refuses the module's HTTP surface with 402 for a tenant whose plan does not grant it. The
//! functions in THIS file are plain library helpers and carry no gate of their own: the contact
//! create path they serve is gated by its own ceiling (`limit_max_contacts`) and must not start
//! refusing writes because a tenant's plan changed.

use crate::contacts::models::Contact;
use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

/// A candidate inbound identity — a form post, a CSV row, a spoke app's capture, a webhook payload.
/// Every field is optional; a caller sends what it has and the cascade uses what it can.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct Identity {
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    /// Free-text employer. Note the display-vs-reference rule this repo already settled
    /// (t_fbb30c16): `contacts.company` is the employer the product renders; `companies.name` is the
    /// company record. This module matches on the free-text spelling first and only then resolves a
    /// `companies` row.
    pub company: Option<String>,
}

fn present(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

impl Identity {
    /// Does this identity carry anything at all that could be matched on?
    pub fn has_identifier(&self) -> bool {
        present(&self.email).is_some()
            || normalize_phone(present(&self.phone).unwrap_or("")).is_some()
            || (present(&self.first_name).is_some() && present(&self.last_name).is_some())
    }
}

/// The comparison key of an email: trimmed, lowercased. `None` for empty/blank input.
///
/// Callers that need the schema's OWN equality (the unique index is on the raw column) use
/// [`find_contact_by_email_exact`] instead — this function is for *matching*, where
/// `Lead@Example.COM` and `lead@example.com` are the same person.
pub fn normalize_email(raw: &str) -> Option<String> {
    let t = raw.trim().to_lowercase();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// The comparison key of a phone number: its digits. `+1 (305) 555-0100` and `13055550100` are the
/// same number. A value with fewer than 7 digits is not a phone number (`None`), which keeps a
/// short extension or a stray country code out of the match set.
pub fn normalize_phone(raw: &str) -> Option<String> {
    let digits: String = raw.chars().filter(char::is_ascii_digit).collect();
    if digits.len() < 7 {
        None
    } else {
        Some(digits)
    }
}

/// The domain part of an email, lowercased, when it looks like a domain.
fn email_domain(email: &str) -> Option<String> {
    let at = email.rfind('@')?;
    let d = email[at + 1..].trim().to_lowercase();
    if d.contains('.') && !d.starts_with('.') && !d.ends_with('.') {
        Some(d)
    } else {
        None
    }
}

/// A matched contact, in the shape the matcher and the SPA both want — no `SELECT *`, so a future
/// column cannot break the decode (the `is_active NULL` class of bug, t_97b46a98).
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct ContactMatch {
    pub id: Uuid,
    pub first_name: String,
    pub last_name: String,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub company: Option<String>,
}

/// A matched COMPANY record (the B2B "account"). `companies.name` is the record; the free-text
/// `contact.company` is not.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct CompanyMatch {
    pub id: Uuid,
    pub name: String,
    pub domain: Option<String>,
}

/// The verdict of one identity lookup.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Match {
    /// True when a contact of this tenant was matched.
    pub matched: bool,
    /// Which arm answered: `email` | `phone` | `name_company` | `none`.
    pub reason: &'static str,
    /// 1.0 email, 0.8 phone, 0.6 name+company, 0.0 no match — the ranking the cascade applies, so a
    /// caller can decide for itself whether to auto-merge or to ask a human.
    pub confidence: f64,
    pub contact: Option<ContactMatch>,
    /// The tenant's company record for this identity, when one is resolvable (email domain, or the
    /// free-text employer's name). Independent of the contact arm: a known company with no known
    /// person is exactly the lead a CRM should route to an account, not to a contact.
    pub company: Option<CompanyMatch>,
}

impl Match {
    fn none(company: Option<CompanyMatch>) -> Self {
        Self {
            matched: false,
            reason: "none",
            confidence: 0.0,
            contact: None,
            company,
        }
    }
}

/// Resolve one inbound identity against the caller's OWN tenant.
///
/// Cascade, highest confidence first: normalized email, then normalized phone, then first+last name
/// corroborated by the company. The first arm that returns a row wins, and the reason/confidence
/// name that arm. Only ACTIVE contacts are matched (`is_active = true`, the same predicate
/// `contacts::handlers::list` uses) — a deactivated record is deliberately not resurrected by an
/// inbound capture.
pub async fn resolve(db: &PgPool, tenant_id: Uuid, identity: &Identity) -> Result<Match, AppError> {
    let company = match_company(db, tenant_id, identity).await?;

    if let Some(email) = present(&identity.email).and_then(normalize_email) {
        let hit = sqlx::query_as::<_, ContactMatch>(
            "SELECT id, first_name, last_name, email, phone, company
               FROM contacts
              WHERE tenant_id = $1 AND is_active = true AND LOWER(btrim(email)) = $2
              ORDER BY created_at
              LIMIT 1",
        )
        .bind(tenant_id)
        .bind(&email)
        .fetch_optional(db)
        .await?;
        if let Some(c) = hit {
            return Ok(Match {
                matched: true,
                reason: "email",
                confidence: 1.0,
                contact: Some(c),
                company,
            });
        }
    }

    if let Some(phone) = present(&identity.phone).and_then(normalize_phone) {
        // Digits, with ONE deliberate tolerance for the country code: a lead form that sends
        // `(305) 555-0142` must match a record stored as `+1 (305) 555-0142`. So a 10-digit query
        // also matches the LAST 10 digits of an 11-digit stored value whose leading digit is `1`
        // (the NANP country code). Nothing wider than that: a bare country-code-insensitive compare
        // would collide across countries, which is worse than not matching.
        //
        // Measured live 2026-10-06: before this arm, the re-formatted probe
        // `{"phone":"305-555-0142"}` answered `matched:false, reason:"none"` against the fixture's
        // `+1 (305) 555-0142` — the exact "one side carries +1" miss this arm closes.
        let hit = sqlx::query_as::<_, ContactMatch>(
            r#"SELECT id, first_name, last_name, email, phone, company
                 FROM contacts
                WHERE tenant_id = $1 AND is_active = true AND phone IS NOT NULL
                  AND ( regexp_replace(phone, '\D', '', 'g') = $2
                        OR ( length($2) = 10
                             AND length(regexp_replace(phone, '\D', '', 'g')) = 11
                             AND left(regexp_replace(phone, '\D', '', 'g'), 1) = '1'
                             AND right(regexp_replace(phone, '\D', '', 'g'), 10) = $2 ) )
                ORDER BY created_at
                LIMIT 1"#,
        )
        .bind(tenant_id)
        .bind(&phone)
        .fetch_optional(db)
        .await?;
        if let Some(c) = hit {
            return Ok(Match {
                matched: true,
                reason: "phone",
                confidence: 0.8,
                contact: Some(c),
                company,
            });
        }
    }

    if let (Some(first), Some(last)) = (present(&identity.first_name), present(&identity.last_name))
    {
        let company_name = present(&identity.company).unwrap_or("").to_lowercase();
        let hit = sqlx::query_as::<_, ContactMatch>(
            "SELECT id, first_name, last_name, email, phone, company
               FROM contacts
              WHERE tenant_id = $1 AND is_active = true
                AND LOWER(btrim(first_name)) = $2 AND LOWER(btrim(last_name)) = $3
                AND LOWER(btrim(COALESCE(company, ''))) = $4
              ORDER BY created_at
              LIMIT 1",
        )
        .bind(tenant_id)
        .bind(first.to_lowercase())
        .bind(last.to_lowercase())
        .bind(&company_name)
        .fetch_optional(db)
        .await?;
        if let Some(c) = hit {
            return Ok(Match {
                matched: true,
                reason: "name_company",
                confidence: 0.6,
                contact: Some(c),
                company,
            });
        }
    }

    Ok(Match::none(company))
}

/// Find the tenant's COMPANY record for an identity: the email's domain first (the strongest signal
/// — `companies.domain` is the column the CRM already keys on), then the free-text employer's name.
async fn match_company(
    db: &PgPool,
    tenant_id: Uuid,
    identity: &Identity,
) -> Result<Option<CompanyMatch>, AppError> {
    let domain = present(&identity.email).and_then(|e| email_domain(&e.to_lowercase()));
    let name = present(&identity.company).map(|c| c.to_lowercase());
    if domain.is_none() && name.is_none() {
        return Ok(None);
    }
    let hit = sqlx::query_as::<_, CompanyMatch>(
        "SELECT id, name, domain
           FROM companies
          WHERE tenant_id = $1 AND COALESCE(is_active, true) = true
            AND ( ($2::text IS NOT NULL AND LOWER(btrim(COALESCE(domain, ''))) = $2)
               OR ($3::text IS NOT NULL AND LOWER(btrim(name)) = $3) )
          ORDER BY created_at
          LIMIT 1",
    )
    .bind(tenant_id)
    .bind(domain.as_deref())
    .bind(name.as_deref())
    .fetch_optional(db)
    .await?;
    Ok(hit)
}

/// One group of contacts of this tenant that share an identity key.
#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct DuplicateGroup {
    /// `phone` | `name`.
    pub kind: String,
    /// The normalized comparison key the group was built on.
    pub key: String,
    pub count: i64,
    /// Every contact id in the group, oldest first.
    pub contact_ids: Vec<Uuid>,
}

/// Contacts of ONE tenant that share a normalized phone number or a normalized name.
///
/// This is the module's "keeps the pipeline clean" reading: it names the rows an operator has to
/// look at (two records for one person) instead of silently merging them. Email is not a key — the
/// unique index makes an in-tenant email collision impossible.
///
/// `limit` is clamped to `1..=500` (default 100) so the report can never be turned into an
/// unbounded read by a caller.
pub async fn duplicate_groups(
    db: &PgPool,
    tenant_id: Uuid,
    limit: Option<i64>,
) -> Result<Vec<DuplicateGroup>, AppError> {
    let limit = limit.unwrap_or(100).clamp(1, 500);
    let rows = sqlx::query_as::<_, DuplicateGroup>(
        r#"WITH ph AS (
               SELECT regexp_replace(phone, '\D', '', 'g') AS key,
                      array_agg(id ORDER BY created_at) AS contact_ids,
                      count(*) AS count
                 FROM contacts
                WHERE tenant_id = $1 AND is_active = true
                  AND phone IS NOT NULL
                  AND length(regexp_replace(phone, '\D', '', 'g')) >= 7
                GROUP BY 1
               HAVING count(*) > 1
           ), nm AS (
               SELECT LOWER(btrim(first_name)) || ' ' || LOWER(btrim(last_name)) AS key,
                      array_agg(id ORDER BY created_at) AS contact_ids,
                      count(*) AS count
                 FROM contacts
                WHERE tenant_id = $1 AND is_active = true
                  AND btrim(first_name) <> '' AND btrim(last_name) <> ''
                GROUP BY 1
               HAVING count(*) > 1
           )
           SELECT 'phone' AS kind, key, count, contact_ids FROM ph
            UNION ALL
           SELECT 'name'  AS kind, key, count, contact_ids FROM nm
            ORDER BY count DESC, kind, key
            LIMIT $2"#,
    )
    .bind(tenant_id)
    .bind(limit)
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// The contact of THIS tenant whose `email` column equals `email` exactly.
///
/// Moved here verbatim from `contacts::handlers::create`'s dedup read: same SQL, same `LIMIT 1`,
/// and deliberately **not** filtered on `is_active` — the create path merges into whatever row
/// already owns that address, including a deactivated one, and the schema's unique index
/// (`idx_contacts_tenant_email`) is on the RAW column. Changing that predicate here would change
/// which row a merge lands on, so it is preserved exactly.
pub async fn find_contact_by_email_exact(
    db: &PgPool,
    tenant_id: Uuid,
    email: &str,
) -> Result<Option<Contact>, AppError> {
    let contact = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(email)
    .fetch_optional(db)
    .await?;
    Ok(contact)
}
