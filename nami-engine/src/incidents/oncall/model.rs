//! On-call schedules, their participants and overrides (validation and SQL),
//! and what the API shows of them: who is on call now, and the shifts of a
//! range with each user's email and membership.

use std::collections::{HashMap, HashSet};

use chrono::{
    DateTime, LocalResult, Months, NaiveDate, NaiveTime, Offset, TimeDelta, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::core::error::AppError;
use crate::core::validate::validate_name;
use crate::incidents::oncall::rotation::{self, Override, Rotation, RotationLength};
use crate::incidents::timewindow::{TimeWindow, parse_hhmm, parse_timezone};

pub const MAX_SCHEDULES_PER_PROJECT: i64 = 50;
pub const MAX_PARTICIPANTS: usize = 50;
pub const MAX_RESTRICTIONS: usize = 14;
pub const MIN_ROTATION_HOURS: i32 = 1;
pub const MAX_ROTATION_HOURS: i32 = 8760;
/// A new `start_date` lies at most this many years before or after today;
/// a replace that keeps the stored one only needs it in
/// [`EARLIEST_TIME`]..[`LATEST_TIME`] ([`check_start_date`]).
pub const MAX_START_DATE_YEARS: u32 = 5;
pub const MAX_OVERRIDE_DAYS: i64 = 90;
/// Overrides of one schedule that have not ended yet.
pub const MAX_OVERRIDES_PER_SCHEDULE: i64 = 100;
/// The default shifts range, and how far ahead "on call now" looks for `until`.
pub const DEFAULT_SHIFT_DAYS: i64 = 14;
pub const MAX_SHIFT_DAYS: i64 = 31;
/// The earliest instant the API takes (shift ranges, override times).
pub const EARLIEST_TIME: DateTime<Utc> = DateTime::UNIX_EPOCH;
/// The latest instant the API takes, 2100-01-01T00:00:00Z. With
/// [`EARLIEST_TIME`] it keeps the rotation math far from chrono's limits,
/// where `rotation::shift_index` would not end.
pub const LATEST_TIME: DateTime<Utc> = match DateTime::from_timestamp(4_102_444_800, 0) {
    Some(t) => t,
    None => panic!("2100-01-01 is within chrono's range"),
};

/// A schedule as the API returns it.
#[derive(Debug, Clone, Serialize)]
pub struct ScheduleView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub timezone: String,
    pub rotation: String,
    pub rotation_hours: Option<i32>,
    /// "HH:MM", local to `timezone`.
    pub handoff_time: String,
    pub start_date: NaiveDate,
    pub restrictions: Vec<TimeWindow>,
    pub participants: Vec<Participant>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A user of a schedule. Users who are no longer project members are listed
/// with `is_member: false` and never paged (R7).
#[derive(Debug, Clone, Serialize)]
pub struct Participant {
    pub user_id: Uuid,
    pub email: String,
    pub is_member: bool,
}

/// The body of a schedule create or replace.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleInput {
    pub name: String,
    pub timezone: String,
    pub rotation: String,
    pub rotation_hours: Option<i32>,
    pub handoff_time: String,
    pub start_date: NaiveDate,
    #[serde(default)]
    pub restrictions: Vec<TimeWindow>,
    pub participants: Vec<Uuid>,
}

/// A [`ScheduleInput`] that passed [`validate`].
#[derive(Debug, Clone)]
pub struct ValidSchedule {
    name: String,
    timezone: Tz,
    length: RotationLength,
    handoff: NaiveTime,
    start_date: NaiveDate,
    restrictions: Vec<TimeWindow>,
    participants: Vec<Uuid>,
}

/// Checks a schedule body; `Err` carries a 400's message. `start_date`
/// ([`check_start_date`]) and whether the participants are project members
/// are checked when writing.
pub fn validate(input: ScheduleInput) -> Result<ValidSchedule, String> {
    let name = validate_name(&input.name)?;
    let timezone = parse_timezone(&input.timezone)?;
    let length = match (input.rotation.as_str(), input.rotation_hours) {
        ("daily", None) => RotationLength::Daily,
        ("weekly", None) => RotationLength::Weekly,
        ("custom", Some(hours)) if (MIN_ROTATION_HOURS..=MAX_ROTATION_HOURS).contains(&hours) => {
            RotationLength::Hours(hours.unsigned_abs())
        }
        ("custom", Some(_)) => {
            return Err(format!(
                "rotation_hours must be between {MIN_ROTATION_HOURS} and {MAX_ROTATION_HOURS}"
            ));
        }
        ("custom", None) => return Err("rotation_hours is required for a custom rotation".into()),
        ("daily" | "weekly", Some(_)) => {
            return Err("rotation_hours is only allowed for a custom rotation".into());
        }
        _ => return Err("rotation must be daily, weekly or custom".into()),
    };
    let handoff = parse_hhmm("handoff_time", &input.handoff_time)?;
    if input.restrictions.len() > MAX_RESTRICTIONS {
        return Err(format!("at most {MAX_RESTRICTIONS} restrictions are allowed"));
    }
    let mut restrictions = input.restrictions;
    for window in &mut restrictions {
        window.validate(timezone).map_err(|e| format!("restrictions: {e}"))?;
        window.weekdays.sort_unstable();
    }
    if !(1..=MAX_PARTICIPANTS).contains(&input.participants.len()) {
        return Err(format!("participants must list 1 to {MAX_PARTICIPANTS} users"));
    }
    let mut seen = HashSet::new();
    if !input.participants.iter().all(|user| seen.insert(user)) {
        return Err("participants must list each user once".into());
    }
    Ok(ValidSchedule {
        name,
        timezone,
        length,
        handoff,
        start_date: input.start_date,
        restrictions,
        participants: input.participants,
    })
}

