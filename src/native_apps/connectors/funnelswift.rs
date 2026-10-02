//! FunnelSwift Connector
//!
//! FunnelSwift is a mobile (Expo/React Native) sales funnel builder.
//! Tenants connect their own FunnelSwift account to sync leads, funnels and tags.
//!
//! Access: Admin + Tenant
//!
//! ── REAL ROUTES (measured live 2026-10-02, kanban t_b43e4604) ─────────────────────────
//! The entity names were settled on this connector's own cards (t_e8a7f651 plural mapping,
//! t_7225a94d declares-only-what-it-serves): push [lead, funnel, tag] -> `/api/v1/leads`,
//! `/api/v1/funnels`, `/api/v1/tags`; pull [leads, funnels, tags]. `contact` (push) and
//! `contacts` (pull) are retired — FunnelSwift serves no contact route and has no `contacts`
//! table.
//!
//! What those cards did NOT measure is the PREFIX this file built. It sent every request to
//! `{base}/v1/…` and probed `{base}/v1/health`, while FunnelSwift's routers are mounted under
//! `/api/v1/*` (and `/api/health`). Measured against the console's own default base
//! (`https://app.funnelswift.net`, the value `IC_BASE_DEFAULT` pre-fills):
//!
//!   test()   GET  /v1/health   -> 200 text/html   the SPA SHELL, for a BOGUS bearer too
//!   push     POST /v1/leads    -> 200 text/html   the SPA SHELL (then a JSON parse failure)
//!   pull     GET  /v1/leads    -> 200 text/html   the SPA SHELL
//!
//! nginx's `location /` on that vhost is `try_files $uri $uri/ /index.html`, so any unmatched
//! path answers the 1900-byte SPA shell with HTTP 200 — which made `test()` VACUOUS (it
//! reported success for any credential at all, and `connect_app()` stores whatever its test
//! accepted) and made every push/pull fail at the JSON parse instead of the far end's own
//! answer. The real surface, measured with a real FunnelSwift JWT: `/api/v1/health` 200,
//! `/api/v1/leads` 200 (GET) / 400 (POST, needs a name), `/api/v1/funnels` 200,
//! `/api/v1/tags` 200/422; a bogus bearer answers 401 on all of them, and `/api/v1/bogus-xyz`
//! answers 404, so a 404 here is the ROUTER.
//!
//! `base_url` is the deployment's HOST (e.g. `https://app.funnelswift.net`) — the connector
//! builds the `/api/v1/…` path itself — and `api_key` holds a FunnelSwift **bearer token**.

use std::collections::HashMap;

const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Test the FunnelSwift connection.
///
/// Probes `GET {base}/api/v1/leads`, which sits behind FunnelSwift's auth middleware: a
/// usable credential answers 200, a rejected one 401/403. Deliberately NOT a health route and
/// NOT `{base}/v1/health`: the latter is served by nginx's SPA fallback, which answers 200
/// text/html to anything (see the module header).
pub async fn test(creds: &serde_json::Value) -> (bool, String) {
    let api_key = match extract_key(creds) {
        Ok(k) => k,
        Err(e) => return (false, e),
    };
    let api_url = api_base(creds);

    let url = format!("{}/api/v1/leads", api_url);
    match reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            (true, "FunnelSwift connection successful".into())
        }
        Ok(resp)
            if resp.status() == reqwest::StatusCode::UNAUTHORIZED
                || resp.status() == reqwest::StatusCode::FORBIDDEN =>
        {
            (
                false,
                format!(
                    "FunnelSwift rejected the credential (HTTP {})",
                    resp.status()
                ),
            )
        }
        Ok(resp) => (
            false,
            format!(
                "FunnelSwift returned status {} — check the base URL points at the host, not a path",
                resp.status()
            ),
        ),
        Err(e) => (false, format!("FunnelSwift connection failed: {}", e)),
    }
}

pub async fn push_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    data: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let api_key = extract_key(creds)?;
    // Use the tenant's base_url from credentials (the Integration Center collects it),
    // then the legacy api_url key, then the default.
    let api_url = api_base(creds);

    match entity_type {
        // FunnelSwift's routes are PLURAL (`/api/v1/leads`, `/api/v1/funnels`, `/api/v1/tags`);
        // it serves no singular route and no contact route at all. `contact` is retired rather
        // than mapped: FunnelSwift has no contact route and no `contacts` table.
        "lead" | "funnel" | "tag" => {
            let path = match entity_type {
                "lead" => "leads",
                "funnel" => "funnels",
                _ => "tags",
            };
            let url = format!("{}/api/v1/{}", api_url, path);
            let resp = reqwest::Client::new()
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .json(data)
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("FunnelSwift push failed: {}", e))?;
            read_response(resp, "push").await
        }
        _ => Err(format!(
            "FunnelSwift does not support entity type: {}",
            entity_type
        )),
    }
}

