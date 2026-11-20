//! Deal reporting — win rate, average days to close, deals rotting in a stage, and the stage
//! totals the board shows.
//!
//! ## Money is never added across currencies
//! Every number here is computed for ONE currency at a time (`GROUP BY currency`), and the report
//! list carries one entry per (pipeline, currency). A workspace selling in USD and EUR gets two
//! report rows, never one row whose total is meaningless. The converting "reporting currency"
//! equivalent is a later layer (card t_44c1aaa4); until it exists the honest answer is the
//! per-currency one, because a wrong total is worse than two right ones (David, 2026-11-20).

use sqlx::{PgPool, Row};
use uuid::Uuid;

use super::models::{DealReport, ReportingTotals, StageTotal};
use crate::errors::AppError;

#[derive(Debug, sqlx::FromRow)]
struct HeadTotals {
    open_count: i64,
    open_value: f64,
    open_weighted: f64,
    won_count: i64,
    won_value: f64,
    lost_count: i64,
    lost_value: f64,
    avg_days_to_close: f64,
    rotting_count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct StageRow {
    stage_id: Uuid,
    stage_name: String,
    position: i32,
    probability: i32,
    is_won: bool,
    is_lost: bool,
    deal_count: i64,
    total_value: f64,
    weighted_value: f64,
}

#[derive(Debug, sqlx::FromRow)]
struct ReportingRow {
    open_count: i64,
    open_value: f64,
    open_weighted: f64,
    won_count: i64,
    won_value: f64,
    lost_count: i64,
    lost_value: f64,
    unconverted_count: i64,
}

/// One report per (pipeline, currency) the workspace actually holds deals in.
pub async fn build_report(
    state: &crate::AppState,
    tenant: Uuid,
    pipeline_id: Option<Uuid>,
    rot_days: i64,
) -> Result<Vec<DealReport>, AppError> {
    let pipelines: Vec<(Uuid, String)> = match pipeline_id {
        Some(pid) => {
            let row: Option<(Uuid, String)> = sqlx::query_as(
                "SELECT id, name FROM pipelines WHERE id = $1 AND tenant_id = $2",
            )
            .bind(pid)
            .bind(tenant)
            .fetch_optional(&state.db)
            .await?;
            vec![row.ok_or_else(|| AppError::NotFound(format!("Pipeline {pid} not found")))?]
        }
        None => sqlx::query_as(
            "SELECT id, name FROM pipelines WHERE tenant_id = $1 AND is_active ORDER BY is_default DESC, created_at",
        )
        .bind(tenant)
        .fetch_all(&state.db)
        .await?,
    };

    let mut out = Vec::new();
    for (pid, pname) in pipelines {
        // ── the reporting-currency answer, computed ONCE per pipeline ────────────────────────
        // Every deal contributes `value` (when it already trades in the reporting currency) or
        // `value * its OWN stored rate`. The stored rate is what makes a past quarter stable:
        // nothing here reads today's market.
        let reporting_currency = super::fx::workspace_reporting_currency(&state.db, tenant)
            .await?
            .unwrap_or_default();
        let rep = if reporting_currency.is_empty() {
            ReportingTotals {
                currency: String::new(),
                open_count: 0,
                open_value: 0.0,
                open_weighted_value: 0.0,
                won_count: 0,
                won_value: 0.0,
                lost_count: 0,
                lost_value: 0.0,
                win_rate: 0.0,
                unconverted_count: 0,
            }
        } else {
            let row: ReportingRow = sqlx::query_as(
                "SELECT
                    COUNT(*) FILTER (WHERE NOT x.is_won AND NOT x.is_lost) AS open_count,
                    COALESCE(SUM(x.conv) FILTER (WHERE NOT x.is_won AND NOT x.is_lost),0)::float8 AS open_value,
                    COALESCE(SUM(x.conv * COALESCE(x.probability,0) / 100.0)
                             FILTER (WHERE NOT x.is_won AND NOT x.is_lost),0)::float8 AS open_weighted,
                    COUNT(*) FILTER (WHERE x.is_won) AS won_count,
                    COALESCE(SUM(x.conv) FILTER (WHERE x.is_won),0)::float8 AS won_value,
                    COUNT(*) FILTER (WHERE x.is_lost) AS lost_count,
                    COALESCE(SUM(x.conv) FILTER (WHERE x.is_lost),0)::float8 AS lost_value,
                    COUNT(*) FILTER (WHERE x.conv IS NULL) AS unconverted_count
                   FROM (
                     SELECT o.is_won, o.is_lost, o.probability,
                            CASE
                              WHEN upper(COALESCE(o.currency,'')) = upper($3) THEN o.value
                              WHEN o.exchange_rate IS NOT NULL
                                   AND upper(COALESCE(o.reporting_currency, o.currency)) = upper($3)
                                  THEN o.value * o.exchange_rate
                            END AS conv
                       FROM opportunities o
                      WHERE o.tenant_id = $1 AND o.pipeline_id = $2
                   ) x",
            )
            .bind(tenant)
            .bind(pid)
            .bind(&reporting_currency)
            .fetch_one(&state.db)
            .await?;
            let closed = row.won_count + row.lost_count;
            ReportingTotals {
                currency: reporting_currency.clone(),
                open_count: row.open_count,
                open_value: row.open_value,
                open_weighted_value: row.open_weighted,
                won_count: row.won_count,
                won_value: row.won_value,
                lost_count: row.lost_count,
                lost_value: row.lost_value,
                win_rate: if closed > 0 {
                    (row.won_count as f64 / closed as f64 * 1000.0).round() / 1000.0
                } else {
                    0.0
                },
                unconverted_count: row.unconverted_count,
            }
        };

        // Which currencies does this pipeline actually hold? A pipeline with no deals still gets
        // one row in the workspace currency (or a blank one) so the screen is not empty-shaped.
        let currencies: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT COALESCE(currency,'') FROM opportunities
              WHERE tenant_id = $1 AND pipeline_id = $2 ORDER BY 1",
        )
        .bind(tenant)
        .bind(pid)
        .fetch_all(&state.db)
        .await?;
        let currencies = if currencies.is_empty() {
            vec![super::writer::workspace_default_currency(&state.db, tenant)
                .await?
                .unwrap_or_default()]
        } else {
            currencies
        };

        for currency in currencies {
            let head: HeadTotals = sqlx::query_as(
                "SELECT
                    COUNT(*) FILTER (WHERE NOT is_won AND NOT is_lost) AS open_count,
                    COALESCE(SUM(value) FILTER (WHERE NOT is_won AND NOT is_lost),0)::float8 AS open_value,
                    COALESCE(SUM(value * COALESCE(probability,0) / 100.0)
                             FILTER (WHERE NOT is_won AND NOT is_lost),0)::float8 AS open_weighted,
                    COUNT(*) FILTER (WHERE is_won) AS won_count,
                    COALESCE(SUM(value) FILTER (WHERE is_won),0)::float8 AS won_value,
                    COUNT(*) FILTER (WHERE is_lost) AS lost_count,
                    COALESCE(SUM(value) FILTER (WHERE is_lost),0)::float8 AS lost_value,
                    COALESCE(AVG(EXTRACT(EPOCH FROM (stage_entered_at - created_at)) / 86400.0)
                             FILTER (WHERE is_won),0)::float8 AS avg_days_to_close,
                    COUNT(*) FILTER (WHERE NOT is_won AND NOT is_lost
                                       AND last_activity_at < NOW() - ($4::int * INTERVAL '1 day'))
                        AS rotting_count
                  FROM opportunities
                 WHERE tenant_id = $1 AND pipeline_id = $2 AND COALESCE(currency,'') = $3",
            )
            .bind(tenant)
            .bind(pid)
            .bind(&currency)
            .bind(rot_days as i32)
            .fetch_one(&state.db)
            .await?;

            let stage_rows: Vec<StageRow> = sqlx::query_as(
                "SELECT s.id AS stage_id, s.name AS stage_name, s.position,
                        COALESCE(s.probability,0) AS probability, s.is_won, s.is_lost,
                        COUNT(o.id) AS deal_count,
                        COALESCE(SUM(o.value),0)::float8 AS total_value,
                        COALESCE(SUM(o.value * COALESCE(o.probability,0) / 100.0),0)::float8 AS weighted_value
                   FROM pipeline_stages s
                   LEFT JOIN opportunities o
                          ON o.stage_id = s.id AND o.tenant_id = $1
                         AND COALESCE(o.currency,'') = $3
                  WHERE s.pipeline_id = $2
                  GROUP BY s.id, s.name, s.position, s.probability, s.is_won, s.is_lost
                  ORDER BY s.position, s.name",
            )
            .bind(tenant)
            .bind(pid)
            .bind(&currency)
            .fetch_all(&state.db)
            .await?;

            let closed = head.won_count + head.lost_count;
            let win_rate = if closed > 0 {
                head.won_count as f64 / closed as f64
            } else {
                0.0
            };

            out.push(DealReport {
                pipeline_id: pid,
                pipeline_name: pname.clone(),
                currency: currency.clone(),
                open_count: head.open_count,
                open_value: head.open_value,
                open_weighted_value: head.open_weighted,
                won_count: head.won_count,
                won_value: head.won_value,
                lost_count: head.lost_count,
                lost_value: head.lost_value,
                win_rate: (win_rate * 1000.0).round() / 1000.0,
                avg_days_to_close: (head.avg_days_to_close * 10.0).round() / 10.0,
                rotting_count: head.rotting_count,
                rot_days,
                stages: stage_rows
                    .into_iter()
                    .map(|s| StageTotal {
                        stage_id: s.stage_id,
                        stage_name: s.stage_name,
                        position: s.position,
                        probability: s.probability,
                        is_won: s.is_won,
                        is_lost: s.is_lost,
                        currency: currency.clone(),
                        deal_count: s.deal_count,
                        total_value: s.total_value,
                        weighted_value: s.weighted_value,
                    })
                    .collect(),
                reporting: rep.clone(),
            });
        }
    }
    Ok(out)
}

/// Count of deals rotting (no activity for `rot_days`) — used by nothing yet but kept private to
/// the module so the calculation has ONE home. Kept because a caller in the console (the
/// dashboard's "needs attention" tile) reaches it through [`build_report`].
#[allow(dead_code)]
pub async fn rotting_count(db: &PgPool, tenant: Uuid, rot_days: i64) -> Result<i64, AppError> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS n FROM opportunities
          WHERE tenant_id = $1 AND NOT is_won AND NOT is_lost
            AND last_activity_at < NOW() - ($2::int * INTERVAL '1 day')",
    )
    .bind(tenant)
    .bind(rot_days as i32)
    .fetch_one(db)
    .await?;
    Ok(row.get::<i64, _>("n"))
}
