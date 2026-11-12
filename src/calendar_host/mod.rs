//! Hosted calendar address — one working public calendar per workspace (kanban t_8457bfef).
//!
//! EVERY workspace gets a unique subdomain under the system domain (`<subdomain>.coreswiftcrm.com`),
//! so a customer has a calendar address the moment the workspace exists. A workspace that owns a
//! domain can point that domain at the same calendar instead; the system subdomain keeps answering
//! as the fallback, so pointing a custom domain is never a way to break the address that already
//! worked.
//!
//! WHAT THE ADDRESS SERVES: the workspace's ACTIVE booking calendars and their open slot types, and
//! a visitor can book one without an account. The visitor never names a workspace — the request's
//! `Host` header does, which is why this file owns the resolution and the booking write: the tenant
//! comes from the address, not from a field a caller could set to somebody else's id.
//!
//! WHY THE DATABASE OWNS UNIQUENESS: `calendar_hosts` carries `UNIQUE (lower(subdomain))` and a
//! partial `UNIQUE (lower(custom_domain))` (migration 146). A handler that "checks then inserts" has
//! a race between the two statements, and two workspaces sharing one address is a tenancy defect,
//! not an inconvenience. The insert paths below treat a unique violation (SQLSTATE 23505) as
//! "somebody owns that name" and answer it as such.
//!
//! GENERATION IS ONE WRITER. [`ensure_for_tenant`] is the only place a row is minted from a
//! workspace's slug, and every arm that can create a workspace inherits it: the sweep
//! [`ensure_all`] runs on the worker's schedule and fills any workspace without a row, and the read
//! path calls [`ensure_for_tenant`] lazily so a brand-new workspace sees its address on the first
//! visit instead of after the next sweep.
//!
//! Routes:
//! - GET    /api/calendar-host                     — this workspace's address + custom-domain state
//! - PATCH  /api/calendar-host/subdomain           — claim a different system subdomain
//! - PUT    /api/calendar-host/custom-domain       — point an owned domain at this calendar
//! - DELETE /api/calendar-host/custom-domain       — drop it, fall back to the system subdomain
//! - POST   /api/calendar-host/custom-domain/check — prove the domain really reaches this workspace
//! - GET    /api/public/calendar/site              — the hosted page's own data (Host-resolved)
//! - POST   /api/public/calendar/book              — book a slot (Host-resolved, tenant-scoped)
//!
//! The `PATCH`/`PUT`/`DELETE` arms are the ACCOUNT's own settings (they change where this
//! workspace's calendar answers), so they demand owner/admin — the same roles the rest of the
//! account surface demands, not merely a session.

pub mod handlers;

