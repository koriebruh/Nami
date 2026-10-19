//! Maintenance windows: while one covers a monitor or heartbeat, its checks
//! still run and are recorded, and its pings still arrive, but its state
//! machine stands still: no incident opens or resolves, no alert is sent, and
//! no counter moves toward a transition (see `uptime::worker::apply_outcome`
//! and `uptime::heartbeat`). An incident already open when a window starts
//! stays open, and can resolve once the window is over.
//!
//! A window belongs to a project and either covers one of its monitors
//! (`monitor_id`) or, with no monitor, every monitor and every heartbeat of
//! the project. It is either one-off (`starts_at` to `ends_at`) or weekly
//! (wall-clock times on chosen ISO weekdays in an IANA time zone, see
//! [`Schedule::covers`]).

use std::str::FromStr;

use chrono::{
    DateTime, Datelike, LocalResult, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Timelike, Utc,
    Weekday,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

use crate::core::error::AppError;
use crate::core::validate::{nullable, validate_label, with_unchanged_values};
use crate::uptime::quota::ProjectQuota;

/// The longest a one-off window may last.
pub const MAX_ONE_OFF_DAYS: i64 = 366;

/// One-off windows must lie between these years: RFC 3339 (the API's date
/// format) has four-digit years, and nothing before 1970 can be a window
/// anyone still cares about.
pub const MIN_YEAR: i32 = 1970;
pub const MAX_YEAR: i32 = 9999;

/// The longest time zone name accepted (the longest IANA name is 32 bytes).
const MAX_TIMEZONE_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
pub struct MaintenanceWindow {
    pub id: Uuid,
    pub project_id: Uuid,
    /// `None`: every monitor and heartbeat of the project.
    pub monitor_id: Option<Uuid>,
    pub title: String,
    /// `once` or `weekly`.
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    /// ISO weekdays, 1 = Monday ... 7 = Sunday.
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub timezone: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// When a window is in effect, independent of how it is stored.
#[derive(Debug, Clone, PartialEq)]
pub enum Schedule {
    Once {
        starts_at: DateTime<Utc>,
        ends_at: DateTime<Utc>,
    },
    Weekly {
        days: Vec<Weekday>,
        start: NaiveTime,
        end: NaiveTime,
        timezone: Tz,
    },
}

/// A half-open interval of time, `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

/// The first instant whose wall-clock time in `tz` is at or after `local`:
/// the earlier of two on a repeated hour (`prefer_latest = false`) or the
/// later one (`true`), the end of the gap for a skipped one.
fn instant_at_or_after(tz: Tz, local: NaiveDateTime, prefer_latest: bool) -> DateTime<Utc> {
    let mut candidate = local;
    // A DST gap is at most a few hours; stop after 180 minutes regardless.
    for _ in 0..=180 {
        match tz.from_local_datetime(&candidate) {
            LocalResult::Single(t) => return t.with_timezone(&Utc),
            LocalResult::Ambiguous(first, second) => {
                return if prefer_latest { second } else { first }.with_timezone(&Utc);
            }
            LocalResult::None => candidate += TimeDelta::minutes(1),
        }
    }
    local.and_utc()
}

impl Schedule {
    /// The spans inside `[from, to)` during which the window is in effect,
    /// sorted by start, clipped to the range, possibly overlapping (weekly
    /// windows whose days wrap past midnight do not overlap each other, but
    /// callers merge anyway). Follows the wall-clock semantics of
    /// [`Schedule::covers`]; the one difference is a window that starts or
    /// ends inside a repeated DST hour, where `covers` has a hole and this
    /// reports one span from the earliest start to the latest end.
    pub fn spans(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span> {
        let clip = |start: DateTime<Utc>, end: DateTime<Utc>| {
            let (start, end) = (start.max(from), end.min(to));
            (start < end).then_some(Span { start, end })
        };
        match self {
            Schedule::Once { starts_at, ends_at } => {
                clip(*starts_at, *ends_at).into_iter().collect()
            }
            Schedule::Weekly {
                days,
                start,
                end,
                timezone,
            } => {
                if from >= to {
                    return Vec::new();
                }
                let first = from.with_timezone(timezone).date_naive() - TimeDelta::days(1);
                let last = to.with_timezone(timezone).date_naive();
                let wraps = end <= start;
                let mut spans = Vec::new();
                let mut day = first;
                while day <= last {
                    if days.contains(&day.weekday()) {
                        let end_day = if wraps { day + TimeDelta::days(1) } else { day };
                        let s = instant_at_or_after(*timezone, day.and_time(*start), false);
                        let e = instant_at_or_after(*timezone, end_day.and_time(*end), true);
                        spans.extend(clip(s, e));
                    }
                    day += TimeDelta::days(1);
                }
                spans.sort_by_key(|span| span.start);
                spans
            }
        }
    }

    /// Whether the window is in effect at `at`.
    ///
    /// - One-off: `starts_at <= at < ends_at`.
    /// - Weekly: compares wall-clock time in `timezone`. On each listed day
    ///   the window runs from `start` (inclusive) to `end` (exclusive); an
    ///   `end` at or before `start` runs past midnight into the next day
    ///   (`start == end` is a full 24 hours), so a Sunday 23:00-01:00 window
    ///   carries over into Monday. Because it compares what a clock on the
    ///   wall shows, a window inside an hour skipped by a DST change does not
    ///   happen that day (one across it is an hour shorter), and one across a
    ///   repeated hour lasts an hour longer.
    pub fn covers(&self, at: DateTime<Utc>) -> bool {
        match self {
            Schedule::Once { starts_at, ends_at } => *starts_at <= at && at < *ends_at,
            Schedule::Weekly {
                days,
                start,
                end,
                timezone,
            } => {
                let local = at.with_timezone(timezone);
                let time = local.time();
                let today = local.weekday();
                let wraps = end <= start;
                let started_today =
                    days.contains(&today) && time >= *start && (wraps || time < *end);
                let carried_over = wraps && days.contains(&today.pred()) && time < *end;
                started_today || carried_over
            }
        }
    }
}

/// ISO weekday number (1 = Monday ... 7 = Sunday).
pub fn weekday_from_iso(day: i16) -> Option<Weekday> {
    u8::try_from(day)
        .ok()
        .filter(|d| (1..=7).contains(d))
        .and_then(|d| Weekday::try_from(d - 1).ok())
}

impl MaintenanceWindow {
    /// `None` for a row whose fields do not form a schedule. The table's
    /// check constraint and [`validate`] rule that out; should it happen
    /// anyway (a time zone a newer tz database dropped), such a window covers
    /// nothing.
    pub fn schedule(&self) -> Option<Schedule> {
        match self.kind.as_str() {
            "once" => Some(Schedule::Once {
                starts_at: self.starts_at?,
                ends_at: self.ends_at?,
            }),
            "weekly" => Some(Schedule::Weekly {
                days: self
                    .days_of_week
                    .as_ref()?
                    .iter()
                    .map(|d| weekday_from_iso(*d))
                    .collect::<Option<_>>()?,
                start: self.start_time?,
                end: self.end_time?,
                timezone: Tz::from_str(self.timezone.as_deref()?).ok()?,
            }),
            _ => None,
        }
    }

    pub fn covers(&self, at: DateTime<Utc>) -> bool {
        self.schedule().is_some_and(|schedule| schedule.covers(at))
    }

    /// [`Schedule::spans`]; empty for a row that is not a schedule.
    pub fn spans(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Span> {
        self.schedule()
            .map(|schedule| schedule.spans(from, to))
            .unwrap_or_default()
    }

    /// Project-wide windows apply to every monitor; scoped ones to theirs.
    pub fn applies_to_monitor(&self, monitor_id: Uuid) -> bool {
        self.monitor_id.is_none_or(|id| id == monitor_id)
    }

    /// Heartbeats are only covered by project-wide windows.
    pub fn applies_to_heartbeats(&self) -> bool {
        self.monitor_id.is_none()
    }
}

/// The windows of one project in effect at one instant. Only
/// [`active_windows`] hands one out (the field and the constructor are
/// private), so the coverage questions below can never be asked of windows
/// that were not filtered by [`MaintenanceWindow::covers`] first.
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveWindows(Vec<MaintenanceWindow>);

impl ActiveWindows {
    /// The windows of `windows` that are in effect at `at`.
    fn in_effect(windows: Vec<MaintenanceWindow>, at: DateTime<Utc>) -> ActiveWindows {
        ActiveWindows(windows.into_iter().filter(|w| w.covers(at)).collect())
    }

    /// Whether one of these windows covers `monitor_id` (project-wide, or
    /// scoped to it).
    pub fn covers_monitor(&self, monitor_id: Uuid) -> bool {
        self.0.iter().any(|w| w.applies_to_monitor(monitor_id))
    }

    /// Whether one of these windows covers the project's heartbeats (only a
    /// project-wide one does).
    pub fn covers_heartbeats(&self) -> bool {
        self.0.iter().any(MaintenanceWindow::applies_to_heartbeats)
    }
}

/// A window as API responses show it: `active` = in effect right now.
#[derive(Debug, Serialize)]
pub struct WindowView {
    #[serde(flatten)]
    pub window: MaintenanceWindow,
    pub active: bool,
}

impl WindowView {
    pub fn new(window: MaintenanceWindow, now: DateTime<Utc>) -> WindowView {
        let active = window.covers(now);
        WindowView { window, active }
    }
}

/// `POST /projects/{id}/maintenance-windows` body.
#[derive(Debug, Deserialize)]
pub struct CreateWindowRequest {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

/// `PATCH` body: absent (or `null`) fields keep their value; `monitor_id:
/// null` makes the window project-wide. Switching `kind` drops the other
/// kind's fields.
#[derive(Debug, Default, Deserialize)]
pub struct PatchWindowRequest {
    pub title: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub monitor_id: Option<Option<Uuid>>,
    pub kind: Option<String>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

/// The editable fields as received, before [`validate`].
#[derive(Debug, Clone, PartialEq)]
pub struct WindowConfig {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub timezone: Option<String>,
}

impl From<CreateWindowRequest> for WindowConfig {
    fn from(r: CreateWindowRequest) -> Self {
        WindowConfig {
            title: r.title,
            monitor_id: r.monitor_id,
            kind: r.kind,
            starts_at: r.starts_at,
            ends_at: r.ends_at,
            days_of_week: r.days_of_week,
            start_time: r.start_time,
            end_time: r.end_time,
            timezone: r.timezone,
        }
    }
}

impl From<&MaintenanceWindow> for WindowConfig {
    fn from(w: &MaintenanceWindow) -> Self {
        WindowConfig {
            title: w.title.clone(),
            monitor_id: w.monitor_id,
            kind: w.kind.clone(),
            starts_at: w.starts_at,
            ends_at: w.ends_at,
            days_of_week: w.days_of_week.clone(),
            start_time: w.start_time.map(|t| t.format("%H:%M:%S").to_string()),
            end_time: w.end_time.map(|t| t.format("%H:%M:%S").to_string()),
            timezone: w.timezone.clone(),
        }
    }
}

impl PatchWindowRequest {
    pub fn apply_to(self, base: WindowConfig) -> WindowConfig {
        WindowConfig {
            title: self.title.unwrap_or(base.title),
            monitor_id: self.monitor_id.unwrap_or(base.monitor_id),
            kind: self.kind.unwrap_or(base.kind),
            starts_at: self.starts_at.or(base.starts_at),
            ends_at: self.ends_at.or(base.ends_at),
            days_of_week: self.days_of_week.or(base.days_of_week),
            start_time: self.start_time.or(base.start_time),
            end_time: self.end_time.or(base.end_time),
            timezone: self.timezone.or(base.timezone),
        }
    }
}

/// A window ready to store: the other kind's fields are `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidWindow {
    pub title: String,
    pub monitor_id: Option<Uuid>,
    pub kind: String,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub days_of_week: Option<Vec<i16>>,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub timezone: Option<String>,
}

/// "HH:MM" or "HH:MM:SS", no fractions, no leap second.
fn parse_time(field: &str, raw: Option<&str>) -> Result<NaiveTime, String> {
    let raw = raw.map(str::trim).unwrap_or_default();
    let parsed = if raw.len() <= "HH:MM:SS".len() {
        NaiveTime::parse_from_str(raw, "%H:%M")
            .or_else(|_| NaiveTime::parse_from_str(raw, "%H:%M:%S"))
            .ok()
    } else {
        None
    };
    // chrono reads ":60" as a leap second, which Postgres' `time` refuses.
    parsed
        .filter(|t| t.nanosecond() < 1_000_000_000)
        .ok_or_else(|| format!("{field} must be a time of day as HH:MM or HH:MM:SS"))
}

/// The IANA zone `raw` names (case-sensitive, as the tz database spells it).
fn parse_timezone(raw: Option<&str>) -> Result<Tz, String> {
    raw.map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= MAX_TIMEZONE_LEN)
        .and_then(|name| Tz::from_str(name).ok())
        .ok_or_else(|| {
            "timezone must be an IANA time zone name such as Europe/Berlin or UTC".to_string()
        })
}

/// Fail-closed validation; the message is a 400's.
pub fn validate(c: WindowConfig) -> Result<ValidWindow, String> {
    let title = validate_label("title", &c.title)?;
    match c.kind.as_str() {
        "once" => {
            let (Some(starts_at), Some(ends_at)) = (c.starts_at, c.ends_at) else {
                return Err("starts_at and ends_at are required for one-off windows".into());
            };
            if ends_at <= starts_at {
                return Err("ends_at must be after starts_at".into());
            }
            if starts_at.year() < MIN_YEAR || ends_at.year() > MAX_YEAR {
                return Err(format!(
                    "starts_at and ends_at must be between the years {MIN_YEAR} and {MAX_YEAR}"
                ));
            }
            if ends_at - starts_at > TimeDelta::days(MAX_ONE_OFF_DAYS) {
                return Err(format!(
                    "a one-off window must last at most {MAX_ONE_OFF_DAYS} days"
                ));
            }
            Ok(ValidWindow {
                title,
                monitor_id: c.monitor_id,
                kind: c.kind,
                starts_at: Some(starts_at),
                ends_at: Some(ends_at),
                days_of_week: None,
                start_time: None,
                end_time: None,
                timezone: None,
            })
        }
        "weekly" => {
            let mut days = c.days_of_week.unwrap_or_default();
            if days.is_empty() || days.iter().any(|d| weekday_from_iso(*d).is_none()) {
                return Err("days_of_week must list ISO weekdays, 1 (Monday) to 7 (Sunday)".into());
            }
            days.sort_unstable();
            days.dedup();
            let start_time = parse_time("start_time", c.start_time.as_deref())?;
            let end_time = parse_time("end_time", c.end_time.as_deref())?;
            let zone = parse_timezone(c.timezone.as_deref())?;
            Ok(ValidWindow {
                title,
                monitor_id: c.monitor_id,
                kind: c.kind,
                starts_at: None,
                ends_at: None,
                days_of_week: Some(days),
                start_time: Some(start_time),
                end_time: Some(end_time),
                timezone: Some(zone.name().to_string()),
            })
        }
        _ => Err("kind must be once or weekly".into()),
    }
}

/// SQLSTATE 23503 (foreign_key_violation): the project (global admins skip
/// membership) or the monitor does not exist in the project — the composite
/// key refuses another project's monitor. Either way, a 404.
fn not_found_on_foreign_key(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
            AppError::NotFound
        }
        _ => AppError::Database(e),
    }
}

/// Stores a validated window, unless the project already holds
/// [`ProjectQuota::MAX_ROWS`] (a 400). A `monitor_id` outside the project (or
/// a project that does not exist) is a 404: the composite foreign key refuses
/// it, so there is no check-then-write race.
pub async fn insert_window(
    pool: &PgPool,
    project_id: Uuid,
    w: &ValidWindow,
) -> Result<MaintenanceWindow, AppError> {
    let mut tx = pool.begin().await?;
    ProjectQuota::MaintenanceWindows
        .check(&mut tx, project_id)
        .await?;
    let window = sqlx::query_as(
        "insert into maintenance_windows
             (project_id, monitor_id, title, kind, starts_at, ends_at, days_of_week, start_time, end_time, timezone)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         returning *",
    )
    .bind(project_id)
    .bind(w.monitor_id)
    .bind(&w.title)
    .bind(&w.kind)
    .bind(w.starts_at)
    .bind(w.ends_at)
    .bind(&w.days_of_week)
    .bind(w.start_time)
    .bind(w.end_time)
    .bind(&w.timezone)
    .fetch_one(&mut *tx)
    .await
    .map_err(not_found_on_foreign_key)?;
    tx.commit().await?;
    Ok(window)
}

/// Every window of the project (both kinds, in effect or not), newest first.
pub async fn list_for_project<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
) -> Result<Vec<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as(
        "select * from maintenance_windows where project_id = $1 order by created_at desc, id",
    )
    .bind(project_id)
    .fetch_all(executor)
    .await
}

