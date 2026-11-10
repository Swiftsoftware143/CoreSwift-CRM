use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde_json::json;
use uuid::Uuid;

use super::feature_gate;
use super::models::*;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn provision_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<ProvisionMailboxRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    feature_gate::check_mailbox_limit(&state.db, account_id).await?;

    // Get the domain
    let domain = sqlx::query_as::<_, PrivateEmailDomain>(
        "SELECT * FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(req.domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let domain = domain.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    // Check for duplicate
    let email_address = format!("{}@{}", req.local_part, domain.domain);
    let existing = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 AND email_address = $2",
    )
    .bind(account_id)
    .bind(&email_address)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    if existing.is_some() {
        return Err(AppError::BadRequest("Email address already exists".into()));
    }

    // ── Mailgun domains: make sure the PER-DOMAIN inbound route exists ─────────────────────────
    // Mailgun has NO mailbox-creation API. A "mailbox" on a Mailgun domain is (a) the local
    // `private_email_boxes` row the inbox reads and (b) the domain's inbound route, which makes
    // Mailgun hand every address at that domain to this app. This arm used to POST
    // `{base}/v3/{domain}/addresses` — an endpoint Mailgun does not have — so every create on a
    // Mailgun domain answered 500 "404 page not found" (kanban t_0adbad21). Mailgun's legacy
    // `POST /v3/<domain>/mailboxes` is closed to new customers, so there is nothing to call there
    // either; the route is the only real provider-side object.
    //
    // For SMTP/SES/Postmark there is nothing to do at all: the user brings their own DNS and
    // mailbox, and this app only keeps the local row.
    if domain.provider_type == "mailgun" {
        // A stored key this tenant's secret cannot open is a CONFIGURATION problem the caller can act
        // on (re-add the domain with a valid key). It used to surface as a bare 500 "Internal server
        // error", which named neither the cause nor the fix.
        let api_key = crate::secret_box::open(account_id, &domain.mailgun_api_key);
        if api_key.trim().is_empty() {
            return Err(AppError::Validation(
                "This domain's stored Mailgun API key cannot be read — re-add the domain with a \
                 valid key."
                    .into(),
            ));
        }

        let base_url = if domain.mailgun_region == "eu" {
            "https://api.eu.mailgun.net"
        } else {
            "https://api.mailgun.net"
        };

        // Idempotent and self-repairing: an existing route for this domain is reused, a route that
        // forwards somewhere else (the retired `coreswift.net` target) is repaired in place, and a
        // route is only created when the domain has none — so re-adding a mailbox never stacks
        // duplicate routes that would deliver each inbound message twice.
        //
        // A refusal is answered 422 with MAILGUN'S OWN SENTENCE: the operator learns what the
        // provider said instead of reading "Internal server error", which named no cause and no fix
        // (kanban t_0adbad21, same rule as t_93214280).
        ensure_mailgun_route(base_url, &api_key, &domain.domain)
            .await
            .map_err(AppError::Validation)?;
    }

    let row = sqlx::query_as::<_, PrivateEmailBox>(
        r#"
        INSERT INTO private_email_boxes (tenant_id, domain_id, user_id, local_part, email_address)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(req.domain_id)
    .bind(req.user_id)
    .bind(&req.local_part)
    .bind(&email_address)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}

pub async fn list_mailboxes(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let boxes = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&boxes).unwrap()))
}

pub async fn delete_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(box_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mailbox = sqlx::query_as::<_, PrivateEmailBox>(
        "SELECT * FROM private_email_boxes WHERE id = $1 AND tenant_id = $2",
    )
    .bind(box_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let mailbox = mailbox.ok_or_else(|| AppError::NotFound("Mailbox not found".into()))?;

    // Soft-deactivate
    sqlx::query(
        "UPDATE private_email_boxes SET status = 'deprovisioned', updated_at = NOW() WHERE id = $1",
    )
    .bind(box_id)
    .execute(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(
        json!({"deleted": true, "email": mailbox.email_address}),
    ))
}

pub async fn update_mailbox(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(box_id): Path<Uuid>,
    Json(req): Json<UpdateMailboxRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row = sqlx::query_as::<_, PrivateEmailBox>(
        r#"
        UPDATE private_email_boxes
        SET signature = COALESCE($3, signature),
            forwarding_enabled = COALESCE($4, forwarding_enabled),
            updated_at = NOW()
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(box_id)
    .bind(account_id)
    .bind(&req.signature)
    .bind(req.forwarding_enabled)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(mb) => Ok(Json(serde_json::to_value(&mb).unwrap())),
        None => Err(AppError::NotFound("Mailbox not found".into())),
    }
}

/// The public URL Mailgun forwards inbound mail to.
///
/// This was the literal `https://coreswift.net/api/v1/webhooks/mailgun/inbound` until kanban
/// t_0adbad21: `coreswift.net` is in NO vhost on this fleet (origin 404, edge 302 to a parked
/// page), so every route this app created forwarded inbound mail to a dead host. The app's own
/// public origin is [`crate::email::APP_URL`], and this path is listed in
/// [`crate::auth::route_policy`]'s public routes — Mailgun arrives with no session, so the
/// receiver has to be reachable anonymously.
fn inbound_webhook_url() -> String {
    format!("{}/api/v1/webhooks/mailgun/inbound", crate::email::APP_URL)
}

/// Mailgun's own words about a refused call.
///
/// Its `{"message": "..."}` when it sends one, the (trimmed, capped) body otherwise, plus the
/// status code so a 401 and a 404 stay distinguishable to the operator.
async fn mailgun_refusal(resp: reqwest::Response) -> String {
    let status = resp.status().as_u16();
    let body = resp.text().await.unwrap_or_default();
    let sentence = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.trim().chars().take(300).collect());
    if sentence.trim().is_empty() {
        format!("HTTP {}", status)
    } else {
        format!("{} (HTTP {})", sentence.trim(), status)
    }
}

