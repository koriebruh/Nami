//! Availability (SLA) report: the share of a range a monitor or heartbeat was
//! not down, with maintenance taken out of both sides.
//!
//! Downtime is the union of the incident intervals inside the range, minus
//! the union of the maintenance spans, so a second that two incidents (or an
//! incident and a window) cover counts once. The pure part ([`compute`]) takes
//! spans; [`for_monitor`] and [`for_heartbeat`] gather them. Incident
//! Management's SLA report reuses those two.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::{
    heartbeat,
    maintenance::{self, MaintenanceWindow, Span},
    monitor,
};

/// The longest range a report covers (three years).
pub const MAX_RANGE_DAYS: i64 = 1096;

/// Merges overlapping and touching spans; drops empty and inverted ones.
/// The result is sorted and disjoint.
pub fn union(mut spans: Vec<Span>) -> Vec<Span> {
    spans.retain(|s| s.start < s.end);
    spans.sort_by_key(|s| s.start);
    let mut merged: Vec<Span> = Vec::with_capacity(spans.len());
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => last.end = last.end.max(span.end),
            _ => merged.push(span),
        }
    }
    merged
}

/// The parts of `spans` that `minus` does not cover. Both are merged first,
/// so any input order and overlap is fine.
pub fn subtract(spans: &[Span], minus: &[Span]) -> Vec<Span> {
    let minus = union(minus.to_vec());
    let mut out = Vec::new();
    for span in union(spans.to_vec()) {
        let mut start = span.start;
        for hole in &minus {
            if hole.end <= start {
                continue;
            }
            if hole.start >= span.end {
                break;
            }
            if hole.start > start {
                out.push(Span {
                    start,
                    end: hole.start,
                });
            }
            start = start.max(hole.end);
        }
        if start < span.end {
            out.push(Span {
                start,
                end: span.end,
            });
        }
    }
    out
}

/// Total whole seconds in `spans` (assumed disjoint).
pub fn seconds(spans: &[Span]) -> i64 {
    spans
        .iter()
        .map(|s| (s.end - s.start).num_seconds().max(0))
        .sum()
}

