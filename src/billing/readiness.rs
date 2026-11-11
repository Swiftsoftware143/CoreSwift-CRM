//! Payment readiness — the ONE read that answers "can a buyer pay on this install right now?".
//!
//! Why this exists (kanban t_f11f71fb, 2026-11-11). CoreSwift has no card checkout: `POST
//! /checkout/create` and its provider webhooks were RETIRED 2026-09-25 (see the note in
//! `billing/mod.rs`), and no payment provider exists in `available_providers`. The ONLY arm a buyer
//! can be charged on is a plan's own hosted payment page (`plans.checkout_url`), which the console
//! offers as "Open payment page". So "can a buyer pay?" = "does any ACTIVE paid plan carry a link
//! that actually opens?".
//!
//! MEASURED 2026-11-11: all four plan links on this install point at
//! `https://mintbird.com/checkout/crm-swift-*` and every one of them answers **404** to a GET (a real
//! slug and an invented slug answer the same way, and a HEAD answers 200 for anything — so a HEAD
//! check would have lied). A buyer pressing Upgrade on Starter, Professional or Enterprise landed on
//! "No Data Found." and nothing told the operator. Counting rows cannot see that: the rows exist and
//! look sellable, which is exactly why the count-only version of this signal would stay silent while
//! nobody could pay.
//!
//! NON-VACUITY RULES, all of them load-bearing:
//! * `can_sell` is TRUE only when a plan link was really FETCHED and answered a 2xx.
//! * A probe that times out or fails to connect is NOT proof of a dead link: it answers `null` and
//!   is excluded from `links_ok`, so a slow host can never be reported as "customers cannot pay"
//!   unless every single link came back with a definitive non-2xx.
//! * `links_ok` counts only definitive answers, `links_dead` counts 4xx/5xx. The caller paints the
//!   warning on `can_sell == false` from a SUCCESSFUL read — never on an error, which is a 500.
//!
//! Cost control: the answer is cached for [`CACHE_TTL`] (the dashboard is opened far more often than
//! plans change), at most [`MAX_PROBED`] links are probed, they are probed concurrently, and each
//! probe is bounded by [`PROBE_TIMEOUT`]. The route is platform-admin only — it is a statement about
//! the platform's own catalogue, not about a tenant's data.

use crate::auth::models::Claims;
use crate::auth::platform_admin::require_platform_admin;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{extract::Extension, extract::State, Json};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// How long one answer is reused. Plans change by human hand, so five minutes is well inside the
/// window where a corrected link is still seen promptly, while a dashboard reload never pays for a
/// new round of outbound requests.
const CACHE_TTL: Duration = Duration::from_secs(300);
/// One hosted page may take this long to answer before the probe gives up and reports "unknown".
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// Concurrency and cost ceiling: the catalogue has a handful of paid tiers, and a bigger list must
/// not turn a dashboard read into a scan.
const MAX_PROBED: usize = 6;

type Cached = Mutex<Option<(Instant, Value)>>;
static CACHE: OnceLock<Cached> = OnceLock::new();

