use reqwest::Client;
use serde_json::json;
use tracing::{error, info, warn};

/// Broadcast a portfolio company change to all sister apps.
/// Each app receives the sync via its POST /api/v1/admin/portfolio-sync endpoint,
/// authenticated by x-internal-key header.
pub async fn broadcast_portfolio_sync(
    action: &str,
    portfolio_id: uuid::Uuid,
    tenant_id: uuid::Uuid,
    name: &str,
    slug: &str,
    email: &str,
    description: &str,
    internal_sync_key: String,
) {
    let payload = json!({
        "action": action,
        "portfolio_id": portfolio_id.to_string(),
        "tenant_id": tenant_id.to_string(),
        "name": name,
        "slug": slug,
        "email": email,
        "description": description,
    });

    let client = Client::new();

    // Where a portfolio company is mirrored.
    //
    // Owner's rule (David, 2026-11-13): one owner, several businesses — a portfolio company created
    // here belongs in EVERY sibling app...
    //
    //   * MultiDirectory is deliberately ABSENT: a portfolio company is not a directory tenant, and
    //     its tenants are the client businesses it serves.
    //   * ADASwift is deliberately ABSENT too: it is a service SwiftImpact runs, not a place a
    //     portfolio company exists as an account. Its connector is retired on the same reasoning.
    let targets: &[(&str, u16)] = &[
        ("FunnelSwift", 8080),
        ("WorkflowSwift", 8085),
        ("IncentiveSwift", 8083),
        ("MissedCallRespondr", 8088),
    ];

    for (app_name, port) in targets {
        let url = format!("http://127.0.0.1:{}/api/v1/internal/portfolio-sync", port);
        match client
            .post(&url)
            .header("x-internal-key", &internal_sync_key)
            .header("Content-Type", "application/json")
            .json(&payload)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    info!(
                        "Portfolio sync ({}) broadcast to {} ({}): OK",
                        action, app_name, port
                    );
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    warn!(
                        "Portfolio sync ({}) to {} ({}): HTTP {} — {}",
                        action,
                        app_name,
                        port,
                        status.as_u16(),
                        body
                    );
                }
            }
            Err(e) => {
                error!(
                    "Portfolio sync ({}) to {} ({}): failed — {}",
                    action, app_name, port, e
                );
            }
        }
    }
}
