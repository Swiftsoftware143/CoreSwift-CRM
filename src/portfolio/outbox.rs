//! The outbox drain: provisioning and broadcasting for companies nobody created through the API.
//!
//! The trigger in migration 174 writes a row for every portfolio company insert or edit, so a company created by
//! SQL, a migration or a script is provisioned exactly like one created by hand. This module drains those rows:
//! provision locally first (tenant, owner, one key, welcome mail — all a top-up, so a second pass changes
//! nothing), then broadcast to the five siblings that mirror a portfolio company.
//!
//! A failure is retried, not lost: the row stays pending with its attempt count and the error text, and gives up
//! after five tries with the reason recorded. Silence was the original bug; a row that can be read afterwards is
//! the fix.

use sqlx::PgPool;
use uuid::Uuid;

use super::provision::{self, Company};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DrainReport {
    pub processed: usize,
    pub provisioned: usize,
    pub failed: usize,
    pub gave_up: usize,
}

const MAX_ATTEMPTS: i32 = 5;

/// Drain up to `limit` pending companies.
pub async fn drain(pool: &PgPool, sync_key: &str, limit: i64) -> DrainReport {
    let mut report = DrainReport::default();

    let pending: Vec<(Uuid, Uuid, String, String, String, Option<String>, i32)> =
        match sqlx::query_as(
            "SELECT o.id, c.id, c.name, c.slug, COALESCE(c.email, ''), c.description, o.attempts
           FROM portfolio_sync_outbox o
           JOIN portfolio_companies c ON c.id = o.portfolio_id
          WHERE o.processed_at IS NULL
          ORDER BY o.created_at
          LIMIT $1",
        )
        .bind(limit)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::error!(target: "portfolio_sync", error = %e, "could not read the portfolio outbox");
                return report;
            }
        };

    for (outbox_id, company_id, name, slug, email, description, attempts) in pending {
        let company = Company {
            id: company_id,
            name: &name,
            slug: &slug,
            email: &email,
        };

        // a company with no address cannot have an owner: record why instead of provisioning half of one
        if email.trim().is_empty() {
            let _ = sqlx::query(
                "UPDATE portfolio_sync_outbox
                    SET processed_at = now(), last_error = 'no email address on the company'
                  WHERE id = $1",
            )
            .bind(outbox_id)
            .execute(pool)
            .await;
            report.gave_up += 1;
            tracing::warn!(target: "portfolio_sync", "portfolio {} has no email; nothing to provision", company_id);
            continue;
        }

        match provision::provision(pool, sync_key, &company, false, false).await {
            Ok(done) => {
                report.provisioned += 1;
                // Broadcast through the app's own door, so the payload and its per-target logging stay in one
                // place. The action is `create` only when this pass made the account; a re-sync is an update.
                let action = if done.user_created {
                    "create"
                } else {
                    "update"
                };
                super::sync::broadcast_portfolio_sync(
                    action,
                    company_id,
                    done.tenant_id.unwrap_or(company_id),
                    &name,
                    &slug,
                    &email,
                    description.as_deref().unwrap_or(""),
                    sync_key.to_string(),
                )
                .await;
                let _ = sqlx::query(
                    "UPDATE portfolio_sync_outbox SET processed_at = now(), attempts = attempts + 1 WHERE id = $1",
                )
                .bind(outbox_id)
                .execute(pool)
                .await;
                report.processed += 1;
            }
            Err(e) => {
                let attempts = attempts + 1;
                if attempts >= MAX_ATTEMPTS {
                    let _ = sqlx::query(
                        "UPDATE portfolio_sync_outbox
                            SET attempts = $2, last_error = $3, processed_at = now()
                          WHERE id = $1",
                    )
                    .bind(outbox_id)
                    .bind(attempts)
                    .bind(format!("{e}"))
                    .execute(pool)
                    .await;
                    report.gave_up += 1;
                } else {
                    let _ = sqlx::query(
                        "UPDATE portfolio_sync_outbox SET attempts = $2, last_error = $3 WHERE id = $1",
                    )
                    .bind(outbox_id)
                    .bind(attempts)
                    .bind(format!("{e}"))
                    .execute(pool)
                    .await;
                    report.failed += 1;
                }
                tracing::warn!(
                    target: "portfolio_sync",
                    "portfolio {} could not be provisioned (attempt {}): {e}", company_id, attempts
                );
            }
        }
    }

    if report != DrainReport::default() {
        tracing::info!(
            target: "portfolio_sync",
            "outbox drain: {} processed, {} provisioned, {} retrying, {} given up",
            report.processed, report.provisioned, report.failed, report.gave_up
        );
    }
    report
}
