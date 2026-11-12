//! The customer's BILL, as a read and as a gate (kanban t_5d1d6d31).
//!
//! WHY THIS MODULE EXISTS. David's rule for CoreSwift: Inbox is a STANDALONE door at $1/mo
//! ($10/yr) and nothing else. Upgrading to a CRM plan CANCELS that $1 in the same action — a
//! customer must never be billed $30, must never hold two subscriptions, and must never be
//! surprised by a proration. Downgrading back to Inbox returns to $1 with the mailbox and every
//! message intact.
//!
//! Two instruments carry that:
//!
//! * `plan_lines` (migration 144) is an append-only ledger with a PARTIAL UNIQUE INDEX on
//!   `(tenant_id) WHERE ended_at IS NULL`, i.e. a workspace can hold at most ONE open line. Two
//!   live subscriptions are not something code has to remember to avoid — they are
//!   unrepresentable. The trigger on `tenant_plans` closes the old line and opens the new one in
//!   the same statement, so no writer (including a hand SQL correction) can drift the ledger.
//!
//! * [`summary`] is the ONE read that answers "what does this workspace pay, and what ended when
//!   it last changed?". It derives the current charge from `tenant_plans` (the entitlement row)
//!   and the closed line from the ledger, so the console can say "$29/month — your Inbox
//!   subscription ended when you upgraded" from facts, not from a guess.
//!
//! [`plan_for_write`] is the gate that keeps a plan with no working payment page off a customer's
//! account: `plans.is_active` is false for Inbox until a buyer can actually pay, and every plan
//! writer asks this function before it writes.

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, State},
    Json,
};
use chrono::{DateTime, Utc};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

/// A plan that may be written onto a workspace.
#[derive(Debug, sqlx::FromRow)]
pub struct WritablePlan {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub price_monthly: Decimal,
}

