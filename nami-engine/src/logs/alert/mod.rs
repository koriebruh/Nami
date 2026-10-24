//! Log alerts: a count of a project's matching log rows over a window,
//! compared with a threshold (R-P2-18). The worker ([`run_forever`]) claims
//! due alerts like the uptime worker claims monitors, counts in ClickHouse,
//! and moves each alert between `ok` and `alerting`, opening and resolving
//! its incident through the seam below (R-P2-27) and notifying through
//! `uptime::alerts::notify_transition`.

pub mod model;
pub mod routes;

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::{PgConnection, PgPool};
use tokio::{sync::Semaphore, task::JoinSet, time::MissedTickBehavior};
use uuid::Uuid;

use crate::{
    core::clickhouse::ClickHouse,
    core::error::AppError,
    core::state::AppState,
    incidents::model as incident,
    logs::query::{
        self, LogFilters, Sql, Window,
        search::{READ_SETTINGS, user_error},
    },
    logs::view::saved_query::LogQuery,
    logs::alert::model::{AlertState, Comparison},
    uptime::alerts::{Subject, Transition, TransitionKind},
    uptime::maintenance::model as maintenance,
    uptime::worker::{Notifications, spawn_alerts},
};

/// How often a pass starts.
pub const ALERT_TICK: Duration = Duration::from_secs(5);
/// Alerts claimed per pass, at most.
pub const ALERT_CLAIM_BATCH: i64 = 50;
/// Evaluations running at once, across every pass of one instance. A pass
/// claims only as many alerts as there are free places, so an alert is never
/// pushed one interval ahead without the capacity to evaluate it.
pub const MAX_ALERT_EVALUATIONS: usize = 8;

/// What the worker shows when a count fails for any reason a user cannot fix.
const STORE_UNAVAILABLE: &str = "the log store is unavailable";
const QUERY_NO_LONGER_VALID: &str = "the alert's query is no longer valid";

/// An alert as claimed for one evaluation.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DueAlert {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub query: sqlx::types::Json<LogQuery>,
    pub threshold: i64,
    #[sqlx(try_from = "String")]
    pub comparison: Comparison,
    pub window_seconds: i32,
    /// As claimed: an evaluation is dropped when the row's differs (edited
    /// meanwhile, R-P2-18).
    pub updated_at: DateTime<Utc>,
}

/// `gt`: `count > threshold`; `lt`: `count < threshold`.
pub fn condition_met(comparison: Comparison, count: u64, threshold: i64) -> bool {
    let (count, threshold) = (i128::from(count), i128::from(threshold));
    match comparison {
        Comparison::Gt => count > threshold,
        Comparison::Lt => count < threshold,
    }
}

/// The incident cause: `12 matching events in the last 5 min (threshold: > 10)`.
/// The window reads in hours or minutes when it divides evenly, else seconds.
pub fn describe(alert: &DueAlert, count: u64) -> String {
    let seconds = alert.window_seconds;
    let window = if seconds % 3600 == 0 {
        format!("{} h", seconds / 3600)
    } else if seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{seconds} s")
    };
    let op = match alert.comparison {
        Comparison::Gt => ">",
        Comparison::Lt => "<",
    };
    format!(
        "{count} matching events in the last {window} (threshold: {op} {})",
        alert.threshold
    )
}

