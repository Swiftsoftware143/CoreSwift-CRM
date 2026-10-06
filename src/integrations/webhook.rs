//! Outbound delivery for the tenant-facing **Outgoing Webhook Endpoints** feature.
//!
//! Registration/CRUD lives in `src/integrations/handlers.rs` (`/api/integrations/webhooks`, its own
//! `webhooks` plan module — Enterprise/Agency). This module is the SENDER.
//!
//! Until 2026-10-06 this file was a `tracing::debug!` stub with ZERO callers while `public/guide.html`
//! promised that "CoreSwift will POST events to" a registered endpoint — a served promise with no
//! implementation behind it (kanban t_ee3c086f). `dispatch_webhook` is now called from the one place an
//! event enters the app (`src/events/handlers.rs`, the `POST /api/events/ingest/:source` ingress, next
//! to `dispatch_automation`).
//!
//! Delivery contract (mirrored for integrators in `public/guide.html`):
//!   * POST, `content-type: application/json`, body
//!     `{"event","tenant_id","webhook_id","delivered_at","data"}` where `data` is the event payload.
//!   * `X-CoreSwift-Event: <event_type>`
//!   * `X-CoreSwift-Delivery: <endpoint id>`, `X-CoreSwift-Delivery-Attempt: <n>`
//!   * `X-CoreSwift-Signature: t=<unix seconds>,v1=<hex hmac_sha256(secret, "<t>.<raw body>")>`
//!     only when the endpoint has a signing secret (unsealed from `webhook_endpoints.secret`).
//!   * `events` filter: an endpoint with an EMPTY `events` array subscribes to EVERY event; a
//!     non-empty array subscribes to exactly the listed event types. (The column's default is `'{}'`,
//!     so "empty means all" is what makes a default-created endpoint deliver anything at all.)
//!   * `is_active = false` endpoints receive nothing.
//!   * `retry_count` retries (exponential backoff, capped) after the first attempt, each attempt bounded
//!     by `timeout_ms`. A 2xx is success; anything else (3xx included — redirects are NOT followed) is a
//!     failed attempt. Success stamps `last_triggered_at` and clears `failure_count`; failure increments
//!     it. Fire-and-forget: delivery never blocks or fails the caller's own request.

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use sqlx::PgPool;
use std::time::Duration;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// The subset of a `webhook_endpoints` row the sender needs. Selected with COALESCE so a legacy row
/// whose NULLable `retry_count`/`timeout_ms`/`secret` are still NULL can never fail the decode and
/// silence delivery (the columns are `DEFAULT 3`/`5000` but NULL is legal in the live schema).
#[derive(Debug, sqlx::FromRow)]
struct Endpoint {
    id: Uuid,
    tenant_id: Uuid,
    url: String,
    secret: Option<String>,
    retry_count: i32,
    timeout_ms: i32,
}

/// `hmac_sha256(secret, "{t}.{raw body}")`, rendered as the header value documented above.
fn sign(secret: &str, timestamp: i64, body: &[u8]) -> Option<String> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body);
    Some(format!(
        "t={},v1={}",
        timestamp,
        hex::encode(mac.finalize().into_bytes())
    ))
}

/// Deliver `event`/`payload` to every ACTIVE endpoint this tenant has registered for that event.
/// Fire-and-forget — logs failures, never blocks or fails its caller.
pub async fn dispatch_webhook(db: &PgPool, tenant_id: Uuid, event: &str, payload: &Value) {
    let endpoints = match sqlx::query_as::<_, Endpoint>(
        r#"SELECT id, tenant_id, url, secret,
                  COALESCE(retry_count, 3) AS retry_count,
                  COALESCE(timeout_ms, 5000) AS timeout_ms
             FROM webhook_endpoints
            WHERE tenant_id = $1
              AND is_active = true
              AND (COALESCE(array_length(events, 1), 0) = 0 OR $2 = ANY(events))
            ORDER BY created_at ASC"#,
    )
    .bind(tenant_id)
    .bind(event)
    .fetch_all(db)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, tenant = %tenant_id, event = %event, "could not load webhook endpoints");
            return;
        }
    };

    if endpoints.is_empty() {
        return;
    }
    tracing::debug!(
        tenant = %tenant_id,
        event = %event,
        endpoints = endpoints.len(),
        "dispatching outbound webhooks"
    );

    for endpoint in endpoints {
        let db = db.clone();
        let event = event.to_string();
        let payload = payload.clone();
        tokio::spawn(async move {
            deliver(&db, &endpoint, &event, &payload).await;
        });
    }
}

