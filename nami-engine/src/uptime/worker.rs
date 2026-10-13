//! The background uptime worker: every tick it claims due monitors, checks
//! them, applies the state machine, hands the alerts to a bounded set of
//! background tasks, and finally stores the results. Then it sweeps overdue
//! heartbeats, whose alerts go through the same bounded set.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use super::{
    alerts::{self, Subject, Transition, TransitionKind},
    checker::{self, CheckOutcome, CheckStatus},
    heartbeat,
    store::{self, UptimeCheckRow},
};
use crate::{
    error::AppError,
    models::{incident, monitor::Monitor},
    state::AppState,
};

pub const TICK: Duration = Duration::from_secs(5);
/// Monitors claimed per tick.
pub const CLAIM_BATCH: i64 = 50;
/// Overdue heartbeats the sweep marks down per tick; the next tick takes the
/// rest.
pub const SWEEP_BATCH: i64 = 100;
/// Checks in flight at once.
pub const MAX_IN_FLIGHT: usize = 20;
/// Alert batches (one per pass that had transitions) allowed to be sending at
/// once; see [`Notifications`].
pub const MAX_NOTIFICATION_BATCHES: usize = 32;

/// What a check reports when the task running it panicked.
const INTERNAL_CHECKER_ERROR: &str = "internal checker error";