use crate::{errors::AppError, AppState};
use axum::{
    middleware,
    routing::{get, patch, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// The system domain every generated address hangs off. `CALENDAR_SYSTEM_DOMAIN` overrides it; the
/// default is the deployment's own apex, so an unset environment is still correct.
pub fn system_domain(state: &AppState) -> String {
    state.config.calendar_system_domain.clone()
}

/// Names the platform already answers on, or reserves for another purpose. A workspace may never
/// claim one: `<reserved>.coreswiftcrm.com` is either a live system host (app, admin, inbox, mail)
/// or a name an operator will need later. Mirrors the CHECK in migration 146 — the database refuses
/// them too, so this list is the friendly answer, not the enforcement.
pub const RESERVED_SUBDOMAINS: &[&str] = &[
    "app",
    "www",
    "admin",
    "api",
    "apex",
    "inbox",
    "pmail",
    "mail",
    "email",
    "smtp",
    "imap",
    "pop",
    "pop3",
    "webmail",
    "mx",
    "mxa",
    "mxb",
    "ns1",
    "ns2",
    "ns",
    "cdn",
    "static",
    "assets",
    "media",
    "status",
    "docs",
    "help",
    "support",
    "billing",
    "payments",
    "checkout",
    "calendar",
    "calendars",
    "book",
    "booking",
    "bookings",
    "crm",
    "dashboard",
    "login",
    "logout",
    "register",
    "signup",
    "sign-up",
    "account",
    "accounts",
    "auth",
    "portal",
    "blog",
    "news",
    "dev",
    "staging",
    "test",
    "staging1",
    "origin",
    "secure",
    "vpn",
    "ftp",
    "whois",
    "verify",
    "downloads",
    "files",
];

/// A row of `calendar_hosts`, joined with the branding the hosted page renders.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CalendarHost {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subdomain: String,
    pub custom_domain: Option<String>,
    pub custom_domain_status: String,
    pub custom_domain_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub custom_domain_last_error: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ResolvedHost {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub subdomain: String,
    pub custom_domain: Option<String>,
    pub custom_domain_status: String,
    pub tenant_name: String,
    pub tenant_slug: String,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub branding_name: Option<String>,
}

/// Reduce a workspace slug to something that can be a DNS label. Lowercase, `[a-z0-9-]`, no leading
/// or trailing hyphen. Never returns an empty string (a slug made only of symbols becomes
/// `workspace`), so a caller always has a base to suffix.
pub fn sanitise_base(slug: &str) -> String {
    let mut out = String::with_capacity(slug.len());
    let mut last_dash = false;
    for ch in slug.chars().flat_map(|c| c.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    let trimmed = if trimmed.len() > 50 {
        trimmed[..50].trim_end_matches('-')
    } else {
        trimmed
    };
    if trimmed.len() < 3 {
        format!(
            "workspace-{}",
            if trimmed.is_empty() { "1" } else { trimmed }
        )
    } else {
        trimmed.to_string()
    }
}

/// True when `value` is a legal single DNS label for a calendar subdomain.
pub fn valid_subdomain(value: &str) -> bool {
    let b = value.as_bytes();
    if b.len() < 3 || b.len() > 63 {
        return false;
    }
    if !b[0].is_ascii_alphanumeric() || !b[b.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    if !b
        .iter()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
    {
        return false;
    }
    // `xn--` is punycode: it can render as ANY unicode name, including one that reads as another
    // brand. Refuse the prefix rather than try to police what it decodes to.
    !value.starts_with("xn--")
}

pub fn is_reserved(value: &str) -> bool {
    RESERVED_SUBDOMAINS.contains(&value)
}

/// Normalise a domain a customer typed or pasted: strip a scheme, a path, a port, a trailing dot and
/// any `www.`-preserving case. Returns the bare lowercase host, or `None` when nothing usable is
/// left. This is deliberately forgiving — an operator pasting `https://Calendar.Acme.com/` means the
/// same thing as `calendar.acme.com`, and refusing it teaches nothing.
pub fn normalise_domain(input: &str) -> Option<String> {
    let mut s = input.trim().to_string();
    if let Some(idx) = s.find("://") {
        s = s[idx + 3..].to_string();
    }
    if let Some(idx) = s.find('/') {
        s.truncate(idx);
    }
    if let Some(idx) = s.find('?') {
        s.truncate(idx);
    }
    if let Some(idx) = s.find('@') {
        s = s[idx + 1..].to_string();
    }
    if let Some(idx) = s.rfind(':') {
        // A port, unless this colon is inside an IPv6 literal (which has more colons).
        if s.matches(':').count() == 1 {
            s.truncate(idx);
        }
    }
    let s = s.trim_end_matches('.').to_lowercase();
    if s.is_empty() || s.len() > 253 {
        return None;
    }
    Some(s)
}

/// True when `domain` is a legal hostname (not an IP, at least two labels, every label a DNS label).
pub fn valid_domain(domain: &str) -> bool {
    if domain.len() < 4 || domain.len() > 253 {
        return false;
    }
    if domain.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    if labels.iter().any(|l| l.is_empty() || l.len() > 63) {
        return false;
    }
    if labels
        .iter()
        .any(|l| !l.starts_with(|c: char| c.is_ascii_alphanumeric()))
    {
        return false;
    }
    if labels
        .iter()
        .any(|l| !l.ends_with(|c: char| c.is_ascii_alphanumeric()))
    {
        return false;
    }
    if labels
        .iter()
        .any(|l| !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
    {
        return false;
    }
    // The TLD must be alphabetic: `1.2.3.4` is an IP (caught above) and `acme.123` is not a site.
    labels
        .last()
        .map(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
        .unwrap_or(false)
}

/// Is `domain` inside the deployment's own system domain? Those names belong to the platform, and a
/// workspace cannot claim one as its "custom" domain — the system subdomain arm already owns them.
pub fn is_system_domain(domain: &str, system: &str) -> bool {
    domain == system || domain.ends_with(&format!(".{system}"))
}

/// This workspace's address, minting it if it does not exist yet. `None` means the tenant row itself
/// is gone (a caller with a stale token), not a failure.
pub async fn ensure_for_tenant(
    db: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<CalendarHost>, sqlx::Error> {
    if let Some(row) = get_by_tenant(db, tenant_id).await? {
        return Ok(Some(row));
    }
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(db)
        .await?;
    let Some(slug) = slug else {
        return Ok(None);
    };
    let base = sanitise_base(&slug);
    for attempt in 1..=60u32 {
        let candidate = if attempt == 1 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        if !valid_subdomain(&candidate) || is_reserved(&candidate) {
            continue;
        }
        let inserted = sqlx::query_as::<_, CalendarHost>(
            r#"INSERT INTO calendar_hosts (tenant_id, subdomain)
               VALUES ($1, $2)
               ON CONFLICT (tenant_id) DO NOTHING
               RETURNING id, tenant_id, subdomain, custom_domain, custom_domain_status,
                         custom_domain_checked_at, custom_domain_last_error, is_active"#,
        )
        .bind(tenant_id)
        .bind(&candidate)
        .fetch_optional(db)
        .await;
        match inserted {
            Ok(Some(row)) => return Ok(Some(row)),
            // Another writer (the sweep, or a second request) claimed this workspace first.
            Ok(None) => return get_by_tenant(db, tenant_id).await,
            // That subdomain belongs to another workspace: try the next suffix.
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

pub async fn get_by_tenant(
    db: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<CalendarHost>, sqlx::Error> {
    sqlx::query_as::<_, CalendarHost>(
        r#"SELECT id, tenant_id, subdomain, custom_domain, custom_domain_status,
                  custom_domain_checked_at, custom_domain_last_error, is_active
           FROM calendar_hosts WHERE tenant_id = $1"#,
    )
    .bind(tenant_id)
    .fetch_optional(db)
    .await
}

/// The sweep: mint an address for every active workspace that has none. Idempotent by construction
/// (`ensure_for_tenant` reads first), so it is safe to run on a timer and safe to run twice at once.
/// Returns how many rows were created.
pub async fn ensure_all(db: &PgPool) -> Result<usize, sqlx::Error> {
    let missing: Vec<Uuid> = sqlx::query_scalar(
        r#"SELECT t.id FROM tenants t
           WHERE t.is_active
             AND NOT EXISTS (SELECT 1 FROM calendar_hosts ch WHERE ch.tenant_id = t.id)
           ORDER BY t.created_at
           LIMIT 500"#,
    )
    .fetch_all(db)
    .await?;
    let mut created = 0usize;
    for id in missing {
        if ensure_for_tenant(db, id).await?.is_some() {
            created += 1;
        }
    }
    Ok(created)
}

/// Resolve an incoming `Host` to the workspace whose calendar it names. Both arms are tried: the
/// custom domain first (it is the more specific claim) and then `<subdomain>.<system>`.
pub async fn resolve_host(
    db: &PgPool,
    raw_host: &str,
    system: &str,
) -> Result<Option<ResolvedHost>, sqlx::Error> {
    let host = host_only(raw_host);
    if host.is_empty() {
        return Ok(None);
    }
    sqlx::query_as::<_, ResolvedHost>(
        r#"SELECT ch.id, ch.tenant_id, ch.subdomain, ch.custom_domain, ch.custom_domain_status,
                  t.name AS tenant_name, t.slug AS tenant_slug, t.logo_url,
                  t.primary_color, t.accent_color, t.branding_name
           FROM calendar_hosts ch
           JOIN tenants t ON t.id = ch.tenant_id
           WHERE ch.is_active AND t.is_active
             AND (lower(ch.custom_domain) = $1
                  OR lower(ch.subdomain) || '.' || $2 = $1)
           LIMIT 1"#,
    )
    .bind(&host)
    .bind(system)
    .fetch_optional(db)
    .await
}

/// The `Host` header without its port, lowercased. An IPv6 literal keeps its brackets.
pub fn host_only(raw: &str) -> String {
    let h = raw.trim().to_lowercase();
    if h.starts_with('[') {
        // [::1]:8084 — the port is after the closing bracket.
        return match h.find(']') {
            Some(i) => h[..=i].to_string(),
            None => h,
        };
    }
    match h.rfind(':') {
        Some(i) => h[..i].to_string(),
        None => h,
    }
}

/// The public URL of a resolved host, always https.
pub fn public_url(subdomain: &str, custom: Option<&str>, system: &str) -> String {
    match custom {
        Some(d) if !d.is_empty() => format!("https://{d}"),
        _ => format!("https://{subdomain}.{system}"),
    }
}

/// Console router — every route is the signed-in workspace's OWN address.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", get(handlers::get_host))
        .route("/subdomain", patch(handlers::set_subdomain))
        .route(
            "/custom-domain",
            put(handlers::set_custom_domain).delete(handlers::clear_custom_domain),
        )
        .route("/custom-domain/check", post(handlers::check_custom_domain))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}

/// Public router — resolve from the `Host` header, never from a caller-supplied workspace id.
pub fn public_router() -> Router<AppState> {
    Router::new()
        .route("/site", get(handlers::public_site))
        .route("/book", post(handlers::public_book))
}

/// The sentence a duplicate name is answered with. One string, so the console, the API and the
/// guide all say the same thing.
pub fn taken_message(value: &str) -> String {
    format!("The calendar address '{value}' is already in use. Try a different name.")
}

/// Map a database unique violation to the friendly duplicate answer; anything else stays a 500.
pub fn map_unique(err: sqlx::Error, value: &str) -> AppError {
    if let sqlx::Error::Database(ref d) = err {
        if d.code().as_deref() == Some("23505") {
            return AppError::Conflict(taken_message(value));
        }
    }
    AppError::Database(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitise_keeps_dns_labels() {
        assert_eq!(sanitise_base("Acme Roofing"), "acme-roofing");
        assert_eq!(sanitise_base("--acme--"), "acme");
        assert_eq!(
            sanitise_base("swiftimpact-solutions"),
            "swiftimpact-solutions"
        );
        assert_eq!(sanitise_base("!!!"), "workspace-1");
        assert_eq!(sanitise_base("ab"), "workspace-ab");
        assert!(sanitise_base(&"x".repeat(80)).len() <= 50);
    }

    #[test]
    fn subdomain_rules() {
        assert!(valid_subdomain("acme"));
        assert!(valid_subdomain("acme-roofing-2"));
        assert!(!valid_subdomain("ab"));
        assert!(!valid_subdomain("-acme"));
        assert!(!valid_subdomain("acme-"));
        assert!(!valid_subdomain("Acme"));
        assert!(!valid_subdomain("acme_roofing"));
        assert!(!valid_subdomain("xn--acme"));
        assert!(!valid_subdomain(&"a".repeat(64)));
        assert!(is_reserved("app"));
        assert!(!is_reserved("acme"));
    }

    #[test]
    fn domain_normalisation_is_forgiving_about_what_people_paste() {
        assert_eq!(
            normalise_domain("https://Calendar.Acme.com/").as_deref(),
            Some("calendar.acme.com")
        );
        assert_eq!(
            normalise_domain("  acme.com.  ").as_deref(),
            Some("acme.com")
        );
        assert_eq!(
            normalise_domain("http://acme.com:8080/x?y=1").as_deref(),
            Some("acme.com")
        );
        assert_eq!(normalise_domain("").as_deref(), None);
        assert!(valid_domain("calendar.acme.com"));
        assert!(!valid_domain("acme"));
        assert!(!valid_domain("1.2.3.4"));
        assert!(!valid_domain("acme.123"));
        assert!(!valid_domain("-acme.com"));
        assert!(is_system_domain("app.coreswiftcrm.com", "coreswiftcrm.com"));
        assert!(is_system_domain("coreswiftcrm.com", "coreswiftcrm.com"));
        assert!(!is_system_domain(
            "coreswiftcrm.com.evil.net",
            "coreswiftcrm.com"
        ));
    }

    #[test]
    fn host_header_is_read_without_its_port() {
        assert_eq!(
            host_only("Acme.CoreSwiftCRM.com:443"),
            "acme.coreswiftcrm.com"
        );
        assert_eq!(host_only("acme.coreswiftcrm.com"), "acme.coreswiftcrm.com");
        assert_eq!(host_only("[::1]:8084"), "[::1]");
    }
}