pub async fn pull_entity(
    creds: &serde_json::Value,
    entity_type: &str,
    filters: &HashMap<String, String>,
) -> Result<serde_json::Value, String> {
    let api_key = extract_key(creds)?;
    let query = if filters.is_empty() {
        String::new()
    } else {
        let params: Vec<String> = filters
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect();
        format!("?{}", params.join("&"))
    };

    let api_url = api_base(creds);

    match entity_type {
        // `contacts` is retired: FunnelSwift serves no `/api/v1/contacts` (404 live)
        // and has no `contacts` table, so no request could ever resolve.
        "leads" | "funnels" | "tags" => {
            let url = format!("{}/api/v1/{}{}", api_url, entity_type, query);
            let resp = reqwest::Client::new()
                .get(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .send()
                .await
                .map_err(|e| format!("FunnelSwift pull failed: {}", e))?;
            read_response(resp, "pull").await
        }
        _ => Err(format!(
            "FunnelSwift does not support pulling entity type: {}",
            entity_type
        )),
    }
}

/// Turn a FunnelSwift response into the JSON the sync log/modal shows, refusing honestly.
///
/// A non-2xx (or an SPA-shell 200) used to be parsed as if it were a payload, so a refusal
/// surfaced as a parser error with the far end's own reason thrown away.
async fn read_response(
    resp: reqwest::Response,
    direction: &str,
) -> Result<serde_json::Value, String> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("FunnelSwift response read failed: {}", e))?;
    let trimmed = body.trim();
    if !status.is_success() {
        return Err(format!(
            "FunnelSwift {} refused (HTTP {}): {}",
            direction, status, trimmed
        ));
    }
    serde_json::from_str(trimmed).map_err(|e| {
        format!(
            "FunnelSwift response parse failed: {} (body: {})",
            e,
            &trimmed.chars().take(200).collect::<String>()
        )
    })
}

pub fn get_meta() -> serde_json::Value {
    serde_json::json!({
        "name": "FunnelSwift",
        "slug": "funnelswift",
        "description": "Mobile sales funnel builder (Expo/React Native)",
        "auth_type": "api_key",
        "auth_fields": ["api_key", "base_url"],
        "access_level": "admin_tenant",
        // The declared entity vocabulary IS the implemented one — nothing more (kanban t_7225a94d).
        // This list used to advertise `product_selection` (push) and `affiliate_products` +
        // `my_products` (pull) while `push_entity()`/`pull_entity()` below matched only
        // lead|contact|funnel|tag and leads|contacts|funnels|tags — so all three fell to the
        // "does not support ..." arm. An advertised capability the connector itself refuses.
        //
        // MEASURED 2026-10-02 before removing them:
        //   * `grep -rn "affiliate_products|my_products|product_selection" src/` -> the declaration
        //     itself and nothing else: 0 reads, 0 writes, no route, no consumer surface.
        //   * `app_connections` = 0 rows and `app_sync_logs` = 0 rows in coreswift_crm: no tenant had
        //     ever connected a native app, so no pull or push had ever run for ANY entity.
        //   * the shipped console's sync modal drives its own list (contacts|lists|tags), so the
        //     declared names were never reachable from the UI either.
        //   * the names could not have resolved even if wired: FunnelSwift serves
        //     `/api/v1/affiliate-products` (hyphen) and has no `/api/v1/contacts` at all, while this
        //     connector builds `{base}/api/v1/{entity_type}` from the literal below.
        //
        // BUILDING THE SYNC WAS DECLINED, NOT DEFERRED. FunnelSwift owns the affiliate programme and
        // its commissionable catalogue (ARCHITECTURE.md Rules 2 and 4; migration 108 retires
        // CoreSwift's dead copy of that schema), the products are managed in the FunnelSwift admin
        // (docs/ADMIN_GUIDE.md), and CoreSwift has no surface that would show them — so wiring a pull
        // would mean inventing a product decision, not repairing a claim. The direction stays correct
        // if someone later builds a real consumer, in which case the declaration comes back together
        // with the arm that serves it.
        // The declaration names only routes a live request can reach (kanban t_e8a7f651 /
        // t_b43e4604). The remaining names resolve through the plural mapping in `push_entity()`:
        // lead -> `/api/v1/leads` (400 "needs a name" = handler reached), funnel ->
        // `/api/v1/funnels` (201), tag -> `/api/v1/tags` (422 missing field); `pull_entity()`
        // GETs `/api/v1/leads`, `/api/v1/funnels` and `/api/v1/tags` (all 200).
        "entities": { "push": ["lead", "funnel", "tag"], "pull": ["leads", "funnels", "tags"] },
        "features": ["Push leads from CRM into FunnelSwift funnels", "Pull FunnelSwift leads and funnels back into CRM"]
    })
}

fn extract_key(creds: &serde_json::Value) -> Result<String, String> {
    creds
        .get("api_key")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| "FunnelSwift API key (bearer token) is required".into())
}

/// The API base this tenant's FunnelSwift answers on. The credential the tenant enters in
/// the Integration Center is authoritative (`base_url`); `api_url` is the legacy key; the
/// public host is only a fallback so a missing field degrades to a failed test rather than a
/// panic. This is the deployment's HOST — the `/api/v1/…` path is built by the callers above.
fn api_base(creds: &serde_json::Value) -> String {
    creds
        .get("base_url")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            creds
                .get("api_url")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("https://app.funnelswift.net")
        .trim_end_matches('/')
        .to_string()
}