/// Atomically claims up to `limit` due, unpaused monitors and reschedules
/// them `interval_seconds` ahead, in ONE statement: `for update skip locked`
/// means two overlapping ticks (or two engine instances) can never claim
/// the same monitor, and nothing can change between choosing and
/// rescheduling.
pub async fn claim_due_monitors(pool: &PgPool, limit: i64) -> Result<Vec<Monitor>, sqlx::Error> {
    sqlx::query_as(
        "update monitors m
         set next_check_at = now() + make_interval(secs => m.interval_seconds)
         where m.id in (
             select id from monitors
             where not paused and next_check_at <= now()
             order by next_check_at
             limit $1
             for update skip locked
         )
         returning m.*",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The columns `apply_outcome` reads from the monitor row it has locked.
#[derive(sqlx::FromRow)]
struct LockedMonitor {
    project_id: Uuid,
    name: String,
    current_status: String,
    consecutive_failures: i32,
    confirmation_threshold: i32,
    paused: bool,
    last_checked_at: Option<DateTime<Utc>>,
}

/// Applies one check result to its monitor's state in a single transaction.
/// The row is locked (`for update`) and the state read from that locked row
/// rather than from `monitor` (a snapshot taken at claim time), so
/// concurrent results for one monitor are applied one after the other.
///
/// - success: failures reset; `down` -> `up` resolves the open incident and
///   returns a `Recovered` transition; `pending` -> `up` is silent.
/// - failure: failures + 1; reaching the confirmation threshold while not
///   already `down` (including from `pending`) sets `down`, opens an
///   incident and returns a `Down` transition.
///
/// A monitor deleted or paused while its check was in flight is left alone,
/// and so is a stale result: one whose check started no later than the last
/// check already applied (two checks of one monitor overlapped, which an edit
/// that reschedules it can cause, and they finished in the other order).
/// Timestamps come from the engine's clock, so engines sharing a database
/// are expected to keep their clocks in step.
///
/// The cause stored in the incident and sent in the alert is the check's
/// error with control characters replaced by spaces (see
/// [`alerts::sanitize_cause`]).
pub async fn apply_outcome(
    pool: &PgPool,
    monitor: &Monitor,
    outcome: &CheckOutcome,
) -> Result<Option<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<LockedMonitor> = sqlx::query_as(
        "select project_id, name, current_status, consecutive_failures, confirmation_threshold, paused,
                last_checked_at
         from monitors where id = $1 for update",
    )
    .bind(monitor.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(LockedMonitor {
        project_id,
        name,
        current_status: status,
        consecutive_failures: failures,
        confirmation_threshold: threshold,
        paused,
        last_checked_at,
    }) = row
    else {
        return Ok(None);
    };
    if paused {
        return Ok(None);
    }
    if last_checked_at.is_some_and(|last| last >= outcome.checked_at) {
        return Ok(None);
    }

    let transition = match outcome.status {
        CheckStatus::Up => {
            sqlx::query(
                "update monitors set current_status = 'up', consecutive_failures = 0, last_checked_at = $2
                 where id = $1",
            )
            .bind(monitor.id)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
            if status == "down" {
                incident::resolve_for_monitor(&mut tx, monitor.id, outcome.checked_at).await?;
                Some(Transition {
                    kind: TransitionKind::Recovered,
                    subject: Subject::Monitor(monitor.id),
                    project_id,
                    name,
                    cause: None,
                    at: outcome.checked_at,
                })
            } else {
                None
            }
        }
        CheckStatus::Down => {
            let failures = failures.saturating_add(1);
            let goes_down = failures >= threshold && status != "down";
            sqlx::query(
                "update monitors
                 set consecutive_failures = $2,
                     current_status = case when $3 then 'down' else current_status end,
                     last_checked_at = $4
                 where id = $1",
            )
            .bind(monitor.id)
            .bind(failures)
            .bind(goes_down)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
            if goes_down {
                let cause = outcome.error.as_deref().map(alerts::sanitize_cause);
                let opened = incident::open_for_monitor(
                    &mut tx,
                    project_id,
                    monitor.id,
                    &format!("{name} is down"),
                    cause.as_deref(),
                    outcome.checked_at,
                )
                .await?;
                // `None`: an incident was somehow already open — it already
                // alerted, so this must not alert again.
                opened.map(|_| Transition {
                    kind: TransitionKind::Down,
                    subject: Subject::Monitor(monitor.id),
                    project_id,
                    name,
                    cause,
                    at: outcome.checked_at,
                })
            } else {
                None
            }
        }
    };
    tx.commit().await?;
    Ok(transition)
}

/// The background tasks that send alerts, so that sending never holds up the
/// checks: a pass hands its transitions over and moves on, whatever the SMTP
/// server is doing. Cheap to clone; clones share one set. `run_forever` owns
/// the set, and the tasks stop when the last handle is dropped.
///
/// Bounded: at most `limit` batches send at once. A pass starts one batch for
/// all of its transitions (up to `CLAIM_BATCH` of them for a check pass, up to
/// `SWEEP_BATCH` for a heartbeat sweep), and a batch sends them one after the
/// other, so a burst of alerts after a shared dependency fails is never
/// refused, and never opens a stampede of SMTP connections.
/// Only when `limit` batches are already stuck (an SMTP server that has not
/// answered for a long while) is a new one refused, never waited for.
#[derive(Clone)]
pub struct Notifications {
    tasks: Arc<Mutex<JoinSet<()>>>,
    limit: usize,
}

impl Default for Notifications {
    fn default() -> Self {
        Self::with_limit(MAX_NOTIFICATION_BATCHES)
    }
}

impl Notifications {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limit(limit: usize) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(JoinSet::new())),
            limit,
        }
    }

    fn tasks(&self) -> MutexGuard<'_, JoinSet<()>> {
        // Nothing holds the lock across an await or runs user code under it,
        // so a poisoned lock still guards a consistent set.
        self.tasks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts `work` as a background task, unless `limit` tasks are already
    /// running. Returns false, without starting or waiting for anything, when
    /// it is refused.
    pub fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) -> bool {
        let mut tasks = self.tasks();
        log_finished(&mut tasks);
        if tasks.len() >= self.limit {
            return false;
        }
        tasks.spawn(work);
        true
    }

    /// Collects the tasks that have finished (logging any that panicked) and
    /// returns how many are still running.
    pub fn reap(&self) -> usize {
        let mut tasks = self.tasks();
        log_finished(&mut tasks);
        tasks.len()
    }

    /// Waits until every task running right now has finished.
    pub async fn drain(&self) {
        let mut running = std::mem::take(&mut *self.tasks());
        while let Some(joined) = running.join_next().await {
            if let Err(e) = joined {
                tracing::error!(error = %e, "alert task failed");
            }
        }
    }
}

fn log_finished(tasks: &mut JoinSet<()>) {
    while let Some(joined) = tasks.try_join_next() {
        if let Err(e) = joined {
            tracing::error!(error = %e, "alert task failed");
        }
    }
}

