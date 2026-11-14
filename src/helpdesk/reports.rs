//! The four questions worth asking about a support desk.
//!
//! How fast did we answer, how fast did we finish, how much is open and how much of it is late. Everything
//! here is computed from the ticket rows themselves rather than from a rollup, because at this size a rollup
//! is a second copy of the truth that can be wrong; when the table is big enough that the query hurts, the
//! rollup arrives WITH the query that needs it.
//!
//! Two honest limits, stated rather than hidden:
//!
//!   * Times are wall-clock. Business hours exist in the policy and are not applied here, so an answer sent
//!     at 9am Monday to a Friday-evening question reads as sixty hours. The alternative — quietly pretending
//!     to know the calendar — would be worse.
//!   * The AI figures are present and will read zero until an AI actually answers. A report that omits a
//!     number because the feature is unbuilt is harder to trust than one that shows the zero.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::errors::AppError;

#[derive(Debug, Serialize, Default)]
pub struct DeskReport {
    pub days: i64,
    pub created: i64,
    pub solved: i64,
    pub open_now: i64,
    pub unanswered_now: i64,
    pub breached_now: i64,
    /// Median and 90th percentile in minutes, from creation to a person's first reply.
    pub first_response_median_mins: Option<f64>,
    pub first_response_p90_mins: Option<f64>,
    /// Median and 90th percentile in minutes, from creation to solved.
    pub resolution_median_mins: Option<f64>,
    pub resolution_p90_mins: Option<f64>,
    pub by_channel: Vec<(String, i64)>,
    pub by_status: Vec<(String, i64)>,
}

/// Everything the strip shows, in one round trip.
pub async fn desk_report(
    pool: &PgPool,
    tenant_id: Uuid,
    days: i64,
) -> Result<DeskReport, AppError> {
    let days = days.clamp(1, 365);
    let window = format!("{days} days");

    let (created, solved): (i64, i64) = sqlx::query_as(
        r#"SELECT
             count(*) FILTER (WHERE created_at >= NOW() - $2::interval),
             count(*) FILTER (WHERE solved_at IS NOT NULL AND solved_at >= NOW() - $2::interval)
           FROM tickets WHERE tenant_id = $1"#,
    )
    .bind(tenant_id)
    .bind(&window)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    let (open_now, unanswered_now, breached_now): (i64, i64, i64) = sqlx::query_as(
        r#"SELECT
             count(*) FILTER (WHERE status IN ('new', 'open', 'pending', 'on_hold')),
             count(*) FILTER (WHERE status IN ('new', 'open', 'pending', 'on_hold')
                                AND first_responded_at IS NULL),
             count(*) FILTER (WHERE status IN ('new', 'open', 'pending', 'on_hold')
                                AND first_responded_at IS NULL
                                AND first_response_due IS NOT NULL
                                AND first_response_due < NOW())
           FROM tickets WHERE tenant_id = $1"#,
    )
    .bind(tenant_id)
    .fetch_one(pool)
    .await
    .map_err(AppError::Database)?;

    let (fr_med, fr_p90, res_med, res_p90): (Option<f64>, Option<f64>, Option<f64>, Option<f64>) =
        sqlx::query_as(
            r#"SELECT
             percentile_cont(0.5) WITHIN GROUP (
               ORDER BY EXTRACT(EPOCH FROM (first_responded_at - created_at)) / 60.0),
             percentile_cont(0.9) WITHIN GROUP (
               ORDER BY EXTRACT(EPOCH FROM (first_responded_at - created_at)) / 60.0),
             percentile_cont(0.5) WITHIN GROUP (
               ORDER BY EXTRACT(EPOCH FROM (solved_at - created_at)) / 60.0),
             percentile_cont(0.9) WITHIN GROUP (
               ORDER BY EXTRACT(EPOCH FROM (solved_at - created_at)) / 60.0)
           FROM tickets
          WHERE tenant_id = $1
            AND created_at >= NOW() - $2::interval"#,
        )
        .bind(tenant_id)
        .bind(&window)
        .fetch_one(pool)
        .await
        .map_err(AppError::Database)?;

    let by_channel: Vec<(String, i64)> = sqlx::query_as(
        r#"SELECT source, count(*) FROM tickets
            WHERE tenant_id = $1 AND created_at >= NOW() - $2::interval
            GROUP BY source ORDER BY count(*) DESC"#,
    )
    .bind(tenant_id)
    .bind(&window)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    let by_status: Vec<(String, i64)> = sqlx::query_as(
        r#"SELECT status, count(*) FROM tickets WHERE tenant_id = $1 GROUP BY status ORDER BY 2 DESC"#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(AppError::Database)?;

    Ok(DeskReport {
        days,
        created,
        solved,
        open_now,
        unanswered_now,
        breached_now,
        // The median of an empty set is NULL, not zero: saying "0 minutes" for a desk that has never been
        // asked anything would read as perfect performance.
        first_response_median_mins: fr_med,
        first_response_p90_mins: fr_p90,
        resolution_median_mins: res_med,
        resolution_p90_mins: res_p90,
        by_channel,
        by_status,
    })
}

// ── HTTP ──────────────────────────────────────────────────────────────────────────────────────

use crate::auth::models::Claims;
use crate::errors::ApiResult;
use crate::AppState;
use axum::{extract::Query, extract::State, Extension, Json};

#[derive(Debug, serde::Deserialize)]
pub struct ReportQuery {
    pub days: Option<i64>,
}

pub async fn handler(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<DeskReport>> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(desk_report(&s.db, tid, q.days.unwrap_or(30)).await?))
}

pub fn router(_state: AppState) -> axum::Router<AppState> {
    axum::Router::new().route("/reports", axum::routing::get(handler))
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_empty_desk_reports_nothing_rather_than_zero() {
        // a NULL median must stay NULL through serialisation, otherwise the strip would claim "0 minutes"
        let report = super::DeskReport::default();
        assert!(report.first_response_median_mins.is_none());
        assert_eq!(report.created, 0);
    }
}