async fn deliver(db: &PgPool, endpoint: &Endpoint, event: &str, payload: &Value) {
    // A row whose url is not an http(s) URL (a canary/placeholder value) must never be dialled.
    let url = match reqwest::Url::parse(endpoint.url.trim()) {
        Ok(u) if u.scheme() == "http" || u.scheme() == "https" => u,
        _ => {
            tracing::warn!(endpoint = %endpoint.id, "webhook endpoint url is not an http(s) URL; skipping");
            return;
        }
    };

    let body = json!({
        "event": event,
        "tenant_id": endpoint.tenant_id,
        "webhook_id": endpoint.id,
        "delivered_at": chrono::Utc::now().to_rfc3339(),
        "data": payload,
    });
    let body_bytes = match serde_json::to_vec(&body) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, endpoint = %endpoint.id, "could not serialise webhook body");
            return;
        }
    };

    let secret = endpoint
        .secret
        .as_deref()
        .map(|stored| crate::secret_box::open(endpoint.tenant_id, stored))
        .unwrap_or_default();
    let timestamp = chrono::Utc::now().timestamp();
    let signature = (!secret.trim().is_empty())
        .then(|| sign(&secret, timestamp, &body_bytes))
        .flatten();

    // Redirects are NOT followed: a registered public host that 30x-es to loopback must not become a
    // free hop past the endpoint's own destination.
    let client = match reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "could not build webhook delivery client");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms.clamp(1, 120_000) as u64);
    let attempts = endpoint.retry_count.clamp(0, 10) + 1;

    let mut delivered = false;
    let mut last_error = String::from("not attempted");
    for attempt in 1..=attempts {
        if attempt > 1 {
            let backoff = (250u64 << (attempt - 2).min(5)).min(5_000);
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }
        let mut request = client
            .post(url.clone())
            .header("content-type", "application/json")
            .header("x-coreswift-event", event)
            .header("x-coreswift-delivery", endpoint.id.to_string())
            .header("x-coreswift-delivery-attempt", attempt.to_string())
            .timeout(timeout)
            .body(body_bytes.clone());
        if let Some(sig) = signature.as_deref() {
            request = request.header("x-coreswift-signature", sig);
        }
        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                delivered = true;
                break;
            }
            Ok(resp) => last_error = format!("HTTP {}", resp.status()),
            Err(e) => last_error = e.to_string(),
        }
        tracing::debug!(endpoint = %endpoint.id, attempt, error = %last_error, "webhook delivery attempt failed");
    }

    let outcome = if delivered {
        sqlx::query(
            "UPDATE webhook_endpoints SET last_triggered_at = NOW(), failure_count = 0 WHERE id = $1",
        )
        .bind(endpoint.id)
        .execute(db)
        .await
    } else {
        sqlx::query(
            "UPDATE webhook_endpoints SET failure_count = COALESCE(failure_count, 0) + 1 WHERE id = $1",
        )
        .bind(endpoint.id)
        .execute(db)
        .await
    };
    if let Err(e) = outcome {
        tracing::warn!(error = %e, endpoint = %endpoint.id, "could not record webhook delivery outcome");
    }

    if delivered {
        tracing::info!(endpoint = %endpoint.id, event = %event, "outbound webhook delivered");
    } else {
        tracing::warn!(endpoint = %endpoint.id, event = %event, error = %last_error, "outbound webhook failed after retries");
    }
}