/// Runs `check` for every monitor, at most `MAX_IN_FLIGHT` at a time, and
/// returns one outcome per monitor. A check whose task panics does not vanish:
/// its monitor gets a `Down` outcome with a generic error, so it still gets a
/// result (and the panic is logged with the monitor's id).
async fn check_all<F, Fut>(monitors: Vec<Monitor>, check: F) -> Vec<(Monitor, CheckOutcome)>
where
    F: Fn(Monitor) -> Fut,
    Fut: Future<Output = CheckOutcome> + Send + 'static,
{
    let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut checks = JoinSet::new();
    let mut spawned = HashMap::new();
    for monitor in monitors {
        let work = check(monitor.clone());
        let semaphore = Arc::clone(&semaphore);
        let task = checks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("the check semaphore is never closed");
            work.await
        });
        spawned.insert(task.id(), monitor);
    }
    let mut results = Vec::with_capacity(spawned.len());
    while let Some(joined) = checks.join_next_with_id().await {
        let (id, outcome) = match joined {
            Ok((id, outcome)) => (id, Ok(outcome)),
            Err(e) => (e.id(), Err(e)),
        };
        let Some(monitor) = spawned.remove(&id) else {
            continue;
        };
        let outcome = outcome.unwrap_or_else(|e| {
            tracing::error!(error = %e, monitor_id = %monitor.id, "uptime check task failed");
            CheckOutcome {
                status: CheckStatus::Down,
                status_code: None,
                response_time_ms: None,
                error: Some(INTERNAL_CHECKER_ERROR.to_string()),
                checked_at: Utc::now(),
            }
        });
        results.push((monitor, outcome));
    }
    results
}

/// Hands `transitions` to `notifications` as one batch, which a background
/// task sends one alert after the other. Never waits: when
/// `MAX_NOTIFICATION_BATCHES` batches are still sending, the new batch is
/// refused and its alerts are logged as dropped.
fn spawn_alerts(state: &AppState, notifications: &Notifications, transitions: Vec<Transition>) {
    if transitions.is_empty() {
        return;
    }
    let affected: Vec<_> = transitions.iter().map(|t| (t.kind, t.subject)).collect();
    let alert_state = state.clone();
    let started = notifications.spawn(async move {
        for transition in &transitions {
            alerts::notify_transition(&alert_state, transition).await;
        }
    });
    if !started {
        tracing::error!(
            limit = MAX_NOTIFICATION_BATCHES,
            alerts = ?affected,
            "alerts dropped: too many alert batches are still sending (is the SMTP server hung?)"
        );
    }
}

/// One worker pass over due monitors. Returns how many checks ran.
///
/// In this order: claim and check the monitors; apply every result to its
/// monitor (state and incidents); hand the alerts to `notifications`, which
/// sends them in the background; and only then store the results in
/// ClickHouse. History comes last because it is the one step that can be slow
/// without anyone being hurt: with ClickHouse unavailable or hung, state
/// changes and alerts still happen on time (a hung insert only delays the end
/// of the pass, by at most the client's own 30 s timeout).
pub async fn run_pass(state: &AppState, notifications: &Notifications) -> Result<usize, AppError> {
    let monitors = claim_due_monitors(&state.db, CLAIM_BATCH).await?;
    if monitors.is_empty() {
        return Ok(0);
    }

    // Built per pass: cheap next to the checks themselves, and it never
    // outlives the runtime that drives its connections.
    let http = checker::build_http_client();
    let results = check_all(monitors, move |monitor| {
        let http = http.clone();
        async move { checker::perform_check(&http, &monitor).await }
    })
    .await;

    let mut transitions = Vec::new();
    for (monitor, outcome) in &results {
        match apply_outcome(&state.db, monitor, outcome).await {
            Ok(Some(transition)) => transitions.push(transition),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, monitor_id = %monitor.id, "failed to apply check outcome")
            }
        }
    }
    spawn_alerts(state, notifications, transitions);

    // ClickHouse holds history only: if it is unavailable, the state changes
    // and alerts above have already happened.
    let rows: Vec<UptimeCheckRow> = results
        .iter()
        .map(|(m, o)| UptimeCheckRow::from_outcome(m, o))
        .collect();
    if let Err(e) = store::insert_checks(&state.clickhouse, &rows).await {
        tracing::error!(error = %e, count = rows.len(), "failed to store uptime checks");
    }
    Ok(results.len())
}

/// [`run_pass`] that also waits for the alerts it started to be sent, so the
/// caller knows they are out when it returns. Returns how many checks ran.
pub async fn run_due_checks(state: &AppState) -> Result<usize, AppError> {
    let notifications = Notifications::new();
    let ran = run_pass(state, &notifications).await?;
    notifications.drain().await;
    Ok(ran)
}

