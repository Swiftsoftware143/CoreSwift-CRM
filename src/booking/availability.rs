//! Pure time math for appointment calendars: no database, fully unit-tested.

use super::models::{AvailabilityRule, Resource};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

/// Convert a local wall-clock time in `tz` to UTC (None if it falls in a DST gap).
pub fn local_to_utc(tz: Tz, local: NaiveDateTime) -> Option<DateTime<Utc>> {
    tz.from_local_datetime(&local)
        .earliest()
        .map(|d| d.with_timezone(&Utc))
}

/// UTC instant of local midnight at the start of `day`.
pub fn day_start(tz: Tz, day: NaiveDate) -> Option<DateTime<Utc>> {
    local_to_utc(tz, day.and_hms_opt(0, 0, 0)?)
}

/// Every candidate (start, end) for `resource` on `day`, from its weekly rules.
pub fn appointment_windows(
    resource: &Resource,
    rules: &[AvailabilityRule],
    tz: Tz,
    day: NaiveDate,
) -> Vec<(DateTime<Utc>, DateTime<Utc>)> {
    let weekday = day.weekday().num_days_from_monday() as i16;
    let duration = Duration::minutes(resource.duration_min.max(1) as i64);
    let step = Duration::minutes(resource.slot_interval_min.max(1) as i64);
    let mut out = Vec::new();
    for rule in rules.iter().filter(|r| r.weekday == weekday) {
        let mut t = day.and_time(rule.start_time);
        let close = day.and_time(rule.end_time);
        while t + duration <= close {
            if let (Some(s), Some(e)) = (local_to_utc(tz, t), local_to_utc(tz, t + duration)) {
                out.push((s, e));
            }
            t += step;
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Seats already taken in [start, end), padding existing bookings by `buffer_min` on both sides.
pub fn seats_used(
    busy: &[(DateTime<Utc>, DateTime<Utc>, i32)],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    buffer_min: i32,
) -> i64 {
    let pad = Duration::minutes(buffer_min.max(0) as i64);
    busy.iter()
        .filter(|(bs, be, _)| *bs - pad < end && *be + pad > start)
        .map(|(_, _, q)| *q as i64)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;
    use serde_json::json;
    use uuid::Uuid;

    fn resource(duration: i32, step: i32) -> Resource {
        Resource {
            id: Uuid::new_v4(),
            calendar_id: Uuid::new_v4(),
            name: "Consult".into(),
            description: String::new(),
            capacity: 1,
            price_cents: 0,
            duration_min: duration,
            slot_interval_min: step,
            buffer_min: 0,
            min_notice_min: 0,
            sort_order: 0,
            metadata: json!({}),
            active: true,
            created_at: Utc::now(),
        }
    }

    fn rule(weekday: i16, start: (u32, u32), end: (u32, u32)) -> AvailabilityRule {
        AvailabilityRule {
            weekday,
            start_time: NaiveTime::from_hms_opt(start.0, start.1, 0).unwrap(),
            end_time: NaiveTime::from_hms_opt(end.0, end.1, 0).unwrap(),
        }
    }

    #[test]
    fn generates_windows_in_local_time() {
        let tz: Tz = "America/New_York".parse().unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let w = appointment_windows(&resource(30, 30), &[rule(0, (9, 0), (11, 0))], tz, monday);
        assert_eq!(w.len(), 4);
        // 9:00 EST == 14:00 UTC
        assert_eq!(w[0].0.to_rfc3339(), "2026-11-16T14:00:00+00:00");
        assert_eq!(w[3].1.to_rfc3339(), "2026-11-16T16:00:00+00:00");
    }

    #[test]
    fn last_window_must_fit_before_close_and_other_days_are_empty() {
        let tz: Tz = "UTC".parse().unwrap();
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        let rules = [rule(0, (9, 0), (10, 15))];
        assert_eq!(
            appointment_windows(&resource(60, 30), &rules, tz, monday).len(),
            1
        );
        assert!(
            appointment_windows(&resource(60, 30), &rules, tz, monday.succ_opt().unwrap())
                .is_empty()
        );
    }

    #[test]
    fn buffer_blocks_adjacent_times() {
        let t = |h, m| Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap();
        let busy = [(t(10, 0), t(10, 30), 1)];
        assert_eq!(seats_used(&busy, t(10, 30), t(11, 0), 0), 0);
        assert_eq!(seats_used(&busy, t(10, 30), t(11, 0), 15), 1);
        assert_eq!(seats_used(&busy, t(10, 15), t(10, 45), 0), 1);
    }
}