/// Atomically claims up to `limit` due, enabled alerts and reschedules them
/// `interval_seconds` ahead in one statement (`for update skip locked`), so
/// two passes or two engine instances never claim the same alert.
pub async fn claim_due_alerts(pool: &PgPool, limit: i64) -> Result<Vec<DueAlert>, sqlx::Error> {
    sqlx::query_as(
        "update log_alerts a
         set next_evaluation_at = now() + make_interval(secs => a.interval_seconds)
         where a.id in (
             select id from log_alerts
             where enabled and next_evaluation_at <= now()
             order by next_evaluation_at
             limit $1
             for update skip locked
         )
         returning a.id, a.project_id, a.name, a.query, a.threshold, a.comparison,
                   a.window_seconds, a.updated_at",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Deserialize)]
struct CountRow {
    count: u64,
}

/// How many of the project's rows match `filters` in `window`, under
/// [`READ_SETTINGS`] and the client's time limit. Input errors ClickHouse
/// reports become `Validation` ([`user_error`]).
pub async fn count_matches(
    ch: &ClickHouse,
    project_id: Uuid,
    filters: &LogFilters,
    window: &Window,
) -> Result<u64, AppError> {
    let mut sql = Sql::new();
    sql.push("SELECT count() AS count FROM logs");
    query::push_where(&mut sql, project_id, filters);
    query::push_window(&mut sql, window);
    let rows: Vec<CountRow> = ch
        .query_rows_with(sql.text(), &sql.params(), &READ_SETTINGS)
        .await
        .map_err(user_error)?;
    Ok(rows.first().map_or(0, |row| row.count))
}

/// Applies one count to its alert in a transaction, under the row lock.
/// Nothing changes for an alert deleted, disabled or edited since it was
/// claimed, or for a stale evaluation (started no later than the last one
/// applied). Inside a project-wide maintenance window the count is recorded
/// but the state stands still. Otherwise a met condition while `ok` opens
/// the incident (`Down`), a cleared one while `alerting` resolves it
/// (`Recovered`). `updated_at` is never touched here: it is the edit marker.
pub async fn apply_evaluation(
    pool: &PgPool,
    alert: &DueAlert,
    count: u64,
    started_at: DateTime<Utc>,
) -> Result<Option<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, bool, Option<DateTime<Utc>>, DateTime<Utc>)> = sqlx::query_as(
        "select state, enabled, last_evaluated_at, updated_at from log_alerts where id = $1 for update",
    )
    .bind(alert.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((state, enabled, last, updated_at)) = row else {
        return Ok(None);
    };
    let state = AlertState::parse(&state).map_err(|e| sqlx::Error::Decode(e.into()))?;
    if !enabled || updated_at != alert.updated_at || last.is_some_and(|last| last >= started_at) {
        return Ok(None);
    }
    let held = maintenance::active_windows(&mut *tx, alert.project_id, started_at)
        .await?
        .covers_project();
    let met = condition_met(alert.comparison, count, alert.threshold);
    let next_state = state.next(met, held);
    sqlx::query(
        "update log_alerts set state = $2, last_count = $3, last_evaluated_at = $4,
                last_error = null, last_error_at = null
         where id = $1",
    )
    .bind(alert.id)
    .bind(next_state.as_str())
    .bind(i64::try_from(count).unwrap_or(i64::MAX))
    .bind(started_at)
    .execute(&mut *tx)
    .await?;
    let transition = match (state, next_state) {
        (AlertState::Ok, AlertState::Alerting) => {
            open_alert_incident(&mut tx, alert, count, started_at).await?
        }
        (AlertState::Alerting, AlertState::Ok) => {
            resolve_alert_incident(
                &mut tx,
                alert.id,
                alert.project_id,
                &alert.name,
                started_at,
                true,
            )
            .await?
        }
        _ => None,
    };
    tx.commit().await?;
    Ok(transition)
}

/// A failed count: only `last_error` / `last_error_at` change (R-P2-18).
/// `message` is a fixed, user-safe text, never ClickHouse's own.
pub async fn record_failure(pool: &PgPool, alert_id: Uuid, message: &str) -> Result<(), sqlx::Error> {
    sqlx::query("update log_alerts set last_error = $2, last_error_at = now() where id = $1")
        .bind(alert_id)
        .bind(message)
        .execute(pool)
        .await?;
    Ok(())
}

/// The incident seam (R-P2-27), with [`resolve_alert_incident`] the only
/// code that opens or resolves a log-alert incident. Opens the alert's
/// incident and returns its `Down` transition, or `None` when one was
/// already open (it already alerted).
pub async fn open_alert_incident(
    conn: &mut PgConnection,
    alert: &DueAlert,
    count: u64,
    at: DateTime<Utc>,
) -> Result<Option<Transition>, sqlx::Error> {
    let cause = describe(alert, count);
    let opened = incident::open_for_log_alert(
        conn,
        alert.project_id,
        alert.id,
        &format!("Log alert: {}", alert.name),
        Some(&cause),
        at,
    )
    .await?;
    Ok(opened.map(|_| Transition {
        kind: TransitionKind::Down,
        subject: Subject::LogAlert(alert.id),
        project_id: alert.project_id,
        name: alert.name.clone(),
        cause: Some(cause),
        details: None,
        at,
    }))
}

/// Resolves the alert's open incident (if any). Returns the `Recovered`
/// transition only when `notify` (disabling an alert resolves quietly).
pub async fn resolve_alert_incident(
    conn: &mut PgConnection,
    alert_id: Uuid,
    project_id: Uuid,
    name: &str,
    at: DateTime<Utc>,
    notify: bool,
) -> Result<Option<Transition>, sqlx::Error> {
    incident::resolve_for_log_alert(conn, alert_id, at).await?;
    Ok(notify.then(|| Transition {
        kind: TransitionKind::Recovered,
        subject: Subject::LogAlert(alert_id),
        project_id,
        name: name.to_string(),
        cause: None,
        details: None,
        at,
    }))
}

/// Records a failed evaluation; a failure to record it is only logged.
async fn note_failure(pool: &PgPool, alert_id: Uuid, message: &str) {
    if let Err(e) = record_failure(pool, alert_id, message).await {
        tracing::error!(alert_id = %alert_id, error = %e, "failed to record a log alert failure");
    }
}

/// Counts and applies one claimed alert. A failure is recorded on the alert
/// and changes nothing else.
async fn evaluate(state: &AppState, alert: &DueAlert) -> Option<Transition> {
    let started_at = Utc::now();
    let Ok(filters) = alert.query.to_filters() else {
        note_failure(&state.db, alert.id, QUERY_NO_LONGER_VALID).await;
        return None;
    };
    let window = Window {
        from: started_at - chrono::Duration::seconds(alert.window_seconds.into()),
        to: started_at,
    };
    let count = match count_matches(&state.clickhouse, alert.project_id, &filters, &window).await {
        Ok(count) => count,
        Err(e) => {
            tracing::warn!(alert_id = %alert.id, error = ?e, "log alert count failed");
            let message = match &e {
                AppError::Validation(message) => message.as_str(),
                _ => STORE_UNAVAILABLE,
            };
            note_failure(&state.db, alert.id, message).await;
            return None;
        }
    };
    match apply_evaluation(&state.db, alert, count, started_at).await {
        Ok(transition) => transition,
        Err(e) => {
            tracing::error!(alert_id = %alert.id, error = %e, "failed to apply a log alert evaluation");
            None
        }
    }
}

/// One pass with its own budget of [`MAX_ALERT_EVALUATIONS`] (tests);
/// [`run_forever`] uses [`run_pass_with`] and one budget shared by every
/// pass. Returns how many alerts were claimed.
pub async fn run_pass(state: &AppState, notifications: &Notifications) -> Result<usize, AppError> {
    run_pass_with(
        state,
        notifications,
        &Arc::new(Semaphore::new(MAX_ALERT_EVALUATIONS)),
    )
    .await
}

/// Claims as many due alerts as `budget` has free places (at most
/// [`ALERT_CLAIM_BATCH`], none when no place is free), evaluates them at
/// once, each holding one place, and hands their transitions to
/// `notifications` once every evaluation has ended. Returns how many alerts
/// were claimed.
pub async fn run_pass_with(
    state: &AppState,
    notifications: &Notifications,
    budget: &Arc<Semaphore>,
) -> Result<usize, AppError> {
    let room = budget
        .available_permits()
        .min(ALERT_CLAIM_BATCH as usize);
    if room == 0 {
        return Ok(0);
    }
    // Reserved before claiming, so overlapping passes never claim more than
    // there are places for; if another pass took some meanwhile, this one
    // claims nothing and the next pass will.
    let Ok(mut reserved) = Arc::clone(budget).try_acquire_many_owned(room as u32) else {
        return Ok(0);
    };
    let alerts = claim_due_alerts(&state.db, room as i64).await?;
    let claimed = alerts.len();
    let mut evaluations = JoinSet::new();
    for alert in alerts {
        let place = reserved
            .split(1)
            .expect("a reserved place for every claimed alert");
        let state = state.clone();
        evaluations.spawn(async move {
            let _place = place;
            evaluate(&state, &alert).await
        });
    }
    // The places nobody claimed go back now.
    drop(reserved);
    let mut transitions = Vec::new();
    while let Some(ended) = evaluations.join_next().await {
        match ended {
            Ok(Some(transition)) => transitions.push(transition),
            Ok(None) => {}
            Err(e) => tracing::error!(error = %e, "log alert evaluation panicked"),
        }
    }
    spawn_alerts(state, notifications, transitions);
    Ok(claimed)
}

/// [`run_pass`], then waits for its notifications (tests). Returns how many
/// alerts were claimed.
pub async fn evaluate_due_alerts(state: &AppState) -> Result<usize, AppError> {
    let notifications = Notifications::new();
    let claimed = run_pass(state, &notifications).await?;
    notifications.drain().await;
    Ok(claimed)
}

/// Starts a pass every [`ALERT_TICK`], never waiting for the passes before
/// it; they share one budget of [`MAX_ALERT_EVALUATIONS`] and one
/// [`Notifications`]. Each pass runs in a task of its own, so a panic is
/// logged instead of stopping the worker.
pub async fn run_forever(state: AppState) {
    let budget = Arc::new(Semaphore::new(MAX_ALERT_EVALUATIONS));
    let notifications = Notifications::new();
    let mut passes = JoinSet::new();
    let mut ticker = tokio::time::interval(ALERT_TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        notifications.reap();
        while let Some(ended) = passes.try_join_next() {
            if let Err(e) = ended {
                tracing::error!(error = %e, "log alert pass panicked");
            }
        }
        let (state, notifications, budget) =
            (state.clone(), notifications.clone(), Arc::clone(&budget));
        passes.spawn(async move {
            if let Err(e) = run_pass_with(&state, &notifications, &budget).await {
                tracing::error!(error = ?e, "log alert pass failed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conditions_compare_strictly() {
        assert!(condition_met(Comparison::Gt, 11, 10));
        assert!(!condition_met(Comparison::Gt, 10, 10));
        assert!(condition_met(Comparison::Lt, 0, 1));
        assert!(!condition_met(Comparison::Lt, 1, 1));
        assert!(Comparison::parse("eq").is_err(), "unknown comparisons are refused");
    }

    #[test]
    fn the_cause_names_count_window_and_threshold() {
        let alert = DueAlert {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            name: "n".into(),
            query: sqlx::types::Json(LogQuery::default()),
            threshold: 10,
            comparison: Comparison::Gt,
            window_seconds: 300,
            updated_at: chrono::DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
        };
        assert_eq!(
            describe(&alert, 12),
            "12 matching events in the last 5 min (threshold: > 10)"
        );
        let hourly = DueAlert {
            comparison: Comparison::Lt,
            window_seconds: 3600,
            threshold: 1,
            ..alert.clone()
        };
        assert_eq!(
            describe(&hourly, 0),
            "0 matching events in the last 1 h (threshold: < 1)"
        );
        let odd = DueAlert {
            window_seconds: 90,
            ..alert
        };
        assert_eq!(
            describe(&odd, 1),
            "1 matching events in the last 90 s (threshold: > 10)"
        );
    }
}
