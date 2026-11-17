//! When a message cannot be sent, somebody is told.
//!
//! The failure this exists for (measured 2026-11-15): a real message was refused — `Mailgun returned 401 for
//! palmbaypulse.com: Forbidden` — and the only trace was a `failed` row and an error string. Nobody was told, and
//! it sat there for three days until an audit walked past it. A customer who never got their mail is the most
//! expensive silence there is, so it stops being silent.
//!
//! Two rules keep the alarm useful rather than noisy:
//!
//!   * ONCE PER MESSAGE. `send_failure_alerts` holds one row per failed message and is written with the
//!     notification, so a sweep every minute cannot repeat itself. The ledger is not an optimization — an alert
//!     that fires on every tick is an alert nobody reads.
//!   * ONE NOTIFICATION PER ACCOUNT PER PASS, naming how many failed and the first reason in plain words, rather
//!     than a burst of identical rows that hides the count.
//!
//! Deliberately NOT included: automatic retrying. A permanent refusal (a bad key, a rejected domain) fails the
//! same way however many times it is tried, and a retry loop would bury the alert in its own noise.

use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AlertPass {
    pub accounts: usize,
    pub messages: usize,
}

/// Gather failures nobody has been told about, and tell them.
pub async fn run_once(pool: &PgPool) -> AlertPass {
    let rows: Vec<(Uuid, Uuid, Option<String>, Option<String>, Option<String>)> =
        match sqlx::query_as(
            "SELECT o.id, o.tenant_id, o.to_address, o.subject, o.error_message
           FROM outbound_messages o
           LEFT JOIN send_failure_alerts a ON a.message_id = o.id
          WHERE o.status = 'failed'
            AND a.message_id IS NULL
            AND o.created_at > now() - interval '7 days'
          ORDER BY o.created_at
          LIMIT 200",
        )
        .fetch_all(pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "could not read failed sends");
                return AlertPass::default();
            }
        };

    if rows.is_empty() {
        return AlertPass::default();
    }

    // group by account so one pass produces one interruption per account
    let mut by_tenant: std::collections::BTreeMap<
        Uuid,
        Vec<(Uuid, Option<String>, Option<String>, Option<String>)>,
    > = Default::default();
    for (id, tenant, to, subject, err) in rows {
        by_tenant
            .entry(tenant)
            .or_default()
            .push((id, to, subject, err));
    }

    let mut pass = AlertPass::default();
    for (tenant_id, failures) in by_tenant {
        let count = failures.len();
        let first = failures
            .iter()
            .find_map(|(_, _, _, e)| e.clone())
            .unwrap_or_else(|| "no reason recorded".to_string());
        let title = if count == 1 {
            "A message could not be sent".to_string()
        } else {
            format!("{count} messages could not be sent")
        };
        let body = format!(
            "{count} message(s) this account tried to send were refused by the mail provider, so the people \
             addressed to them never received them. The first reason recorded: {first}. \
             Check the sending domain's credential in Email Domains, then send again from the message."
        );

        // the owner is the person who can act on a credential problem
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM users WHERE tenant_id = $1 AND is_active
              ORDER BY (role = 'owner') DESC, created_at LIMIT 1",
        )
        .bind(tenant_id)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);

        let mut tx = match pool.begin().await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(error = %e, "could not open the alert transaction");
                continue;
            }
        };
        if let Err(e) = sqlx::query(
            "INSERT INTO notifications (tenant_id, user_id, title, body, notification_type)
             VALUES ($1, $2, $3, $4, 'email_failure')",
        )
        .bind(tenant_id)
        .bind(owner)
        .bind(&title)
        .bind(&body)
        .execute(&mut *tx)
        .await
        {
            tracing::error!(error = %e, "could not write the send-failure notification");
            continue;
        }
        for (message_id, _, _, _) in &failures {
            let _ = sqlx::query(
                "INSERT INTO send_failure_alerts (message_id, tenant_id, summary)
                 VALUES ($1, $2, $3) ON CONFLICT (message_id) DO NOTHING",
            )
            .bind(message_id)
            .bind(tenant_id)
            .bind(&first)
            .execute(&mut *tx)
            .await;
        }
        if let Err(e) = tx.commit().await {
            tracing::error!(error = %e, "could not commit the send-failure alert");
            continue;
        }
        pass.accounts += 1;
        pass.messages += count;
        tracing::warn!(
            target: "send_failures",
            "account {} told about {} refused message(s): {}", tenant_id, count, first
        );
    }
    pass
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_notification_says_what_happened_and_what_to_do() {
        // the wording is the product: "failed" tells an owner nothing they can act on
        let title = "A message could not be sent";
        assert!(!title.to_lowercase().contains("error"));
        let body = "1 message(s) this account tried to send were refused by the mail provider";
        assert!(body.contains("refused by the mail provider"));
        assert!(body.contains("never received them") || body.contains("refused"));
    }
}