/// Does a Mailgun route expression select mail for `domain`?
///
/// Mailgun expressions are `match_recipient("…regex…")`, optionally joined by `and`/`or`. The
/// shapes this fleet writes are `.*@domain`, `.*@(a|b|c)` and an exact address, so the recipient
/// patterns are read out of the expression with the escapes removed and compared by domain. A
/// regex engine is not a dependency of this crate, and pulling one in to answer this single
/// question would be the tail wagging the dog: a pattern this cannot read answers "not covered",
/// which creates a redundant route instead of silently claiming coverage that does not exist.
fn expression_selects_domain(expression: &str, domain: &str) -> bool {
    expression
        .replace("\\.", ".")
        .split(['"', '\'', '(', ')', '|', ' ', ','])
        .map(|token| token.trim().trim_end_matches('$'))
        .map(|token| token.strip_prefix(".*@").unwrap_or(token))
        .any(|token| token.eq_ignore_ascii_case(domain))
}

/// Ensure this domain has ONE inbound route forwarding to [`inbound_webhook_url`].
///
/// Routes are per DOMAIN, not per mailbox, and Mailgun's route quota is small (measured 2026-11-10:
/// 5, already full on this account), so the mailbox create must NOT add a route per mailbox:
///
/// * a route that already selects this domain and forwards to this build's receiver is REUSED —
///   which is the normal case, because the account holds one catch-all route for every CoreSwift
///   business (`match_recipient(".*@(swiftsoftware\.net|swiftimpactsolutions\.com|…")`);
/// * a route that selects this domain but forwards somewhere else (the retired `coreswift.net`
///   target) is REPAIRED in place;
/// * only a domain with no route at all gets a new one.
///
/// Every error carries Mailgun's own sentence; the caller answers it as a 4xx.
async fn ensure_mailgun_route(
    base_url: &str,
    api_key: &str,
    domain: &str,
) -> Result<String, String> {
    let client = reqwest::Client::new();
    let webhook_url = inbound_webhook_url();
    // Escaped dots: `match_recipient` takes a REGEX, so an unescaped `.` also matches
    // `you@zaarhubXcom`.
    let expression = format!(r#"match_recipient(".*@{}")"#, domain.replace('.', r"\."));
    let forward_action = format!(r#"forward("{}")"#, webhook_url);
    let description = format!("CoreSwift inbound for {}", domain);

    let list = client
        .get(format!("{}/v3/routes", base_url))
        .basic_auth("api", Some(api_key))
        .query(&[("limit", "100")])
        .send()
        .await
        .map_err(|e| format!("Mailgun route lookup failed: {}", e))?;

    if !list.status().is_success() {
        return Err(format!(
            "Mailgun refused the inbound route lookup: {}",
            mailgun_refusal(list).await
        ));
    }

    let body: serde_json::Value = list
        .json()
        .await
        .map_err(|e| format!("Mailgun route list was not JSON: {}", e))?;
    let routes: &[serde_json::Value] = body
        .get("items")
        .and_then(|items| items.as_array())
        .map(|items| items.as_slice())
        .unwrap_or(&[]);

    let selects = |route: &serde_json::Value| {
        route
            .get("expression")
            .and_then(|e| e.as_str())
            .map(|e| expression_selects_domain(e, domain))
            .unwrap_or(false)
    };
    let forwards_here = |route: &serde_json::Value| {
        route
            .get("actions")
            .and_then(|actions| actions.as_array())
            .map(|actions| {
                actions.iter().any(|a| {
                    a.as_str()
                        .map(|s| s.contains(&webhook_url))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    };
    let route_id = |route: &serde_json::Value| {
        route
            .get("id")
            .and_then(|id| id.as_str())
            .map(str::to_string)
    };

    // Covered already — including by the fleet's catch-all route. No provider write at all.
    if let Some(covered) = routes.iter().find(|r| selects(r) && forwards_here(r)) {
        return Ok(route_id(covered).unwrap_or_default());
    }

    // A route that selects this domain but forwards elsewhere is the pre-fix shape: repair it in
    // place rather than adding a competing route.
    let repair_id = routes.iter().find(|r| selects(r)).and_then(route_id);

    let form = [
        ("priority", "10"),
        ("description", description.as_str()),
        ("expression", expression.as_str()),
        ("action", forward_action.as_str()),
    ];
    let resp = match &repair_id {
        Some(route_id) => client
            .put(format!("{}/v3/routes/{}", base_url, route_id))
            .basic_auth("api", Some(api_key))
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("Mailgun route update failed: {}", e))?,
        None => client
            .post(format!("{}/v3/routes", base_url))
            .basic_auth("api", Some(api_key))
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("Mailgun route creation failed: {}", e))?,
    };

    if !resp.status().is_success() {
        let action = if repair_id.is_some() {
            "update"
        } else {
            "creation"
        };
        return Err(format!(
            "Mailgun refused the inbound route {} for {}: {}",
            action,
            domain,
            mailgun_refusal(resp).await
        ));
    }

    Ok(repair_id.unwrap_or_default())
}
