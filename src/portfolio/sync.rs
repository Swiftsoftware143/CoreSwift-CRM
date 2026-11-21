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
    // WHAT `tenant_id` MEANS HERE (fixed 2026-11-15, CEO finding on BV Premiere Products).
    //
    // A sibling's sync door creates ITS OWN tenant from this field. It used to carry CoreSwift's ADMIN
    // workspace id, which is the same value for every company — and in ADASwift that id is already Xride
    // Drivers' workspace, so a push either collided or wrote one company over another. The id sent now is the
    // COMPANY's own id: distinct per company, stable across re-pushes (so a sibling can upsert on it), and
    // incapable of naming a workspace that already exists somewhere else.
    //
    // `portfolio_id` stays the identity of the company itself; `owner_tenant_id` is carried for information
    // only, so a sibling can tell which CoreSwift account the push came from without ever using it as a key.
    let sibling_tenant_id = portfolio_id;
    let payload = json!({
        "action": action,
        "portfolio_id": portfolio_id.to_string(),
        "tenant_id": sibling_tenant_id.to_string(),
        "owner_tenant_id": tenant_id.to_string(),
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
    //   * MultiDirectory is the ONE exclusion: a portfolio company is not a directory tenant, and
    //     its tenants are the client businesses it serves.
    //   * ADASwift IS included (David, 2026-11-13, corrected live): *"even my personal company's needs an ADA
    //     widget"* — every portfolio company needs its own ADA workspace, which is what this row
    //     becomes there. (Its CONTACT connector stays retired; this is the control message.)
    let targets: &[(&str, u16)] = &[
        ("FunnelSwift", 8080),
        ("WorkflowSwift", 8085),
        ("ADASwift", 8087),
        ("IncentiveSwift", 8083),
        ("MissedCallRespondr", 8088),
        // XrideDispatch (2026-11-21): the dispatch app was created after this list was written, so a
        // portfolio company existed in the CRM and nowhere in dispatch — the owner's own businesses
        // stop being mirrored the moment a new sibling app ships unless somebody remembers this array.
        // Dispatch is a RECEIVER, not a snapshot: it upserts on `portfolio_id`, retires on a delete and
        // creates no account or key of its own (nothing logs into dispatch as a portfolio company).
        ("XrideDispatch", 8102),
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
                    // ONE LINE PER TARGET, URL AND STATUS INCLUDED (CEO, 2026-11-15): the first version logged
                    // success without the address, so "did the broadcast run at all?" could not be answered from
                    // the log. This line answers it for each app, and the ids make an id-contract mistake visible.
                    info!(
                        target: "portfolio_sync",
                        "portfolio-sync {} -> {} {} : HTTP {} (portfolio {} , sibling tenant {})",
                        action,
                        app_name,
                        url,
                        status.as_u16(),
                        portfolio_id,
                        sibling_tenant_id
                    );
                } else {
                    let body = resp.text().await.unwrap_or_default();
                    warn!(
                        target: "portfolio_sync",
                        "portfolio-sync {} -> {} {} : HTTP {} — body: {}",
                        action,
                        app_name,
                        url,
                        status.as_u16(),
                        body
                    );
                }
            }
            Err(e) => {
                error!(
                    target: "portfolio_sync",
                    "portfolio-sync {} -> {} {} : request failed — {}",
                    action, app_name, url, e
                );
            }
        }
    }
    info!(
        target: "portfolio_sync",
        "portfolio-sync {} for portfolio {} finished: {} target(s) attempted",
        action,
        portfolio_id,
        targets.len()
    );
}
