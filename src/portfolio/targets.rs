//! Delivery to a portfolio company's OWN integration targets (David, 2026-11-21).
//!
//! WHY THIS FILE EXISTS. The Portfolio screen has carried an "Integration Targets" form since the
//! portfolio module landed: name, provider (webhook / n8n / Zapier / Slack / generic), webhook URL, an
//! optional API key and a comma-separated event list. Every one of those rows was STORED AND NEVER
//! USED — nothing in the codebase read `integration_targets.webhook_url` and posted to it. Measured
//! 2026-11-21: the table held 0 rows, and the only outbound fan-out that existed was
//! `sync::broadcast_portfolio_sync`, which talks to the SIBLING APPS, not to a customer's endpoint.
//!
//! A form that accepts a URL and an API key and then quietly does nothing is worse than no form: the
//! owner believes their n8n/Zapier automation is wired, and the silence looks like the product is
//! broken. So the delivery is implemented on the one portfolio event that genuinely exists —
//! `portfolio.synced`, fired after a company is provisioned and broadcast.
//!
//! Rules:
//!   * only ACTIVE targets fire;
//!   * an empty `events` list means "everything" (an owner who adds a target and names no events
//!     expects it to fire);
//!   * the stored key travels as `Authorization: Bearer …` — it is the owner's own credential for
//!     their own endpoint, and it is read through `secret_box`, never printed;
//!   * one target failing never fails the sync: each is logged with its own outcome.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// The event name a provisioned-and-broadcast portfolio company emits.
pub const EVENT_SYNCED: &str = "portfolio.synced";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub attempted: usize,
    pub delivered: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Does this target want this event? An empty list means everything; `*` is accepted as well.
fn wants(events: &[String], event: &str) -> bool {
    events.is_empty()
        || events
            .iter()
            .any(|e| e.trim() == "*" || e.trim().eq_ignore_ascii_case(event))
}

/// Post one event to every active target belonging to `portfolio_company_id`.
pub async fn deliver_event(
    pool: &PgPool,
    portfolio_company_id: Uuid,
    event: &str,
    payload: &Value,
) -> DeliveryReport {
    let mut report = DeliveryReport::default();

    let rows: Vec<(Uuid, Uuid, String, String, String, Vec<String>)> = match sqlx::query_as(
        "SELECT id, tenant_id, name, webhook_url, COALESCE(api_key, ''), COALESCE(events, '{}')
           FROM integration_targets
          WHERE portfolio_company_id = $1 AND is_active",
    )
    .bind(portfolio_company_id)
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "portfolio_targets", error = %e, "could not read integration targets");
            return report;
        }
    };

    if rows.is_empty() {
        return report;
    }

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "portfolio_targets", error = %e, "could not build the HTTP client");
            return report;
        }
    };

    for (id, tenant_id, name, url, sealed_key, events) in rows {
        if !wants(&events, event) {
            report.skipped += 1;
            continue;
        }
        report.attempted += 1;

        // The credential is the customer's own, sealed at rest; it is opened only to send it.
        let key = if sealed_key.trim().is_empty() {
            String::new()
        } else {
            crate::secret_box::open(tenant_id, &sealed_key)
        };

        let body = json!({
            "event": event,
            "portfolio_company_id": portfolio_company_id,
            "data": payload,
        });

        let mut req = client
            .post(&url)
            .header("content-type", "application/json")
            .header("user-agent", "CoreSwift-PortfolioTargets/1.0")
            .header("x-coreswift-event", event)
            .json(&body);
        if !key.is_empty() {
            req = req.header("authorization", format!("Bearer {key}"));
        }

        match req.send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    report.delivered += 1;
                    tracing::info!(
                        target: "portfolio_targets",
                        target_id = %id, target_name = %name, %status, %event,
                        "portfolio event delivered"
                    );
                } else {
                    report.failed += 1;
                    tracing::warn!(
                        target: "portfolio_targets",
                        target_id = %id, target_name = %name, %status, %event,
                        "portfolio event REFUSED by the endpoint"
                    );
                }
            }
            Err(e) => {
                report.failed += 1;
                tracing::warn!(
                    target: "portfolio_targets",
                    target_id = %id, target_name = %name, %event, error = %e,
                    "portfolio event could not be delivered"
                );
            }
        }
    }

    report
}