/// Resolve a plan the caller asked to assign, and REFUSE one that is not on sale.
///
/// The Inbox plan ships `is_active = false` on purpose: until a buyer can really pay, putting a
/// workspace on it would hand a customer a plan with a dead checkout button. The refusal names the
/// plan and says nothing changed — a plan writer must never half-apply anything.
pub async fn plan_for_write(
    db: &PgPool,
    slug: Option<&str>,
    id: Option<Uuid>,
) -> Result<WritablePlan, AppError> {
    let row: Option<(Uuid, String, String, bool, Decimal)> = match (slug, id) {
        (Some(s), _) => sqlx::query_as(
            "SELECT id, name, slug, is_active, price_monthly FROM plans WHERE slug = $1 LIMIT 1",
        )
        .bind(s)
        .fetch_optional(db)
        .await?,
        (None, Some(pid)) => {
            sqlx::query_as(
                "SELECT id, name, slug, is_active, price_monthly FROM plans WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(db)
            .await?
        }
        (None, None) => {
            return Err(AppError::Validation(
                "plan_slug or plan_id is required".to_string(),
            ))
        }
    };

    let (id, name, plan_slug, is_active, price_monthly) = row.ok_or_else(|| {
        AppError::NotFound(match (slug, id) {
            (Some(s), _) => format!("No plan with slug '{s}'"),
            (None, Some(pid)) => format!("No plan with id '{pid}'"),
            _ => "No plan".to_string(),
        })
    })?;

    if !is_active {
        return Err(AppError::Conflict(format!(
            "The {name} plan is not on sale yet: it has no working payment page, so nobody can be \
             put on it. Nothing was changed."
        )));
    }

    Ok(WritablePlan {
        id,
        name,
        slug: plan_slug,
        price_monthly,
    })
}

#[derive(Debug, sqlx::FromRow)]
struct CurrentPlanRow {
    slug: String,
    name: String,
    price_monthly: Decimal,
    price_yearly: Decimal,
    billing_cycle: String,
    status: String,
    current_period_ends_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct OpenLineRow {
    plan_slug: String,
    price_monthly: Decimal,
    price_yearly: Decimal,
    billing_cycle: String,
    started_at: DateTime<Utc>,
}

#[derive(Debug, sqlx::FromRow)]
struct ClosedLineRow {
    plan_slug: String,
    price_monthly: Decimal,
    billing_cycle: String,
    ended_at: DateTime<Utc>,
    end_reason: Option<String>,
}

fn money(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

/// Slug + monthly price of one plan — for audit trails and the bill.
pub async fn plan_money(db: &PgPool, id: Uuid) -> Result<Option<(String, f64)>, AppError> {
    let row: Option<(String, Decimal)> =
        sqlx::query_as("SELECT slug, price_monthly FROM plans WHERE id = $1")
            .bind(id)
            .fetch_optional(db)
            .await?;
    Ok(row.map(|(slug, price)| (slug, money(price))))
}

/// GET /api/billing/summary — what this workspace pays, and what ended when it last changed.
///
/// Read-only and tenant-scoped (the tenant comes from `Claims.aid`, never from the path). The
/// console's "Your bill" card is the only caller, and it must be able to say three plain things
/// from this answer alone: the one amount due, that there is exactly one subscription, and which
/// line ended (with a date) if the plan changed.
pub async fn summary(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl axum::response::IntoResponse> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let current: Option<CurrentPlanRow> = sqlx::query_as(
        r#"SELECT p.slug, p.name, p.price_monthly, p.price_yearly, tp.billing_cycle, tp.status,
                  tp.current_period_ends_at
             FROM tenant_plans tp
             JOIN plans p ON p.id = tp.plan_id
            WHERE tp.tenant_id = $1"#,
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    let open: Vec<OpenLineRow> = sqlx::query_as(
        r#"SELECT plan_slug, price_monthly, price_yearly, billing_cycle, started_at
             FROM plan_lines
            WHERE tenant_id = $1 AND ended_at IS NULL
            ORDER BY started_at ASC"#,
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    // The last PAID line that ended. A $0 line (the Free seat a workspace starts on) is not a
    // subscription anything was ever charged for, so it is not "the line that ended".
    let previous: Option<ClosedLineRow> = sqlx::query_as(
        r#"SELECT plan_slug, price_monthly, billing_cycle, ended_at, end_reason
             FROM plan_lines
            WHERE tenant_id = $1 AND ended_at IS NOT NULL
              AND (price_monthly > 0 OR price_yearly > 0)
            ORDER BY ended_at DESC
            LIMIT 1"#,
    )
    .bind(tid)
    .fetch_optional(&s.db)
    .await?;

    let (plan_json, amount_due, period, cycle) = match &current {
        Some(p) => {
            let (amount, period) = if p.billing_cycle == "yearly" {
                (money(p.price_yearly), "year")
            } else {
                (money(p.price_monthly), "month")
            };
            (
                json!({
                    "slug": p.slug,
                    "name": p.name,
                    "price_monthly": money(p.price_monthly),
                    "price_yearly": money(p.price_yearly),
                    "billing_cycle": p.billing_cycle,
                    "status": p.status,
                }),
                amount,
                period,
                Some(p.billing_cycle.clone()),
            )
        }
        None => (Value::Null, 0.0, "month", None),
    };

    let current_price = current
        .as_ref()
        .map(|p| money(p.price_monthly))
        .unwrap_or(0.0);

    let previous_json = previous.as_ref().map(|l| {
        // Whether the move was up or down is decided by the two PRICES, never by a plan name.
        // "canceled" is the ledger's own word for a subscription that simply ended.
        let direction = match l.end_reason.as_deref() {
            Some("canceled") => "canceled",
            _ if money(l.price_monthly) < current_price => "upgrade",
            _ if money(l.price_monthly) > current_price => "downgrade",
            _ => "change",
        };
        json!({
            "plan_slug": l.plan_slug,
            "price_monthly": money(l.price_monthly),
            "billing_cycle": l.billing_cycle,
            "ended_at": l.ended_at,
            "end_reason": l.end_reason,
            "move": direction,
        })
    });

    let open_lines = open.len() as i64;

    Ok(Json(json!({
        "plan": plan_json,
        "billing_cycle": cycle,
        "amount_due": amount_due,
        "amount_due_period": period,
        "current_period_ends_at": current.as_ref().and_then(|p| p.current_period_ends_at),
        "currency": "USD",
        // The invariant, reported: at most one open line (the DB index makes 2 impossible).
        "open_lines": open_lines,
        "one_subscription": open_lines <= 1,
        "lines": open.iter().map(|l| json!({
            "plan_slug": l.plan_slug,
            "price_monthly": money(l.price_monthly),
            "price_yearly": money(l.price_yearly),
            "billing_cycle": l.billing_cycle,
            "started_at": l.started_at,
        })).collect::<Vec<_>>(),
        "previous_line": previous_json,
    })))
}