/// One heartbeat sweep: marks up to [`SWEEP_BATCH`] overdue heartbeats down
/// (opening their incidents) and hands the alerts to `notifications`, which
/// sends them in the background like the alerts of [`run_pass`]. Returns how
/// many went down.
///
/// `not_before` is the earliest moment a heartbeat may be judged from (see
/// [`heartbeat::sweep_overdue`]): the worker passes the moment it started.
pub async fn sweep_pass(
    state: &AppState,
    notifications: &Notifications,
    not_before: DateTime<Utc>,
) -> Result<usize, AppError> {
    let transitions = heartbeat::sweep_overdue(&state.db, not_before, SWEEP_BATCH).await?;
    let swept = transitions.len();
    spawn_alerts(state, notifications, transitions);
    Ok(swept)
}

/// [`sweep_pass`] with no restart bound that also waits for the alerts it
/// started to be sent, so the caller knows they are out when it returns.
/// Returns how many heartbeats went down.
pub async fn sweep_heartbeats(state: &AppState) -> Result<usize, AppError> {
    let notifications = Notifications::new();
    let swept = sweep_pass(state, &notifications, DateTime::UNIX_EPOCH).await?;
    notifications.drain().await;
    Ok(swept)
}

/// Ticks every [`TICK`] forever, counting itself started now: no heartbeat is
/// judged from a moment before that (see [`run_forever_since`]).
pub async fn run_forever(state: AppState) {
    run_forever_since(state, Utc::now()).await;
}

/// [`run_forever`] with the moment the worker counts as started, below which
/// the heartbeat sweep never judges a heartbeat (tests give an old one).
///
/// Each tick sweeps the heartbeats first and then runs the check pass, so a
/// slow pass (checks may take a minute) never delays DOWN detection. Each half
/// runs in a task of its own, so even a panic inside one is logged here instead
/// of stopping the worker or skipping the other half. Alerts are sent by tasks
/// that outlive the tick, so a slow SMTP server never delays the next one.
pub async fn run_forever_since(state: AppState, started_at: DateTime<Utc>) {
    let notifications = Notifications::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        notifications.reap();
        let (sweep_state, sweep_notifications) = (state.clone(), notifications.clone());
        let (check_state, check_notifications) = (state.clone(), notifications.clone());
        run_in_turn(
            async move { sweep_tick(&sweep_state, &sweep_notifications, started_at).await },
            async move { check_tick(&check_state, &check_notifications).await },
        )
        .await;
    }
}

/// Runs `first` to its end, then `second`, each in a task of its own: a panic
/// in either is logged here, and neither stops the other from running.
async fn run_in_turn(
    first: impl Future<Output = ()> + Send + 'static,
    second: impl Future<Output = ()> + Send + 'static,
) {
    if let Err(e) = tokio::spawn(first).await {
        tracing::error!(error = %e, "uptime worker task panicked");
    }
    if let Err(e) = tokio::spawn(second).await {
        tracing::error!(error = %e, "uptime worker task panicked");
    }
}

async fn sweep_tick(state: &AppState, notifications: &Notifications, started_at: DateTime<Utc>) {
    if let Err(e) = sweep_pass(state, notifications, started_at).await {
        tracing::error!(error = ?e, "heartbeat sweep failed");
    }
}