fn clip(span: Span, to: Span) -> Span {
    Span {
        start: span.start.max(to.start),
        end: span.end.min(to.end),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Availability {
    /// The effective range: the requested one clipped to the monitor's
    /// creation and to now.
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub total_seconds: i64,
    pub maintenance_seconds: i64,
    pub downtime_seconds: i64,
    /// 0-100; `None` when nothing but maintenance (or nothing) is left.
    pub uptime_pct: Option<f64>,
    /// Incidents overlapping the effective range.
    pub incidents: u32,
    /// The longest single incident's downtime, maintenance removed.
    pub longest_incident_seconds: i64,
}

/// Pure report. `total` is the range already clipped; an empty or inverted
/// one gives zeros. Incidents and maintenance are clipped to it here.
pub fn compute(total: Span, incidents: &[Span], maintenance: &[Span]) -> Availability {
    if total.start >= total.end {
        return Availability {
            from: total.start,
            to: total.end,
            total_seconds: 0,
            maintenance_seconds: 0,
            downtime_seconds: 0,
            uptime_pct: None,
            incidents: 0,
            longest_incident_seconds: 0,
        };
    }
    let maintenance = union(maintenance.iter().map(|s| clip(*s, total)).collect());
    let clipped: Vec<Span> = incidents
        .iter()
        .map(|s| clip(*s, total))
        .filter(|s| s.start < s.end)
        .collect();
    let down = subtract(&union(clipped.clone()), &maintenance);
    let total_seconds = seconds(&[total]);
    let maintenance_seconds = seconds(&maintenance);
    let downtime_seconds = seconds(&down);
    let usable = total_seconds - maintenance_seconds;
    Availability {
        from: total.start,
        to: total.end,
        total_seconds,
        maintenance_seconds,
        downtime_seconds,
        uptime_pct: (usable > 0)
            .then(|| (100.0 * (1.0 - downtime_seconds as f64 / usable as f64)).clamp(0.0, 100.0)),
        incidents: clipped.len() as u32,
        longest_incident_seconds: clipped
            .iter()
            .map(|s| seconds(&subtract(&[*s], &maintenance)))
            .max()
            .unwrap_or(0),
    }
}

/// `Err` carries a 400's message.
pub fn check_range(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<(), String> {
    if from >= to {
        return Err("from must be before to".into());
    }
    if to - from > chrono::TimeDelta::days(MAX_RANGE_DAYS) {
        return Err(format!("the range must be at most {MAX_RANGE_DAYS} days"));
    }
    Ok(())
}

fn window_spans(
    windows: &[MaintenanceWindow],
    applies: impl Fn(&MaintenanceWindow) -> bool,
    total: Span,
) -> Vec<Span> {
    windows
        .iter()
        .filter(|w| applies(w))
        .flat_map(|w| w.spans(total.start, total.end))
        .collect()
}

/// `Ok(None)`: the monitor is not in the project.
pub async fn for_monitor(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<Availability>, sqlx::Error> {
    let Some(m) = monitor::find_in_project(pool, project_id, monitor_id).await? else {
        return Ok(None);
    };
    let total = Span {
        start: from.max(m.created_at),
        end: to.min(now),
    };
    let incidents = incident_spans(pool, "monitor_id", monitor_id, total, now).await?;
    let windows = maintenance::list_for_project(pool, project_id).await?;
    let maintenance = window_spans(&windows, |w| w.applies_to_monitor(monitor_id), total);
    Ok(Some(compute(total, &incidents, &maintenance)))
}

/// `Ok(None)`: the heartbeat is not in the project. Only project-wide
/// windows count.
pub async fn for_heartbeat(
    pool: &PgPool,
    project_id: Uuid,
    heartbeat_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Option<Availability>, sqlx::Error> {
    let Some(h) = heartbeat::find_in_project(pool, project_id, heartbeat_id).await? else {
        return Ok(None);
    };
    let total = Span {
        start: from.max(h.created_at),
        end: to.min(now),
    };
    let incidents = incident_spans(pool, "heartbeat_id", heartbeat_id, total, now).await?;
    let windows = maintenance::list_for_project(pool, project_id).await?;
    let maintenance = window_spans(&windows, MaintenanceWindow::applies_to_heartbeats, total);
    Ok(Some(compute(total, &incidents, &maintenance)))
}

/// Incident intervals overlapping `total`; an open incident runs to `now`.
/// `column` is one of two literals (never user input).
async fn incident_spans(
    pool: &PgPool,
    column: &'static str,
    id: Uuid,
    total: Span,
    now: DateTime<Utc>,
) -> Result<Vec<Span>, sqlx::Error> {
    let sql = match column {
        "monitor_id" => concat!(
            "select started_at, resolved_at from incidents where monitor_id = $1 ",
            "and started_at < $3 and (resolved_at is null or resolved_at > $2)"
        ),
        _ => concat!(
            "select started_at, resolved_at from incidents where heartbeat_id = $1 ",
            "and started_at < $3 and (resolved_at is null or resolved_at > $2)"
        ),
    };
    let rows: Vec<(DateTime<Utc>, Option<DateTime<Utc>>)> = sqlx::query_as(sql)
        .bind(id)
        .bind(total.start)
        .bind(total.end)
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(start, resolved)| Span {
            start,
            end: resolved.unwrap_or(now),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    fn t(h: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + TimeDelta::hours(h)
    }

    fn s(a: i64, b: i64) -> Span {
        Span {
            start: t(a),
            end: t(b),
        }
    }

    #[test]
    fn union_merges_overlapping_and_touching_spans() {
        assert_eq!(
            union(vec![s(5, 7), s(0, 2), s(1, 3), s(3, 4)]),
            vec![s(0, 4), s(5, 7)]
        );
        assert_eq!(union(vec![]), vec![]);
        // Nested, and empty or inverted spans vanish.
        assert_eq!(
            union(vec![s(0, 10), s(2, 3), s(4, 4), s(6, 5)]),
            vec![s(0, 10)]
        );
    }

    #[test]
    fn subtract_cuts_holes() {
        assert_eq!(
            subtract(&[s(0, 10)], &[s(2, 3), s(5, 12)]),
            vec![s(0, 2), s(3, 5)]
        );
        assert_eq!(subtract(&[s(0, 1)], &[s(0, 1)]), vec![]);
        // Unsorted, overlapping and touching holes; hole before the span.
        assert_eq!(
            subtract(&[s(2, 10)], &[s(6, 8), s(-5, 1), s(3, 4), s(4, 5), s(3, 3)]),
            vec![s(2, 3), s(5, 6), s(8, 10)]
        );
        // Holes touching the span edges leave it whole.
        assert_eq!(subtract(&[s(2, 4)], &[s(0, 2), s(4, 6)]), vec![s(2, 4)]);
        assert_eq!(subtract(&[s(0, 4)], &[]), vec![s(0, 4)]);
    }

    #[test]
    fn overlapping_incidents_and_maintenance_count_each_second_once() {
        // Range 0..100h; incidents 10-30, 20-40 (overlap) and one still open
        // from -50 to "now" 100; maintenance 25-35 and 90-200.
        let a = compute(
            s(0, 100),
            &[s(10, 30), s(20, 40), s(-50, 100)],
            &[s(25, 35), s(90, 200)],
        );
        assert_eq!(a.total_seconds, 100 * 3600);
        assert_eq!(a.maintenance_seconds, 20 * 3600); // 25-35, 90-100
        assert_eq!(a.downtime_seconds, 80 * 3600); // 0-100 minus maintenance
        assert_eq!(a.uptime_pct, Some(0.0));
        assert_eq!(a.incidents, 3);
        assert_eq!(a.longest_incident_seconds, 80 * 3600);
        assert_eq!((a.from, a.to), (t(0), t(100)));
        let none = compute(s(0, 10), &[], &[]);
        assert_eq!((none.downtime_seconds, none.uptime_pct), (0, Some(100.0)));
        let all_maintenance = compute(s(0, 10), &[s(0, 10)], &[s(0, 10)]);
        assert_eq!(
            (all_maintenance.downtime_seconds, all_maintenance.uptime_pct),
            (0, None)
        );
        let empty = compute(s(5, 5), &[s(0, 10)], &[]);
        assert_eq!(
            (empty.total_seconds, empty.uptime_pct, empty.incidents),
            (0, None, 0)
        );
        let inverted = compute(s(6, 5), &[s(0, 10)], &[]);
        assert_eq!((inverted.total_seconds, inverted.uptime_pct), (0, None));
        let half = compute(s(0, 10), &[s(-5, 5)], &[]);
        assert_eq!(
            (half.downtime_seconds, half.uptime_pct, half.incidents),
            (5 * 3600, Some(50.0), 1)
        );
    }

    #[test]
    fn touching_and_outside_incidents_do_not_count() {
        // Ends exactly at the range start, starts exactly at its end.
        let a = compute(s(10, 20), &[s(0, 10), s(20, 30)], &[]);
        assert_eq!((a.incidents, a.downtime_seconds), (0, 0));
        // An incident wholly inside maintenance is an incident with no downtime.
        let b = compute(s(0, 10), &[s(2, 4)], &[s(0, 5)]);
        assert_eq!(
            (b.incidents, b.downtime_seconds, b.longest_incident_seconds),
            (1, 0, 0)
        );
        assert_eq!(b.uptime_pct, Some(100.0));
        // Two touching incidents are two incidents but one stretch of downtime.
        let c = compute(s(0, 10), &[s(0, 3), s(3, 6)], &[]);
        assert_eq!((c.incidents, c.downtime_seconds), (2, 6 * 3600));
        assert_eq!(c.longest_incident_seconds, 3 * 3600);
    }

    #[test]
    fn ranges_are_bounded() {
        assert_eq!(
            check_range(t(1), t(1)),
            Err("from must be before to".into())
        );
        assert_eq!(
            check_range(t(2), t(1)),
            Err("from must be before to".into())
        );
        assert!(check_range(t(0), t(24 * MAX_RANGE_DAYS)).is_ok());
        assert_eq!(
            check_range(t(0), t(24 * MAX_RANGE_DAYS + 1)),
            Err("the range must be at most 1096 days".into())
        );
    }
}