/// A new `start_date` (create, or a replace that changes it) lies within
/// [`MAX_START_DATE_YEARS`] of `today`. A replace that keeps the `stored`
/// one accepts it anywhere from [`EARLIEST_TIME`] to before [`LATEST_TIME`],
/// so a schedule that has run for years can still be edited. `Err` carries a
/// 400's message.
pub fn check_start_date(
    start_date: NaiveDate,
    stored: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<(), String> {
    if stored == Some(start_date) {
        let (earliest, latest) = (EARLIEST_TIME.date_naive(), LATEST_TIME.date_naive());
        if !(earliest..latest).contains(&start_date) {
            return Err(format!("start_date must be on or after {earliest} and before {latest}"));
        }
        return Ok(());
    }
    let years = Months::new(12 * MAX_START_DATE_YEARS);
    let too_early = today.checked_sub_months(years).is_none_or(|d| start_date < d);
    let too_late = today.checked_add_months(years).is_none_or(|d| start_date > d);
    if too_early || too_late {
        return Err(format!("start_date must be within {MAX_START_DATE_YEARS} years of today"));
    }
    Ok(())
}

fn out_of_range(fields: &str) -> String {
    format!("{fields} must lie between {EARLIEST_TIME:?} and {LATEST_TIME:?}")
}

/// Checks an override's times at `now`; `Err` carries a 400's message.
pub fn check_override(
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), String> {
    if starts_at < EARLIEST_TIME || ends_at > LATEST_TIME {
        return Err(out_of_range("starts_at and ends_at"));
    }
    if ends_at <= starts_at {
        return Err("ends_at must be after starts_at".into());
    }
    if ends_at <= now {
        return Err("ends_at must be in the future".into());
    }
    if ends_at - starts_at > TimeDelta::days(MAX_OVERRIDE_DAYS) {
        return Err(format!("an override lasts at most {MAX_OVERRIDE_DAYS} days"));
    }
    Ok(())
}

/// The range of a shifts request: `from` defaults to `now`, `to` to `from`
/// plus [`DEFAULT_SHIFT_DAYS`]. Both lie in [`EARLIEST_TIME`]..=[`LATEST_TIME`],
/// `from` before `to`, at most [`MAX_SHIFT_DAYS`] apart; `Err` carries a
/// 400's message.
pub fn shift_range(
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<(DateTime<Utc>, DateTime<Utc>), String> {
    let from = from.unwrap_or(now);
    let in_range = |t: &DateTime<Utc>| (EARLIEST_TIME..=LATEST_TIME).contains(t);
    if !in_range(&from) || to.is_some_and(|t| !in_range(&t)) {
        return Err(out_of_range("from and to"));
    }
    // `from` is in range here, so the sum cannot overflow.
    let to = to.unwrap_or_else(|| (from + TimeDelta::days(DEFAULT_SHIFT_DAYS)).min(LATEST_TIME));
    if from >= to {
        return Err("from must be before to".into());
    }
    if to - from > TimeDelta::days(MAX_SHIFT_DAYS) {
        return Err(format!("the range must be at most {MAX_SHIFT_DAYS} days"));
    }
    Ok((from, to))
}

/// Who is on call in a schedule now, and until when.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OnCallNow {
    pub user_id: Uuid,
    pub email: String,
    pub until: DateTime<Utc>,
}

/// One merged shift as the API returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShiftView {
    pub user_id: Uuid,
    pub email: String,
    pub is_member: bool,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub override_id: Option<Uuid>,
}

/// One schedule with what evaluating it needs: its rotation, its overrides
/// in the loaded range, its restrictions' edge times, and the email and
/// membership of everyone in either.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub view: ScheduleView,
    rotation: Rotation,
    overrides: Vec<Override>,
    edges: Vec<NaiveTime>,
    people: HashMap<Uuid, Participant>,
}