fn cache() -> &'static Cached {
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Is this a link we are willing to fetch from the server?
///
/// Only `https`, only a public-looking host: this app never makes the server visit an address a
/// tenant can write, and although `plans` is operator-authored, a paste of `http://127.0.0.1:8080/…`
/// must not turn the dashboard into an internal port scanner (the SSRF class).
fn probeable(url: &str) -> bool {
    let u = url.trim();
    let rest = match u.strip_prefix("https://") {
        Some(r) => r,
        None => return false,
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@') // drop any userinfo
        .next()
        .unwrap_or("");
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    const PRIVATE: [&str; 6] = [
        "localhost",
        "127.",
        "10.",
        "192.168.",
        "169.254.",
        "0.0.0.0",
    ];
    if PRIVATE.iter().any(|p| host.starts_with(p)) || host.ends_with(".local") || host == "[::1]" {
        return false;
    }
    true
}

/// `true` = the page answered 2xx. `false` = a definitive refusal (4xx/5xx / transport error);
/// `None` = we could not find out inside the deadline, which is not an answer and is never counted
/// as a dead link.
async fn probe(client: &reqwest::Client, url: &str) -> Option<bool> {
    match tokio::time::timeout(PROBE_TIMEOUT, client.get(url).send()).await {
        // HEAD is useless here: the host serving this install's links answers 200 to a HEAD for a
        // path that 404s to a GET. The body is never read; the status is the whole answer.
        Ok(Ok(r)) => Some(r.status().is_success()),
        Ok(Err(_)) => Some(false),
        Err(_) => None,
    }
}

/// `GET /api/billing/readiness` — what the till can do on this install, for the operator dashboard.
pub async fn payment_readiness(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<Value>> {
    require_platform_admin(&s.db, &c.sub).await?;

    if let Some((at, v)) = cache().lock().await.as_ref() {
        if at.elapsed() < CACHE_TTL {
            return Ok(Json(v.clone()));
        }
    }

    // The catalogue, read the way the console reads it: ACTIVE rows only, and a tier is PAID when
    // either of its two prices is above zero (`plans.price_monthly` / `price_yearly`, the pair the
    // billing screen quotes).
    let rows = sqlx::query_as::<_, (String, Option<String>, bool)>(
        "SELECT slug, checkout_url, \
                (COALESCE(price_monthly, 0) > 0 OR COALESCE(price_yearly, 0) > 0) AS paid \
           FROM plans WHERE is_active = true ORDER BY sort_order ASC NULLS LAST, slug ASC",
    )
    .fetch_all(&s.db)
    .await?;

    let paid: Vec<(String, Option<String>)> = rows
        .into_iter()
        .filter(|(_, _, is_paid)| *is_paid)
        .map(|(slug, url, _)| (slug, url))
        .collect();

    let mut targets: Vec<String> = Vec::new();
    for (_, raw) in &paid {
        let u = raw.as_deref().unwrap_or("").trim();
        if u.is_empty() || !probeable(u) {
            continue;
        }
        if !targets.iter().any(|t| t == u) {
            targets.push(u.to_string());
        }
    }
    let planned = targets.len();
    targets.truncate(MAX_PROBED);

    // Concurrent, each bounded: the whole read costs one probe timeout, not one per link.
    let client = reqwest::Client::builder()
        .timeout(PROBE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent("CoreSwift-readiness/1.0")
        .build()
        .map_err(|e| AppError::Internal(format!("readiness client: {e}")))?;
    let mut handles = Vec::with_capacity(targets.len());
    for u in targets {
        let cl = client.clone();
        handles.push(tokio::spawn(async move { probe(&cl, &u).await }));
    }
    let mut links_ok = 0usize;
    let mut links_dead = 0usize;
    let mut links_unknown = 0usize;
    for h in handles {
        match h.await {
            Ok(Some(true)) => links_ok += 1,
            Ok(Some(false)) => links_dead += 1,
            _ => links_unknown += 1,
        }
    }

    let body = json!({
        "can_sell": links_ok > 0,
        "paid_plans": paid.len(),
        "plans_with_link": planned,
        // `links_checked` is what actually got an answer (2xx or a definitive refusal); an unknown
        // link is reported separately so the caller can tell "all dead" from "could not tell".
        "links_checked": links_ok + links_dead,
        "links_ok": links_ok,
        "links_dead": links_dead,
        "links_unknown": links_unknown,
        "checked_at": chrono::Utc::now().to_rfc3339(),
        "cache_seconds": CACHE_TTL.as_secs(),
    });

    *cache().lock().await = Some((Instant::now(), body.clone()));
    Ok(Json(body))
}

#[cfg(test)]
mod tests {
    use super::probeable;

    #[test]
    fn only_public_https_links_are_fetched() {
        assert!(probeable("https://mintbird.com/checkout/crm-swift-starter"));
        assert!(probeable("  https://buy.stripe.com/abc  "));
        // Not fetchable: no TLS, no host, or a host that is this machine or its private network.
        assert!(!probeable("http://mintbird.com/checkout/x"));
        assert!(!probeable(""));
        assert!(!probeable("https://"));
        assert!(!probeable("https://localhost:8080/api/admin"));
        assert!(!probeable("https://127.0.0.1/x"));
        assert!(!probeable("https://10.0.0.5/x"));
        assert!(!probeable("https://192.168.1.9/x"));
        assert!(!probeable("https://169.254.169.254/latest/meta-data"));
        assert!(!probeable("https://printer.local/x"));
    }
}