async fn check_tick(state: &AppState, notifications: &Notifications) {
    if let Err(e) = run_pass(state, notifications).await {
        tracing::error!(error = ?e, "uptime check pass failed");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn monitor(name: &str) -> Monitor {
        Monitor {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            name: name.to_string(),
            check_type: "http".to_string(),
            url: Some("http://example.test/".to_string()),
            host: None,
            port: None,
            http_method: "GET".to_string(),
            keyword: None,
            keyword_mode: "contains".to_string(),
            expected_status_min: 200,
            expected_status_max: 399,
            interval_seconds: 60,
            timeout_seconds: 10,
            confirmation_threshold: 2,
            paused: false,
            current_status: "pending".to_string(),
            consecutive_failures: 0,
            last_checked_at: None,
            next_check_at: Utc::now(),
            created_at: Utc::now(),
        }
    }

    fn up() -> CheckOutcome {
        CheckOutcome {
            status: CheckStatus::Up,
            status_code: Some(200),
            response_time_ms: Some(1),
            error: None,
            checked_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn a_check_that_panics_becomes_a_down_outcome_for_its_monitor() {
        let (healthy, doomed) = (monitor("healthy"), monitor("doomed"));
        let (healthy_id, doomed_id) = (healthy.id, doomed.id);

        let results = check_all(vec![healthy, doomed], |monitor| async move {
            if monitor.name == "doomed" {
                panic!("a bug in a check");
            }
            up()
        })
        .await;

        assert_eq!(results.len(), 2, "every monitor still gets a result");
        let outcome_of = |id: Uuid| &results.iter().find(|(m, _)| m.id == id).unwrap().1;
        assert_eq!(outcome_of(healthy_id).status, CheckStatus::Up);
        let doomed_outcome = outcome_of(doomed_id);
        assert_eq!(doomed_outcome.status, CheckStatus::Down);
        assert_eq!(
            doomed_outcome.error.as_deref(),
            Some("internal checker error")
        );
        assert_eq!(doomed_outcome.status_code, None);
    }

    #[tokio::test]
    async fn checks_never_run_more_than_max_in_flight_at_a_time() {
        let monitors: Vec<Monitor> = (0..MAX_IN_FLIGHT * 2 + 5)
            .map(|i| monitor(&format!("m{i}")))
            .collect();
        let total = monitors.len();
        let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));

        let results = check_all(monitors, {
            let (running, peak) = (Arc::clone(&running), Arc::clone(&peak));
            move |_| {
                let (running, peak) = (Arc::clone(&running), Arc::clone(&peak));
                async move {
                    peak.fetch_max(running.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    running.fetch_sub(1, Ordering::SeqCst);
                    up()
                }
            }
        })
        .await;

        assert_eq!(results.len(), total);
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak <= MAX_IN_FLIGHT, "{peak} checks ran at once");
    }

    /// A task that runs until its sender is dropped or used.
    fn blocked() -> (
        tokio::sync::oneshot::Sender<()>,
        impl Future<Output = ()> + Send,
    ) {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        (release, async move {
            let _ = wait.await;
        })
    }

    #[tokio::test]
    async fn a_full_notification_set_refuses_new_work_instead_of_waiting() {
        let notifications = Notifications::with_limit(2);
        let ((first, first_work), (second, second_work)) = (blocked(), blocked());
        assert!(notifications.spawn(first_work));
        assert!(notifications.spawn(second_work));

        let (_third, third_work) = blocked();
        assert!(!notifications.spawn(third_work), "the set is full");
        assert_eq!(notifications.reap(), 2);

        first.send(()).unwrap();
        second.send(()).unwrap();
        notifications.drain().await;
        assert_eq!(notifications.reap(), 0);
        let (_fourth, fourth_work) = blocked();
        assert!(
            notifications.spawn(fourth_work),
            "finished work frees its place"
        );
    }

    #[tokio::test]
    async fn drain_waits_for_the_work_that_is_running() {
        let notifications = Notifications::new();
        let finished = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let finished = Arc::clone(&finished);
            assert!(notifications.spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                finished.fetch_add(1, Ordering::SeqCst);
            }));
        }

        notifications.drain().await;

        assert_eq!(finished.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn the_two_halves_of_a_tick_run_in_the_order_given() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let first = {
            let order = Arc::clone(&order);
            async move {
                // Slow, so that a second half started alongside it would win.
                tokio::time::sleep(Duration::from_millis(50)).await;
                order.lock().unwrap().push("first");
            }
        };
        let second = {
            let order = Arc::clone(&order);
            async move { order.lock().unwrap().push("second") }
        };

        run_in_turn(first, second).await;

        assert_eq!(*order.lock().unwrap(), vec!["first", "second"]);
    }

    #[tokio::test]
    async fn a_panic_in_one_half_of_a_tick_does_not_skip_the_other() {
        let ran = Arc::new(AtomicUsize::new(0));
        let count = |ran: &Arc<AtomicUsize>| {
            let ran = Arc::clone(ran);
            async move {
                ran.fetch_add(1, Ordering::SeqCst);
            }
        };

        run_in_turn(async { panic!("a bug in the first half") }, count(&ran)).await;
        assert_eq!(ran.load(Ordering::SeqCst), 1, "the second half still ran");

        run_in_turn(count(&ran), async { panic!("a bug in the second half") }).await;
        assert_eq!(
            ran.load(Ordering::SeqCst),
            2,
            "the first half ran, and the tick ended"
        );
    }

    #[tokio::test]
    async fn a_panicking_alert_task_is_reaped_without_harm() {
        let notifications = Notifications::with_limit(1);
        assert!(notifications.spawn(async { panic!("a bug while sending") }));
        notifications.drain().await;
        assert_eq!(notifications.reap(), 0);
        assert!(
            notifications.spawn(async {}),
            "the panic did not use up the place"
        );
    }
}