/// Scoped by both ids: another project's window is not found (404).
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    window_id: Uuid,
) -> Result<Option<MaintenanceWindow>, sqlx::Error> {
    sqlx::query_as("select * from maintenance_windows where id = $1 and project_id = $2")
        .bind(window_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write under the row's lock, so two concurrent patches
/// apply one after the other. `Ok(None)`: no such window in the project. A
/// `monitor_id` outside the project is a 404, like any other foreign id.
pub async fn update_in_project(
    pool: &PgPool,
    project_id: Uuid,
    window_id: Uuid,
    patch: PatchWindowRequest,
) -> Result<Option<MaintenanceWindow>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<MaintenanceWindow> = sqlx::query_as(
        "select * from maintenance_windows where id = $1 and project_id = $2 for update",
    )
    .bind(window_id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let w = with_unchanged_values(vec![current.title.clone()], || {
        validate(patch.apply_to(WindowConfig::from(&current)))
    })
    .map_err(AppError::Validation)?;
    let updated: MaintenanceWindow = sqlx::query_as(
        "update maintenance_windows set
             monitor_id = $3, title = $4, kind = $5, starts_at = $6, ends_at = $7,
             days_of_week = $8, start_time = $9, end_time = $10, timezone = $11
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(window_id)
    .bind(project_id)
    .bind(w.monitor_id)
    .bind(&w.title)
    .bind(&w.kind)
    .bind(w.starts_at)
    .bind(w.ends_at)
    .bind(&w.days_of_week)
    .bind(w.start_time)
    .bind(w.end_time)
    .bind(&w.timezone)
    .fetch_one(&mut *tx)
    .await
    .map_err(not_found_on_foreign_key)?;
    tx.commit().await?;
    Ok(Some(updated))
}

pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    window_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from maintenance_windows where id = $1 and project_id = $2")
        .bind(window_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// The SQL prefilter shared by the coverage queries (`maybe_in_effect!`): weekly windows (decided
/// in Rust, see [`Schedule::covers`]) and the one-off windows in effect at
/// `$1`. One-off windows that are over, or not yet begun, are never loaded,
/// so they cost nothing however many pile up.
/// (A macro, so the queries stay `&'static str` through `concat!`.)
macro_rules! maybe_in_effect {
    () => {
        "(kind = 'weekly' or (starts_at <= $1 and ends_at > $1))"
    };
}

/// The windows of `project_id` (project-wide and monitor-scoped) in effect at
/// `at`: one query, for a whole monitor or heartbeat list.
pub async fn active_windows<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    at: DateTime<Utc>,
) -> Result<ActiveWindows, sqlx::Error> {
    let windows: Vec<MaintenanceWindow> = sqlx::query_as(concat!(
        "select * from maintenance_windows where project_id = $2 and ",
        maybe_in_effect!()
    ))
    .bind(at)
    .bind(project_id)
    .fetch_all(executor)
    .await?;
    Ok(ActiveWindows::in_effect(windows, at))
}

/// Whether a window covering `monitor_id` (project-wide or scoped to it) is
/// in effect at `at`.
pub async fn monitor_in_maintenance<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    monitor_id: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let windows: Vec<MaintenanceWindow> = sqlx::query_as(concat!(
        "select * from maintenance_windows
         where project_id = $2 and (monitor_id is null or monitor_id = $3) and ",
        maybe_in_effect!()
    ))
    .bind(at)
    .bind(project_id)
    .bind(monitor_id)
    .fetch_all(executor)
    .await?;
    Ok(windows.iter().any(|w| w.covers(at)))
}

/// Whether a project-wide window (the only kind covering heartbeats) of
/// `project_id` is in effect at `at`.
pub async fn heartbeat_in_maintenance<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    at: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let windows: Vec<MaintenanceWindow> = sqlx::query_as(concat!(
        "select * from maintenance_windows
         where project_id = $2 and monitor_id is null and ",
        maybe_in_effect!()
    ))
    .bind(at)
    .bind(project_id)
    .fetch_all(executor)
    .await?;
    Ok(windows.iter().any(|w| w.covers(at)))
}

/// The projects whose heartbeats a project-wide window covers at `at`, for
/// the heartbeat sweep to leave alone.
pub async fn projects_with_heartbeats_in_maintenance<'e>(
    executor: impl PgExecutor<'e>,
    at: DateTime<Utc>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let windows: Vec<MaintenanceWindow> = sqlx::query_as(concat!(
        "select * from maintenance_windows where monitor_id is null and ",
        maybe_in_effect!()
    ))
    .bind(at)
    .fetch_all(executor)
    .await?;
    let mut projects: Vec<Uuid> = windows
        .iter()
        .filter(|w| w.covers(at))
        .map(|w| w.project_id)
        .collect();
    projects.sort_unstable();
    projects.dedup();
    Ok(projects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap()
    }

    fn weekly(days: &[i16], start: &str, end: &str, zone: &str) -> Schedule {
        Schedule::Weekly {
            days: days.iter().map(|d| weekday_from_iso(*d).unwrap()).collect(),
            start: NaiveTime::parse_from_str(start, "%H:%M").unwrap(),
            end: NaiveTime::parse_from_str(end, "%H:%M").unwrap(),
            timezone: Tz::from_str(zone).unwrap(),
        }
    }

    #[test]
    fn a_one_off_window_includes_its_start_and_excludes_its_end() {
        let once = Schedule::Once {
            starts_at: utc(2026, 10, 13, 10, 0, 0),
            ends_at: utc(2026, 10, 13, 12, 0, 0),
        };
        assert!(!once.covers(utc(2026, 10, 13, 9, 59, 59)));
        assert!(once.covers(utc(2026, 10, 13, 10, 0, 0)));
        assert!(once.covers(utc(2026, 10, 13, 11, 59, 59)));
        assert!(!once.covers(utc(2026, 10, 13, 12, 0, 0)));
    }

    #[test]
    fn a_weekly_window_uses_wall_clock_time_in_its_zone() {
        // Mondays 09:00-17:00 in Berlin (CEST = UTC+2 on 2026-10-12).
        let office = weekly(&[1], "09:00", "17:00", "Europe/Berlin");
        assert!(
            !office.covers(utc(2026, 10, 12, 6, 59, 59)),
            "08:59:59 local"
        );
        assert!(office.covers(utc(2026, 10, 12, 7, 0, 0)), "09:00 local");
        assert!(
            office.covers(utc(2026, 10, 12, 14, 59, 59)),
            "16:59:59 local"
        );
        assert!(!office.covers(utc(2026, 10, 12, 15, 0, 0)), "17:00 local");
        assert!(
            !office.covers(utc(2026, 10, 13, 8, 0, 0)),
            "Tuesday is not listed"
        );
        // 09:00 UTC on that Monday is 05:00 in New York: not yet.
        assert!(
            !weekly(&[1], "09:00", "17:00", "America/New_York").covers(utc(2026, 10, 12, 9, 0, 0))
        );
    }

    #[test]
    fn the_local_weekday_decides_not_the_utc_one() {
        // Monday 06:00 in Jakarta (UTC+7) is still Sunday 23:00 in UTC.
        let monday_morning = weekly(&[1], "05:00", "07:00", "Asia/Jakarta");
        assert!(monday_morning.covers(utc(2026, 10, 18, 23, 0, 0)));
        assert!(
            !monday_morning.covers(utc(2026, 10, 19, 23, 0, 0)),
            "Tuesday there"
        );
    }

    #[test]
    fn a_window_ending_at_or_before_its_start_runs_past_midnight() {
        // Fridays 22:00 -> Saturday 02:00, UTC. 2026-10-16 is a Friday.
        let deploys = weekly(&[5], "22:00", "02:00", "UTC");
        assert!(!deploys.covers(utc(2026, 10, 16, 21, 59, 59)));
        assert!(deploys.covers(utc(2026, 10, 16, 22, 0, 0)));
        assert!(deploys.covers(utc(2026, 10, 16, 23, 59, 59)));
        assert!(
            deploys.covers(utc(2026, 10, 17, 1, 59, 59)),
            "Saturday morning, carried over"
        );
        assert!(!deploys.covers(utc(2026, 10, 17, 2, 0, 0)));
        assert!(
            !deploys.covers(utc(2026, 10, 17, 23, 0, 0)),
            "Saturday itself is not listed"
        );
        assert!(
            !deploys.covers(utc(2026, 10, 16, 1, 0, 0)),
            "Friday morning: Thursday is not listed"
        );
        assert!(
            !deploys.covers(utc(2026, 10, 15, 23, 0, 0)),
            "neither is Thursday"
        );
    }

    #[test]
    fn a_sunday_night_window_carries_over_into_monday() {
        // 2026-10-18 is a Sunday; Monday's weekday().pred() is Sunday.
        let backup = weekly(&[7], "23:00", "01:00", "UTC");
        assert!(backup.covers(utc(2026, 10, 18, 23, 30, 0)));
        assert!(backup.covers(utc(2026, 10, 19, 0, 30, 0)));
        assert!(!backup.covers(utc(2026, 10, 19, 1, 0, 0)));
        assert!(
            !backup.covers(utc(2026, 10, 19, 23, 30, 0)),
            "Monday night is not listed"
        );
        assert!(
            !backup.covers(utc(2026, 10, 18, 0, 30, 0)),
            "Sunday morning: Saturday is not listed"
        );
    }

    #[test]
    fn several_days_each_get_their_own_window() {
        // Mon, Wed, Fri 01:00-03:00 UTC. 2026-10-12 is a Monday.
        let mwf = weekly(&[1, 3, 5], "01:00", "03:00", "UTC");
        for (day, expected) in [
            (12, true),
            (13, false),
            (14, true),
            (15, false),
            (16, true),
            (17, false),
            (18, false),
        ] {
            assert_eq!(
                mwf.covers(utc(2026, 10, day, 2, 0, 0)),
                expected,
                "day {day}"
            );
            assert!(!mwf.covers(utc(2026, 10, day, 3, 0, 0)), "day {day} 03:00");
        }
        // Consecutive wrapping days chain into one long window.
        let nights = weekly(&[1, 2], "20:00", "20:00", "UTC");
        assert!(nights.covers(utc(2026, 10, 12, 20, 0, 0)));
        assert!(nights.covers(utc(2026, 10, 13, 19, 59, 59)));
        assert!(
            nights.covers(utc(2026, 10, 14, 19, 59, 59)),
            "Tuesday's runs into Wednesday"
        );
        assert!(!nights.covers(utc(2026, 10, 14, 20, 0, 0)));
    }

    #[test]
    fn equal_start_and_end_is_the_whole_day() {
        let wednesday = weekly(&[3], "00:00", "00:00", "UTC"); // 2026-10-14
        assert!(!wednesday.covers(utc(2026, 10, 13, 23, 59, 59)));
        assert!(wednesday.covers(utc(2026, 10, 14, 0, 0, 0)));
        assert!(wednesday.covers(utc(2026, 10, 14, 23, 59, 59)));
        assert!(!wednesday.covers(utc(2026, 10, 15, 0, 0, 0)));
    }

    #[test]
    fn daylight_saving_spring_forward_shortens_or_skips_a_window() {
        // Berlin, Sunday 2026-03-29: 02:00 CET jumps to 03:00 CEST (01:00 UTC).
        let across = weekly(&[7], "01:30", "03:30", "Europe/Berlin");
        assert!(!across.covers(utc(2026, 3, 29, 0, 29, 59)), "01:29:59 CET");
        assert!(across.covers(utc(2026, 3, 29, 0, 45, 0)), "01:45 CET");
        assert!(across.covers(utc(2026, 3, 29, 1, 15, 0)), "03:15 CEST");
        assert!(
            !across.covers(utc(2026, 3, 29, 1, 30, 0)),
            "03:30 CEST: over after one real hour"
        );
        let skipped = weekly(&[7], "02:00", "03:00", "Europe/Berlin");
        assert!(!skipped.covers(utc(2026, 3, 29, 0, 59, 59)), "01:59:59 CET");
        assert!(
            !skipped.covers(utc(2026, 3, 29, 1, 0, 0)),
            "03:00 CEST: the window's hour never existed"
        );
        // The next Sunday it is back: 02:30 CEST = 00:30 UTC.
        assert!(skipped.covers(utc(2026, 4, 5, 0, 30, 0)));
    }

    #[test]
    fn daylight_saving_fall_back_covers_the_repeated_hour_twice() {
        // Berlin, Sunday 2026-10-25: 03:00 CEST falls back to 02:00 CET (01:00 UTC).
        let repeated = weekly(&[7], "02:00", "03:00", "Europe/Berlin");
        assert!(
            !repeated.covers(utc(2026, 10, 24, 23, 59, 59)),
            "01:59:59 CEST"
        );
        assert!(
            repeated.covers(utc(2026, 10, 25, 0, 30, 0)),
            "02:30 CEST, first time"
        );
        assert!(
            repeated.covers(utc(2026, 10, 25, 1, 30, 0)),
            "02:30 CET, second time"
        );
        assert!(!repeated.covers(utc(2026, 10, 25, 2, 0, 0)), "03:00 CET");
    }

    #[test]
    fn a_dst_change_inside_a_midnight_wrap_follows_the_wall_clock() {
        // New York, Saturday 2026-03-07 22:00 EST -> Sunday 2026-03-08 04:00
        // EDT; the clocks jump at 02:00 EST (07:00 UTC) on Sunday.
        let overnight = weekly(&[6], "22:00", "04:00", "America/New_York");
        assert!(
            overnight.covers(utc(2026, 3, 8, 3, 0, 0)),
            "22:00 EST Saturday"
        );
        assert!(
            overnight.covers(utc(2026, 3, 8, 6, 59, 59)),
            "01:59:59 EST Sunday"
        );
        assert!(
            overnight.covers(utc(2026, 3, 8, 7, 0, 0)),
            "03:00 EDT Sunday"
        );
        assert!(
            !overnight.covers(utc(2026, 3, 8, 8, 0, 0)),
            "04:00 EDT: five real hours"
        );
    }

    #[test]
    fn iso_weekdays_map_monday_to_one_and_sunday_to_seven() {
        assert_eq!(weekday_from_iso(1), Some(Weekday::Mon));
        assert_eq!(weekday_from_iso(7), Some(Weekday::Sun));
        assert_eq!(weekday_from_iso(0), None);
        assert_eq!(weekday_from_iso(8), None);
        assert_eq!(weekday_from_iso(-1), None);
        assert_eq!(weekday_from_iso(i16::MAX), None);
    }

    fn row() -> MaintenanceWindow {
        MaintenanceWindow {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            monitor_id: None,
            title: "broken".to_string(),
            kind: "weekly".to_string(),
            starts_at: None,
            ends_at: None,
            days_of_week: Some(vec![1, 2, 3, 4, 5, 6, 7]),
            start_time: NaiveTime::from_hms_opt(0, 0, 0),
            end_time: NaiveTime::from_hms_opt(0, 0, 0),
            timezone: Some("Mars/Olympus_Mons".to_string()),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_row_that_does_not_form_a_schedule_covers_nothing() {
        let broken = row();
        assert_eq!(broken.schedule(), None);
        assert!(!broken.covers(Utc::now()));
        let every_day = MaintenanceWindow {
            timezone: Some("UTC".to_string()),
            ..broken.clone()
        };
        assert!(
            every_day.covers(Utc::now()),
            "the same row with a real zone covers every moment"
        );
        let unknown_kind = MaintenanceWindow {
            kind: "daily".to_string(),
            ..every_day.clone()
        };
        assert!(!unknown_kind.covers(Utc::now()));
        let bad_day = MaintenanceWindow {
            days_of_week: Some(vec![1, 9]),
            ..every_day
        };
        assert!(!bad_day.covers(Utc::now()));
    }

    #[test]
    fn scoped_windows_cover_their_monitor_and_no_heartbeat() {
        let monitor = Uuid::new_v4();
        let project_wide = MaintenanceWindow {
            timezone: Some("UTC".to_string()),
            ..row()
        };
        let scoped = MaintenanceWindow {
            monitor_id: Some(monitor),
            ..project_wide.clone()
        };
        assert!(project_wide.applies_to_monitor(monitor) && project_wide.applies_to_heartbeats());
        assert!(scoped.applies_to_monitor(monitor));
        assert!(!scoped.applies_to_monitor(Uuid::new_v4()));
        assert!(!scoped.applies_to_heartbeats());
        let now = Utc::now();
        let scoped_only = ActiveWindows::in_effect(vec![scoped], now);
        assert!(scoped_only.covers_monitor(monitor));
        assert!(!scoped_only.covers_monitor(Uuid::new_v4()));
        assert!(!scoped_only.covers_heartbeats());
        assert!(ActiveWindows::in_effect(vec![project_wide.clone()], now).covers_heartbeats());
        // Windows not in effect are dropped before any question is asked.
        let broken = row();
        let over = MaintenanceWindow {
            kind: "once".to_string(),
            days_of_week: None,
            start_time: None,
            end_time: None,
            timezone: None,
            starts_at: Some(now - TimeDelta::hours(2)),
            ends_at: Some(now - TimeDelta::hours(1)),
            ..project_wide
        };
        let inactive = ActiveWindows::in_effect(vec![broken, over], now);
        assert!(!inactive.covers_heartbeats() && !inactive.covers_monitor(monitor));
    }

    fn config(kind: &str) -> WindowConfig {
        WindowConfig {
            title: "Deploy".to_string(),
            monitor_id: None,
            kind: kind.to_string(),
            starts_at: Some(utc(2026, 10, 13, 10, 0, 0)),
            ends_at: Some(utc(2026, 10, 13, 12, 0, 0)),
            days_of_week: Some(vec![5, 1, 5]),
            start_time: Some(" 22:00 ".to_string()),
            end_time: Some("02:00:30".to_string()),
            timezone: Some("Asia/Jakarta".to_string()),
        }
    }

    #[test]
    fn validation_keeps_only_the_fields_of_the_chosen_kind() {
        let once = validate(config("once")).unwrap();
        assert_eq!(
            (
                once.days_of_week,
                once.start_time,
                once.end_time,
                once.timezone
            ),
            (None, None, None, None)
        );
        let weekly = validate(config("weekly")).unwrap();
        assert_eq!((weekly.starts_at, weekly.ends_at), (None, None));
        assert_eq!(weekly.days_of_week, Some(vec![1, 5]));
        assert_eq!(weekly.start_time, NaiveTime::from_hms_opt(22, 0, 0));
        assert_eq!(weekly.end_time, NaiveTime::from_hms_opt(2, 0, 30));
        assert_eq!(weekly.timezone.as_deref(), Some("Asia/Jakarta"));
    }

    #[test]
    fn one_off_windows_are_bounded() {
        let start = utc(2026, 1, 1, 0, 0, 0);
        let at_most = WindowConfig {
            starts_at: Some(start),
            ends_at: Some(start + TimeDelta::days(MAX_ONE_OFF_DAYS)),
            ..config("once")
        };
        assert!(validate(at_most.clone()).is_ok());
        let too_long = WindowConfig {
            ends_at: Some(start + TimeDelta::days(MAX_ONE_OFF_DAYS) + TimeDelta::seconds(1)),
            ..at_most
        };
        assert!(validate(too_long).unwrap_err().contains("at most 366 days"));
        let ancient = WindowConfig {
            starts_at: Some(utc(1969, 12, 31, 23, 0, 0)),
            ends_at: Some(utc(1970, 1, 1, 1, 0, 0)),
            ..config("once")
        };
        assert!(validate(ancient).unwrap_err().contains("between the years"));
    }

    #[test]
    fn times_and_zones_are_parsed_strictly() {
        for bad in [
            "24:00",
            "23:60",
            "23:59:60",
            "7",
            "07:00:00.1",
            "07:00 PM",
            "",
            "x",
        ] {
            let c = WindowConfig {
                start_time: Some(bad.to_string()),
                ..config("weekly")
            };
            assert!(
                validate(c)
                    .unwrap_err()
                    .starts_with("start_time must be a time of day"),
                "{bad:?}"
            );
        }
        for bad in [
            "",
            "europe/berlin",
            "UTC+7",
            "Mars/Olympus_Mons",
            "../../etc/passwd",
        ] {
            let c = WindowConfig {
                timezone: Some(bad.to_string()),
                ..config("weekly")
            };
            assert!(
                validate(c)
                    .unwrap_err()
                    .starts_with("timezone must be an IANA"),
                "{bad:?}"
            );
        }
        let long = WindowConfig {
            timezone: Some("A".repeat(10_000)),
            ..config("weekly")
        };
        assert!(validate(long).is_err());
        let trimmed = WindowConfig {
            timezone: Some(" UTC ".to_string()),
            ..config("weekly")
        };
        assert_eq!(validate(trimmed).unwrap().timezone.as_deref(), Some("UTC"));
    }

    #[test]
    fn a_patch_keeps_what_it_does_not_mention() {
        let base = config("weekly");
        let monitor = Uuid::new_v4();
        let patched = PatchWindowRequest {
            title: Some("New".to_string()),
            monitor_id: Some(Some(monitor)),
            ..Default::default()
        }
        .apply_to(base.clone());
        assert_eq!(patched.title, "New");
        assert_eq!(patched.monitor_id, Some(monitor));
        assert_eq!(patched.timezone, base.timezone);
        let cleared = PatchWindowRequest {
            monitor_id: Some(None),
            ..Default::default()
        }
        .apply_to(patched);
        assert_eq!(cleared.monitor_id, None);
    }

    #[test]
    fn one_off_spans_are_clipped_to_the_range() {
        let w = Schedule::Once {
            starts_at: utc(2026, 1, 1, 10, 0, 0),
            ends_at: utc(2026, 1, 1, 12, 0, 0),
        };
        assert_eq!(
            w.spans(utc(2026, 1, 1, 11, 0, 0), utc(2026, 1, 2, 0, 0, 0)),
            vec![Span {
                start: utc(2026, 1, 1, 11, 0, 0),
                end: utc(2026, 1, 1, 12, 0, 0)
            }]
        );
        assert!(
            w.spans(utc(2026, 1, 2, 0, 0, 0), utc(2026, 1, 3, 0, 0, 0))
                .is_empty()
        );
        // Touching the range edge from outside is empty.
        assert!(
            w.spans(utc(2026, 1, 1, 12, 0, 0), utc(2026, 1, 3, 0, 0, 0))
                .is_empty()
        );
    }

    #[test]
    fn weekly_spans_agree_with_covers_every_five_minutes_for_two_years() {
        for (days, start, end, zone) in [
            (&[1, 3, 5][..], "02:00", "04:00", "Europe/Berlin"),
            // Wraps midnight into Monday.
            (&[7][..], "23:00", "01:00", "America/New_York"),
            // Sundays: crosses the March gap and the October repeated hour.
            (&[7][..], "01:30", "03:30", "Europe/Berlin"),
            // Full days.
            (&[1, 2, 3, 4, 5, 6, 7][..], "00:00", "00:00", "Asia/Jakarta"),
        ] {
            let w = weekly(days, start, end, zone);
            let (from, to) = (utc(2026, 1, 1, 0, 0, 0), utc(2028, 1, 1, 0, 0, 0));
            // Merged first so one forward cursor suffices: ~210k samples per
            // case stay fast in debug builds.
            let spans = crate::uptime::availability::union(w.spans(from, to));
            let (mut at, mut next) = (from, 0);
            while at < to {
                while next < spans.len() && spans[next].end <= at {
                    next += 1;
                }
                let in_span = next < spans.len() && spans[next].start <= at;
                assert_eq!(in_span, w.covers(at), "{zone} {start}-{end} at {at}");
                at += TimeDelta::minutes(5);
            }
        }
    }

    #[test]
    fn a_weekly_span_clips_to_a_range_that_starts_inside_it() {
        // Sunday 23:00-01:00 New York; range starts Monday 00:30 local.
        let w = weekly(&[7], "23:00", "01:00", "America/New_York");
        let from = utc(2026, 1, 5, 5, 30, 0); // Mon 00:30 EST
        let spans = w.spans(from, utc(2026, 1, 6, 0, 0, 0));
        assert_eq!(
            spans,
            vec![Span {
                start: from,
                end: utc(2026, 1, 5, 6, 0, 0)
            }]
        );
    }

    #[test]
    fn dst_edge_cases_of_weekly_spans() {
        // Starts inside the March gap (02:30 does not exist on 2026-03-29):
        // the window begins when the clock first shows 03:00, as `covers` does.
        let gap = weekly(&[7], "02:30", "04:00", "Europe/Berlin");
        let (from, to) = (utc(2026, 3, 29, 0, 0, 0), utc(2026, 3, 30, 0, 0, 0));
        assert_eq!(
            gap.spans(from, to),
            vec![Span {
                start: utc(2026, 3, 29, 1, 0, 0),
                end: utc(2026, 3, 29, 2, 0, 0)
            }]
        );
        assert!(!gap.covers(utc(2026, 3, 29, 0, 59, 59)));
        assert!(gap.covers(utc(2026, 3, 29, 1, 0, 0)));
        // Starts inside the October repeated hour: one span from the earliest
        // start to the latest end; `covers` has a hole in between (documented
        // over-count of at most one hour, once a year).
        let repeated = weekly(&[7], "02:30", "04:00", "Europe/Berlin");
        let (from, to) = (utc(2026, 10, 25, 0, 0, 0), utc(2026, 10, 26, 0, 0, 0));
        assert_eq!(
            repeated.spans(from, to),
            vec![Span {
                start: utc(2026, 10, 25, 0, 30, 0),
                end: utc(2026, 10, 25, 3, 0, 0)
            }]
        );
        assert!(!repeated.covers(utc(2026, 10, 25, 1, 0, 0)));
    }
}
