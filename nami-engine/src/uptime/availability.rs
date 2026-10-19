//! Availability (SLA) report: the share of a range a monitor or heartbeat was
//! not down, with maintenance taken out of both sides.
//!
//! Downtime is the union of the incident intervals inside the range, minus
//! the union of the maintenance spans, so a second that two incidents (or an
//! incident and a window) cover counts once. The pure part ([`compute`]) takes
//! spans; [`for_monitor`] and [`for_heartbeat`] gather them. Incident
//! Management's SLA report reuses those two.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
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
    subtract_sorted(&union(spans.to_vec()), &union(minus.to_vec()))
}

/// [`subtract`] for inputs that are already sorted and disjoint (the output
/// of [`union`]): a binary search finds the first hole that can matter for
/// each span, so the cost is O((n + k) log m) instead of rescanning the holes.
fn subtract_sorted(spans: &[Span], minus: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    for span in spans {
        let mut start = span.start;
        let first = minus.partition_point(|hole| hole.end <= span.start);
        for hole in &minus[first..] {
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

/// `?from=&to=` of the availability reports (RFC 3339).
#[derive(Debug, Deserialize)]
pub struct RangeQuery {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
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
        // A range wholly before creation: both bounds are the clipped start.
        return Availability {
            from: total.start,
            to: total.start,
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
    let down = subtract_sorted(&union(clipped.clone()), &maintenance);
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
            .map(|s| seconds(&subtract_sorted(&[*s], &maintenance)))
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

    /// Tiny deterministic generator (xorshift), no dependencies.
    fn rng(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    fn random_spans(seed: &mut u64, n: usize) -> Vec<Span> {
        (0..n)
            .map(|_| {
                let a = (rng(seed) % 200) as i64 - 20;
                let len = (rng(seed) % 15) as i64 - 2; // some empty or inverted
                s(a, a + len)
            })
            .collect()
    }

    #[test]
    fn subtract_sorted_equals_subtract_on_random_inputs() {
        let mut seed = 0x9E37_79B9_7F4A_7C15;
        for _ in 0..500 {
            let (n, m) = (
                (rng(&mut seed) % 12) as usize,
                (rng(&mut seed) % 12) as usize,
            );
            let (spans, minus) = (random_spans(&mut seed, n), random_spans(&mut seed, m));
            let expected = subtract(&spans, &minus);
            assert_eq!(
                subtract_sorted(&union(spans.clone()), &union(minus.clone())),
                expected,
                "{spans:?} - {minus:?}"
            );
            // Seconds check against a brute-force hour grid.
            let covered =
                |set: &[Span], h: i64| set.iter().any(|x| x.start <= t(h) && t(h) < x.end);
            for h in -30..230 {
                assert_eq!(
                    covered(&expected, h),
                    covered(&spans, h) && !covered(&minus, h),
                    "hour {h}"
                );
            }
        }
    }

    #[test]
    fn a_three_year_report_with_thousands_of_incidents_is_fast() {
        // 200 daily windows (as many as a project may hold) over three years
        // and 5000 incidents; held to a generous bound in a debug build.
        let sec = |n: i64| t(0) + TimeDelta::seconds(n);
        let range = 1_096 * 86_400;
        let maintenance: Vec<Span> = (0..219_000)
            .map(|i| Span {
                start: sec(i * 432),
                end: sec(i * 432 + 200),
            })
            .collect();
        let mut seed = 42u64;
        let incidents: Vec<Span> = (0..5000)
            .map(|_| {
                let a = (rng(&mut seed) % range as u64) as i64;
                Span {
                    start: sec(a),
                    end: sec(a + 1 + (rng(&mut seed) % 3000) as i64),
                }
            })
            .collect();
        let total = Span {
            start: sec(0),
            end: sec(range),
        };
        let started = std::time::Instant::now();
        let a = compute(total, &incidents, &maintenance);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(a.incidents, 5000);
        assert!(a.downtime_seconds > 0 && a.uptime_pct.is_some());
    }
}