impl Loaded {
    /// The merged shifts in `[from, to)` (`rotation::shifts`), within the
    /// loaded range. CPU-bound: call it off the async workers.
    pub fn shifts(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<ShiftView> {
        rotation::shifts(&self.rotation, &self.overrides, from, to)
            .into_iter()
            .map(|shift| {
                let person = self.people.get(&shift.user_id);
                ShiftView {
                    user_id: shift.user_id,
                    email: person.map(|p| p.email.clone()).unwrap_or_default(),
                    is_member: person.is_some_and(|p| p.is_member),
                    starts_at: shift.starts_at,
                    ends_at: shift.ends_at,
                    override_id: shift.override_id,
                }
            })
            .collect()
    }

    /// Who is on call at `now`, until the end of their merged shift (at most
    /// [`DEFAULT_SHIFT_DAYS`] ahead, the range to load); `None` when nobody
    /// is. The first of [`Self::shifts`] when it starts at `now`, at the cost
    /// of [`current_shift`] (about a thousand evaluations at most, not one
    /// per minute). Run it off the async workers.
    pub fn on_call_now(&self, now: DateTime<Utc>) -> Option<OnCallNow> {
        let horizon = now + TimeDelta::days(DEFAULT_SHIFT_DAYS);
        let (user_id, _, until) =
            current_shift(&self.rotation, &self.overrides, &self.edges, now, horizon)?;
        let email = self.people.get(&user_id).map(|p| p.email.clone()).unwrap_or_default();
        Some(OnCallNow { user_id, email, until })
    }
}

/// `(user, override id, until)`: who [`rotation::on_call_at`] puts on call
/// at `now`, and the first instant in `(now, horizon)` where its answer
/// changes, else `horizon`. Equals the first of `rotation::shifts(now,
/// horizon)` when that starts at `now`. It evaluates only
/// [`change_candidates`], in order, and stops at the first change.
fn current_shift(
    r: &Rotation,
    overrides: &[Override],
    edges: &[NaiveTime],
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Option<(Uuid, Option<Uuid>, DateTime<Utc>)> {
    let current = rotation::on_call_at(r, overrides, now)?;
    let until = change_candidates(r, overrides, edges, now, horizon)
        .into_iter()
        .find(|t| rotation::on_call_at(r, overrides, *t) != Some(current))
        .unwrap_or(horizon);
    Some((current.0, current.1, until))
}

/// Every instant in `(now, horizon)` where `on_call_at` can change, sorted:
/// override edges and rotation handoffs and, when restrictions apply, each
/// restriction edge (`edges`, local) on each local day (both instants of
/// an ambiguous time) and every UTC offset change. Restrictions compare the
/// wall clock, so they can also flip where it jumps, at no edge: a window
/// ending at 02:30 turns back on when 03:00 falls back to 02:00.
fn change_candidates(
    r: &Rotation,
    overrides: &[Override],
    edges: &[NaiveTime],
    now: DateTime<Utc>,
    horizon: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    let mut out: Vec<DateTime<Utc>> =
        overrides.iter().flat_map(|o| [o.starts_at, o.ends_at]).collect();
    if !r.participants.is_empty() {
        // The handoff after `now` (`shift_start` grows with k, and is
        // `MAX_UTC` past chrono's range).
        let mut k = rotation::shift_index(r, now).map_or(0, |k| k + 1);
        loop {
            let start = rotation::shift_start(r, k);
            if start >= horizon {
                break;
            }
            out.push(start);
            k += 1;
        }
    }
    if !r.participants.is_empty() && !r.restrictions.is_empty() {
        let tz = r.timezone;
        let mut day = now.with_timezone(&tz).date_naive() - TimeDelta::days(1);
        let last = horizon.with_timezone(&tz).date_naive() + TimeDelta::days(1);
        while day <= last {
            for edge in edges {
                match tz.from_local_datetime(&day.and_time(*edge)) {
                    LocalResult::Single(t) => out.push(t.with_timezone(&Utc)),
                    LocalResult::Ambiguous(a, b) => {
                        out.extend([a.with_timezone(&Utc), b.with_timezone(&Utc)]);
                    }
                    // Skipped by a spring-forward: the offset change below.
                    LocalResult::None => {}
                }
            }
            day += TimeDelta::days(1);
        }
        // Hourly samples, then the exact second: real zones never change
        // their offset twice within an hour.
        let offset = |s: i64| {
            DateTime::from_timestamp(s, 0)
                .map(|t| tz.offset_from_utc_datetime(&t.naive_utc()).fix())
        };
        let (mut s, end) = (now.timestamp(), horizon.timestamp());
        while s < end {
            let next = (s + 3600).min(end);
            if offset(next) != offset(s) {
                let (mut lo, mut hi) = (s, next);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if offset(mid) == offset(s) {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                out.extend(DateTime::from_timestamp(hi, 0));
            }
            s = next;
        }
    }
    out.retain(|t| now < *t && *t < horizon);
    out.sort_unstable();
    out.dedup();
    out
}

/// The distinct local times where restriction windows start or end.
fn edge_times(windows: &[TimeWindow]) -> Vec<NaiveTime> {
    let mut edges: Vec<NaiveTime> = windows
        .iter()
        .flat_map(|w| [parse_hhmm("from", &w.from).ok(), parse_hhmm("to", &w.to).ok()])
        .flatten()
        .collect();
    edges.sort_unstable();
    edges.dedup();
    edges
}

#[derive(sqlx::FromRow)]
struct ScheduleRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    timezone: String,
    rotation: String,
    rotation_hours: Option<i32>,
    handoff_time: NaiveTime,
    start_date: NaiveDate,
    restrictions: sqlx::types::Json<Vec<TimeWindow>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct PersonRow {
    schedule_id: Uuid,
    user_id: Uuid,
    email: String,
    is_member: bool,
}

#[derive(sqlx::FromRow)]
struct OverrideRow {
    schedule_id: Uuid,
    id: Uuid,
    user_id: Uuid,
    email: String,
    is_member: bool,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
}

/// The schedules a query reads: `$1` the project and `$2` the schedule
/// (null = any), with `s` the schedule. A macro because sqlx takes SQL as
/// one literal.
macro_rules! in_scope {
    () => {
        "($1::uuid is null or s.project_id = $1) and ($2::uuid is null or s.id = $2)"
    };
}

/// The columns of an [`OverrideView`], with `o` the override and `u` its user.
macro_rules! override_columns {
    () => {
        "o.id, o.user_id, u.email, o.starts_at, o.ends_at, o.created_by, o.created_at"
    };
}

/// Schedules by name with their overrides that overlap `[from, to]`.
async fn load(
    conn: &mut PgConnection,
    project_id: Option<Uuid>,
    schedule_id: Option<Uuid>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Loaded>, sqlx::Error> {
    let rows: Vec<ScheduleRow> = sqlx::query_as(concat!(
        "select s.id, s.project_id, s.name, s.timezone, s.rotation, s.rotation_hours, s.handoff_time,
                s.start_date, s.restrictions, s.created_at, s.updated_at
         from on_call_schedules s where ",
        in_scope!(),
        " order by s.name, s.id"
    ))
    .bind(project_id)
    .bind(schedule_id)
    .fetch_all(&mut *conn)
    .await?;
    let people: Vec<PersonRow> = sqlx::query_as(concat!(
        "select p.schedule_id, p.user_id, u.email,
                exists (select 1 from project_members m
                        where m.project_id = s.project_id and m.user_id = p.user_id) as is_member
         from on_call_participants p
         join on_call_schedules s on s.id = p.schedule_id
         join users u on u.id = p.user_id
         where ",
        in_scope!(),
        " order by p.schedule_id, p.position"
    ))
    .bind(project_id)
    .bind(schedule_id)
    .fetch_all(&mut *conn)
    .await?;
    let overrides: Vec<OverrideRow> = sqlx::query_as(concat!(
        "select o.schedule_id, o.id, o.user_id, u.email,
                exists (select 1 from project_members m
                        where m.project_id = s.project_id and m.user_id = o.user_id) as is_member,
                o.starts_at, o.ends_at, o.created_at
         from on_call_overrides o
         join on_call_schedules s on s.id = o.schedule_id
         join users u on u.id = o.user_id
         where ",
        in_scope!(),
        " and o.ends_at > $3 and o.starts_at <= $4"
    ))
    .bind(project_id)
    .bind(schedule_id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await?;

    let mut people_of: HashMap<Uuid, Vec<Participant>> = HashMap::new();
    for p in people {
        let person = Participant { user_id: p.user_id, email: p.email, is_member: p.is_member };
        people_of.entry(p.schedule_id).or_default().push(person);
    }
    let mut overrides_of: HashMap<Uuid, Vec<OverrideRow>> = HashMap::new();
    for o in overrides {
        overrides_of.entry(o.schedule_id).or_default().push(o);
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let participants = people_of.remove(&row.id).unwrap_or_default();
            let overrides = overrides_of.remove(&row.id).unwrap_or_default();
            loaded(row, participants, overrides)
        })
        .collect())
}

fn loaded(row: ScheduleRow, participants: Vec<Participant>, overrides: Vec<OverrideRow>) -> Loaded {
    let rotation = rotation_of(&row, participants.iter().map(|p| p.user_id).collect());
    let edges = edge_times(&row.restrictions);
    let mut people: HashMap<Uuid, Participant> =
        participants.iter().map(|p| (p.user_id, p.clone())).collect();
    for o in &overrides {
        people.entry(o.user_id).or_insert_with(|| Participant {
            user_id: o.user_id,
            email: o.email.clone(),
            is_member: o.is_member,
        });
    }
    let overrides = overrides
        .into_iter()
        .map(|o| Override {
            id: o.id,
            user_id: o.user_id,
            starts_at: o.starts_at,
            ends_at: o.ends_at,
            created_at: o.created_at,
        })
        .collect();
    let view = ScheduleView {
        id: row.id,
        project_id: row.project_id,
        name: row.name,
        timezone: row.timezone,
        rotation: row.rotation,
        rotation_hours: row.rotation_hours,
        handoff_time: row.handoff_time.format("%H:%M").to_string(),
        start_date: row.start_date,
        restrictions: row.restrictions.0,
        participants,
        created_at: row.created_at,
        updated_at: row.updated_at,
    };
    Loaded { view, rotation, overrides, edges, people }
}

/// The rotation a stored row describes. A row that no longer validates (a
/// zone a chrono-tz upgrade dropped) puts nobody on call through the
/// rotation rather than someone at the wrong hour; overrides still apply.
fn rotation_of(row: &ScheduleRow, participants: Vec<Uuid>) -> Rotation {
    let length = match (row.rotation.as_str(), row.rotation_hours) {
        ("daily", None) => Some(RotationLength::Daily),
        ("weekly", None) => Some(RotationLength::Weekly),
        ("custom", Some(hours)) => u32::try_from(hours).ok().map(RotationLength::Hours),
        _ => None,
    };
    let timezone = parse_timezone(&row.timezone).ok();
    let restrictions = timezone.and_then(|tz| {
        row.restrictions.iter().map(|w| w.validate(tz).ok()).collect::<Option<Vec<_>>>()
    });
    let valid = length.is_some() && restrictions.is_some();
    if !valid {
        tracing::warn!(
            schedule_id = %row.id,
            "stored schedule no longer validates; nobody is on call by rotation"
        );
    }
    Rotation {
        timezone: timezone.unwrap_or(Tz::UTC),
        start_date: row.start_date,
        handoff: row.handoff_time,
        length: length.unwrap_or(RotationLength::Daily),
        participants: if valid { participants } else { Vec::new() },
        restrictions: restrictions.unwrap_or_default(),
    }
}

/// The schedules of a project by name, with their overrides that overlap
/// `[from, to]`.
pub async fn list(
    pool: &PgPool,
    project_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<Loaded>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    load(&mut conn, Some(project_id), None, from, to).await
}

/// One schedule of a project, as [`list`] loads it; `None`: not in the
/// project.
pub async fn get(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Option<Loaded>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    Ok(load(&mut conn, Some(project_id), Some(id), from, to).await?.pop())
}

/// The rotation of one schedule and its overrides that overlap `[from, to]`,
/// for evaluation (the escalation engine passes `now` twice); `None`: no
/// such schedule. Participants who left the project are still in the
/// rotation: callers skip them (R7).
pub async fn load_rotation(
    conn: &mut PgConnection,
    schedule_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Option<(Rotation, Vec<Override>)>, sqlx::Error> {
    let loaded = load(conn, None, Some(schedule_id), from, to).await?.pop();
    Ok(loaded.map(|l| (l.rotation, l.overrides)))
}

/// `(rotation, rotation_hours)` as stored.
fn rotation_columns(length: RotationLength) -> (&'static str, Option<i32>) {
    match length {
        RotationLength::Daily => ("daily", None),
        RotationLength::Weekly => ("weekly", None),
        RotationLength::Hours(hours) => ("custom", i32::try_from(hours).ok()),
    }
}

fn name_taken(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            AppError::Conflict("a schedule with this name already exists".into())
        }
        _ => AppError::Database(e),
    }
}

/// 400 naming the first of `users` who is not a member of `project_id`.
async fn check_members(
    conn: &mut PgConnection,
    project_id: Uuid,
    users: &[Uuid],
) -> Result<(), AppError> {
    let outsider: Option<Uuid> = sqlx::query_scalar(
        "select t.u from unnest($2::uuid[]) with ordinality as t(u, n)
         where not exists (select 1 from project_members m where m.project_id = $1 and m.user_id = t.u)
         order by t.n limit 1",
    )
    .bind(project_id)
    .bind(users)
    .fetch_optional(conn)
    .await?;
    match outsider {
        Some(user) => Err(AppError::Validation(format!(
            "user {user} is not a member of this project"
        ))),
        None => Ok(()),
    }
}

/// Positions 0..n-1 in the order given.
async fn write_participants(
    conn: &mut PgConnection,
    schedule_id: Uuid,
    users: &[Uuid],
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into on_call_participants (schedule_id, position, user_id)
         select $1, (t.n - 1)::int, t.u from unnest($2::uuid[]) with ordinality as t(u, n)",
    )
    .bind(schedule_id)
    .bind(users)
    .execute(conn)
    .await?;
    Ok(())
}

/// Creates a schedule and returns its id. Errors: 404 for a project that
/// does not exist (a global admin passes the role check for any id), 409
/// for a taken name or a project at [`MAX_SCHEDULES_PER_PROJECT`], 400 for a
/// `start_date` [`check_start_date`] refuses or a participant who is not a
/// project member.
pub async fn create(pool: &PgPool, project_id: Uuid, v: &ValidSchedule) -> Result<Uuid, AppError> {
    check_start_date(v.start_date, None, Utc::now().date_naive())
        .map_err(AppError::Validation)?;
    let mut tx = pool.begin().await?;
    // Serialises creates in one project, so the count below is exact.
    let project: Option<Uuid> =
        sqlx::query_scalar("select id from projects where id = $1 for no key update")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    if project.is_none() {
        return Err(AppError::NotFound);
    }
    let count: i64 =
        sqlx::query_scalar("select count(*) from on_call_schedules where project_id = $1")
            .bind(project_id)
            .fetch_one(&mut *tx)
            .await?;
    if count >= MAX_SCHEDULES_PER_PROJECT {
        return Err(AppError::Conflict(format!(
            "schedule limit reached ({MAX_SCHEDULES_PER_PROJECT} per project)"
        )));
    }
    check_members(&mut tx, project_id, &v.participants).await?;
    let (rotation, hours) = rotation_columns(v.length);
    let id: Uuid = sqlx::query_scalar(
        "insert into on_call_schedules
            (project_id, name, timezone, rotation, rotation_hours, handoff_time, start_date, restrictions)
         values ($1, $2, $3, $4, $5, $6, $7, $8) returning id",
    )
    .bind(project_id)
    .bind(&v.name)
    .bind(v.timezone.name())
    .bind(rotation)
    .bind(hours)
    .bind(v.handoff)
    .bind(v.start_date)
    .bind(sqlx::types::Json(&v.restrictions))
    .fetch_one(&mut *tx)
    .await
    .map_err(name_taken)?;
    write_participants(&mut tx, id, &v.participants).await?;
    tx.commit().await?;
    Ok(id)
}

/// Replaces a schedule's fields and participants; `Ok(false)`: not in the
/// project. Errors as for [`create`], without the limit; `start_date` is
/// checked against the stored one.
pub async fn replace(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
    v: &ValidSchedule,
) -> Result<bool, AppError> {
    let mut tx = pool.begin().await?;
    let stored: Option<NaiveDate> = sqlx::query_scalar(
        "select start_date from on_call_schedules where id = $1 and project_id = $2 for update",
    )
    .bind(id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(stored) = stored else {
        return Ok(false);
    };
    check_start_date(v.start_date, Some(stored), Utc::now().date_naive())
        .map_err(AppError::Validation)?;
    let (rotation, hours) = rotation_columns(v.length);
    sqlx::query(
        "update on_call_schedules
         set name = $3, timezone = $4, rotation = $5, rotation_hours = $6, handoff_time = $7,
             start_date = $8, restrictions = $9, updated_at = now()
         where id = $1 and project_id = $2",
    )
    .bind(id)
    .bind(project_id)
    .bind(&v.name)
    .bind(v.timezone.name())
    .bind(rotation)
    .bind(hours)
    .bind(v.handoff)
    .bind(v.start_date)
    .bind(sqlx::types::Json(&v.restrictions))
    .execute(&mut *tx)
    .await
    .map_err(name_taken)?;
    check_members(&mut tx, project_id, &v.participants).await?;
    sqlx::query("delete from on_call_participants where schedule_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    write_participants(&mut tx, id, &v.participants).await?;
    tx.commit().await?;
    Ok(true)
}

/// Outcome of [`delete`].
pub enum Deleted {
    Deleted,
    NotFound,
    /// An escalation step targets the schedule (checked once escalation
    /// policies exist).
    InUse { policy: String },
}

/// Deletes a schedule with its participants and overrides.
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Deleted, sqlx::Error> {
    let result = sqlx::query("delete from on_call_schedules where id = $1 and project_id = $2")
        .bind(id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(if result.rows_affected() > 0 { Deleted::Deleted } else { Deleted::NotFound })
}

/// An override as the API returns it.
#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct OverrideView {
    pub id: Uuid,
    pub user_id: Uuid,
    pub email: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// Puts `user_id` on call in `[starts_at, ends_at)` (times checked by
/// [`check_override`]); `Ok(None)`: the schedule is not in the project.
/// Errors: 400 for a user who is not a project member, 409 for a schedule
/// at [`MAX_OVERRIDES_PER_SCHEDULE`].
pub async fn add_override(
    pool: &PgPool,
    project_id: Uuid,
    schedule_id: Uuid,
    user_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    created_by: Uuid,
) -> Result<Option<OverrideView>, AppError> {
    let mut tx = pool.begin().await?;
    // Serialises adds to one schedule, so the count below is exact.
    let schedule: Option<Uuid> = sqlx::query_scalar(
        "select id from on_call_schedules where id = $1 and project_id = $2 for no key update",
    )
    .bind(schedule_id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    if schedule.is_none() {
        return Ok(None);
    }
    check_members(&mut tx, project_id, &[user_id]).await?;
    let open: i64 = sqlx::query_scalar(
        "select count(*) from on_call_overrides where schedule_id = $1 and ends_at > now()",
    )
    .bind(schedule_id)
    .fetch_one(&mut *tx)
    .await?;
    if open >= MAX_OVERRIDES_PER_SCHEDULE {
        return Err(AppError::Conflict(format!(
            "override limit reached ({MAX_OVERRIDES_PER_SCHEDULE} current and upcoming per schedule)"
        )));
    }
    let created: OverrideView = sqlx::query_as(concat!(
        "with o as (insert into on_call_overrides (schedule_id, user_id, starts_at, ends_at, created_by)
                    values ($1, $2, $3, $4, $5) returning *)
         select ",
        override_columns!(),
        " from o join users u on u.id = o.user_id"
    ))
    .bind(schedule_id)
    .bind(user_id)
    .bind(starts_at)
    .bind(ends_at)
    .bind(created_by)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(created))
}

/// The overrides of a schedule that have not ended, by start; `None`: the
/// schedule is not in the project.
pub async fn list_overrides(
    pool: &PgPool,
    project_id: Uuid,
    schedule_id: Uuid,
) -> Result<Option<Vec<OverrideView>>, sqlx::Error> {
    let found: bool = sqlx::query_scalar(
        "select exists (select 1 from on_call_schedules where id = $1 and project_id = $2)",
    )
    .bind(schedule_id)
    .bind(project_id)
    .fetch_one(pool)
    .await?;
    if !found {
        return Ok(None);
    }
    sqlx::query_as(concat!(
        "select ",
        override_columns!(),
        " from on_call_overrides o join users u on u.id = o.user_id
          where o.schedule_id = $1 and o.ends_at > now() order by o.starts_at, o.id"
    ))
    .bind(schedule_id)
    .fetch_all(pool)
    .await
    .map(Some)
}

/// Deletes an override of a schedule in the project; `false`: none such.
pub async fn delete_override(
    pool: &PgPool,
    project_id: Uuid,
    schedule_id: Uuid,
    override_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "delete from on_call_overrides o using on_call_schedules s
         where o.id = $1 and o.schedule_id = $2 and s.id = o.schedule_id and s.project_id = $3",
    )
    .bind(override_id)
    .bind(schedule_id)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(s: &str) -> NaiveDate {
        s.parse().unwrap()
    }
    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    fn input() -> ScheduleInput {
        ScheduleInput {
            name: "primary".into(),
            timezone: "Europe/Berlin".into(),
            rotation: "weekly".into(),
            rotation_hours: None,
            handoff_time: "09:00".into(),
            start_date: date("2026-10-25"),
            restrictions: vec![],
            participants: vec![Uuid::from_u128(1)],
        }
    }
    fn window() -> TimeWindow {
        TimeWindow { weekdays: vec![5, 1], from: "09:00".into(), to: "17:00".into() }
    }

    #[test]
    fn a_new_start_date_lies_within_five_years_of_today() {
        let today = date("2026-10-25");
        let stored = Some(date("2026-01-01"));
        for (start, ok) in [
            ("2021-10-25", true),
            ("2021-10-24", false),
            ("2031-10-25", true),
            ("2031-10-26", false),
        ] {
            assert_eq!(check_start_date(date(start), None, today).is_ok(), ok, "create {start}");
            assert_eq!(check_start_date(date(start), stored, today).is_ok(), ok, "changed {start}");
        }
        // From a leap day, five years either way is the last day of February.
        let leap = date("2028-02-29");
        for (start, ok) in [
            ("2023-02-28", true),
            ("2023-02-27", false),
            ("2033-02-28", true),
            ("2033-03-01", false),
        ] {
            assert_eq!(check_start_date(date(start), None, leap).is_ok(), ok, "{start}");
        }
    }

    #[test]
    fn a_kept_start_date_only_has_to_lie_in_the_api_range() {
        let today = date("2026-10-25");
        let earliest = EARLIEST_TIME.date_naive();
        let latest = LATEST_TIME.date_naive();
        let day = TimeDelta::days(1);
        for (start, ok) in [
            (date("2016-10-25"), true),
            (earliest, true),
            (earliest - day, false),
            (latest - day, true),
            (latest, false),
        ] {
            assert_eq!(check_start_date(start, Some(start), today).is_ok(), ok, "kept {start}");
        }
        assert!(
            check_start_date(date("2016-10-25"), Some(date("2016-10-24")), today).is_err(),
            "a changed date gets the five-year rule"
        );
    }

    #[test]
    fn custom_rotation_hours_are_bounded() {
        for (hours, ok) in [
            (MIN_ROTATION_HOURS, true),
            (MAX_ROTATION_HOURS, true),
            (MIN_ROTATION_HOURS - 1, false),
            (MAX_ROTATION_HOURS + 1, false),
            (-1, false),
        ] {
            let custom = ScheduleInput {
                rotation: "custom".into(),
                rotation_hours: Some(hours),
                ..input()
            };
            assert_eq!(validate(custom).is_ok(), ok, "{hours}");
        }
        let weekly_with_hours = ScheduleInput { rotation_hours: Some(24), ..input() };
        assert!(validate(weekly_with_hours).is_err());
    }

    #[test]
    fn participants_and_restrictions_are_capped() {
        let users = |n: usize| (0..n).map(|i| Uuid::from_u128(i as u128 + 1)).collect::<Vec<_>>();
        let with =
            |participants, restrictions| ScheduleInput { participants, restrictions, ..input() };
        assert!(validate(with(users(MAX_PARTICIPANTS), vec![])).is_ok());
        assert!(validate(with(users(MAX_PARTICIPANTS + 1), vec![])).is_err());
        let valid = validate(with(users(1), vec![window(); MAX_RESTRICTIONS])).unwrap();
        assert_eq!(valid.restrictions[0].weekdays, [1, 5], "weekdays are stored sorted");
        assert!(validate(with(users(1), vec![window(); MAX_RESTRICTIONS + 1])).is_err());
    }

    #[test]
    fn override_times_are_bounded() {
        let now = utc("2026-10-25T12:00:00Z");
        let longest = TimeDelta::days(MAX_OVERRIDE_DAYS);
        let second = TimeDelta::seconds(1);
        let hour = TimeDelta::hours(1);
        for (starts_at, ends_at, ok) in [
            (now, now + longest, true),
            (now, now + longest + second, false),
            (now - hour, now, false),
            (now - hour, now + second, true),
            (now, now, false),
            (LATEST_TIME - hour, LATEST_TIME, true),
            (LATEST_TIME - hour, LATEST_TIME + second, false),
        ] {
            let result = check_override(starts_at, ends_at, now);
            assert_eq!(result.is_ok(), ok, "{starts_at} .. {ends_at}");
        }
    }

    #[test]
    fn shift_ranges_default_to_two_weeks_and_are_bounded() {
        let now = utc("2026-10-25T12:00:00Z");
        let days = TimeDelta::days;
        let second = TimeDelta::seconds(1);
        assert_eq!(shift_range(None, None, now), Ok((now, now + days(DEFAULT_SHIFT_DAYS))));
        assert_eq!(
            shift_range(Some(LATEST_TIME - days(1)), None, now),
            Ok((LATEST_TIME - days(1), LATEST_TIME)),
            "the default end stops at the latest time"
        );
        for (from, to, ok) in [
            (now, now + days(MAX_SHIFT_DAYS), true),
            (now, now + days(MAX_SHIFT_DAYS) + second, false),
            (now, now, false),
            (now, now - second, false),
            (EARLIEST_TIME, EARLIEST_TIME + days(1), true),
            (EARLIEST_TIME - second, EARLIEST_TIME + days(1), false),
            (LATEST_TIME - days(1), LATEST_TIME, true),
            (LATEST_TIME - days(1), LATEST_TIME + second, false),
        ] {
            assert_eq!(shift_range(Some(from), Some(to), now).is_ok(), ok, "{from} .. {to}");
        }
        assert!(shift_range(Some(DateTime::<Utc>::MAX_UTC), None, now).is_err());
    }

    fn tw(weekdays: &[u8], from: &str, to: &str) -> TimeWindow {
        TimeWindow { weekdays: weekdays.to_vec(), from: from.into(), to: to.into() }
    }

    fn stand_in(id: u128, user: u128, starts_at: DateTime<Utc>, hours: i64) -> Override {
        Override {
            id: Uuid::from_u128(id),
            user_id: Uuid::from_u128(user),
            starts_at,
            ends_at: starts_at + TimeDelta::hours(hours),
            created_at: starts_at - TimeDelta::days(1) + TimeDelta::seconds(id as i64),
        }
    }

    /// `current_shift` against its definition, the first merged shift of the
    /// minute-by-minute `rotation::shifts`, for `now` stepping through the
    /// days around DST changes. The restrictions end inside a skipped or
    /// repeated hour, span midnight or start at it, and two overrides hand
    /// over to the same user.
    #[test]
    fn current_shift_agrees_with_the_minute_by_minute_shifts() {
        let all = [1, 2, 3, 4, 5, 6, 7];
        let berlin = ["2026-03-29T01:00:00Z", "2026-10-25T01:00:00Z"];
        let santiago = ["2026-04-05T03:00:00Z", "2026-09-06T04:00:00Z"];
        let ending_in_the_dst_hour = vec![
            tw(&all, "01:00", "02:30"),
            tw(&[1, 3, 5, 7], "22:00", "06:00"),
            tw(&[7], "02:15", "02:45"),
        ];
        let around_midnight = vec![tw(&[6, 7], "23:30", "00:30"), tw(&[1, 2, 3, 4, 5], "00:00", "12:00")];
        let cases = [
            ("Europe/Berlin", RotationLength::Daily, "02:30", 3, berlin, ending_in_the_dst_hour),
            ("Europe/Berlin", RotationLength::Daily, "09:00", 1, berlin, vec![tw(&all, "01:00", "02:30")]),
            ("Europe/Berlin", RotationLength::Hours(5), "20:00", 2, berlin, vec![]),
            ("Europe/Berlin", RotationLength::Weekly, "09:00", 1, berlin, vec![tw(&all, "00:00", "00:00")]),
            ("America/Santiago", RotationLength::Daily, "00:00", 2, santiago, around_midnight),
        ];
        let horizon = TimeDelta::hours(30);
        let mut checked = 0;
        for (zone, length, handoff, n, transitions, windows) in cases {
            let tz: Tz = zone.parse().unwrap();
            let r = Rotation {
                timezone: tz,
                start_date: date("2026-01-05"),
                handoff: parse_hhmm("handoff", handoff).unwrap(),
                length,
                participants: (0..n).map(|i| Uuid::from_u128(i + 1)).collect(),
                restrictions: windows.iter().map(|w| w.validate(tz).unwrap()).collect(),
            };
            let edges = edge_times(&windows);
            for transition in transitions.map(utc) {
                // Clear of the transition, so they never mask the restrictions there.
                let overrides = [
                    stand_in(10, 9, transition - TimeDelta::hours(22), 2),
                    stand_in(11, 9, transition - TimeDelta::hours(20), 2),
                    stand_in(12, 1, transition + TimeDelta::hours(5), 1),
                ];
                let mut now = transition - TimeDelta::hours(26) + TimeDelta::seconds(17);
                while now < transition + TimeDelta::hours(3) {
                    let expected = rotation::shifts(&r, &overrides, now, now + horizon)
                        .into_iter()
                        .next()
                        .filter(|s| s.starts_at == now)
                        .map(|s| (s.user_id, s.override_id, s.ends_at));
                    let actual = current_shift(&r, &overrides, &edges, now, now + horizon);
                    assert_eq!(actual, expected, "{zone} {handoff} at {now}");
                    checked += 1;
                    now += TimeDelta::minutes(23);
                }
            }
        }
        assert!(checked > 700, "{checked}");
    }
}
