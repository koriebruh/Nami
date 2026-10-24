//! The background uptime worker: two loops that never wait for each other,
//! and alerts sent off both of their paths.
//!
//! - Checks ([`run_forever`]'s check loop): every [`TICK`] a pass claims the
//!   monitors that are due and starts their checks, without waiting for the
//!   passes before it to end. At most [`MAX_IN_FLIGHT`] checks run at once
//!   across all passes, and at most [`MAX_PENDING_CHECKS`] are claimed and not
//!   yet finished, so an overload leaves monitors due instead of piling up
//!   checks. Each outcome is applied to its monitor (state and incident, in a
//!   transaction of its own) the moment its check ends, and its alert handed
//!   over at once: a slow target delays nothing but its own monitor. A pass
//!   stores the check history in ClickHouse once its last check has ended, in
//!   the pass's own task, so a slow or hung ClickHouse delays nothing either.
//! - Heartbeats (the sweep loop): every [`SWEEP_INTERVAL`] the worker records
//!   that it is alive, making up for an outage before (see
//!   [`heartbeat::note_worker_alive`]), and sweeps the overdue heartbeats.
//!   Nothing a check does can delay it.
//! - Alerts are sent by background tasks ([`Notifications`]), so a slow SMTP
//!   server delays neither loop.

use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
    task::JoinSet,
    time::MissedTickBehavior,
};
use uuid::Uuid;

use super::{
    alerts::{self, Subject, Transition, TransitionKind},
    checker::{self, CheckContext, CheckOutcome, CheckStatus},
    heartbeat,
    store::{self, UptimeCheckRow},
};
use crate::{
    core::error::AppError, core::state::AppState, incidents::model as incident,
    uptime::maintenance::model as maintenance, uptime::monitor::model::Monitor,
};

/// How often a check pass starts.
pub const TICK: Duration = Duration::from_secs(5);
/// How often the worker records its liveness and sweeps the heartbeats.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5);
/// Monitors claimed per pass, at most.
pub const CLAIM_BATCH: i64 = 50;
/// Overdue heartbeats the sweep marks down per run; the next run takes the
/// rest.
pub const SWEEP_BATCH: i64 = 100;
/// Checks running at once, across all passes.
pub const MAX_IN_FLIGHT: usize = 20;
/// Checks claimed and not yet finished (running, or waiting for one of the
/// [`MAX_IN_FLIGHT`] places), across all passes. A pass claims only as many
/// monitors as there is room for here; the others stay due, and the next
/// passes take them, the most overdue first.
pub const MAX_PENDING_CHECKS: usize = 2 * CLAIM_BATCH as usize;
/// Alert batches (one per pass or sweep that had transitions) allowed to be
/// sending at once; see [`Notifications`].
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
    consecutive_successes: i32,
    recovery_threshold: i32,
    paused: bool,
    last_checked_at: Option<DateTime<Utc>>,
}

/// Applies one check result to its monitor's state in a single transaction.
/// The row is locked (`for update`) and the state read from that locked row
/// rather than from `monitor` (a snapshot taken at claim time), so
/// concurrent results for one monitor are applied one after the other.
///
/// - success: failures reset. While `down`, successes count up, and reaching
///   the monitor's `recovery_threshold` sets `up`, resolves the open incident
///   and returns a `Recovered` transition; until then the monitor stays
///   `down` and nothing is sent. From `pending` or `up`, a success sets `up`
///   silently and counts nothing.
/// - failure: successes reset; failures + 1; reaching the confirmation
///   threshold while not already `down` (including from `pending`) sets
///   `down`, opens an incident and returns a `Down` transition.
///
/// A monitor deleted or paused while its check was in flight is left alone,
/// and so is a stale result: one whose check started no later than the last
/// check already applied (two checks of one monitor overlapped, which an edit
/// that reschedules it can cause, and they finished in the other order).
/// Timestamps come from the engine's clock, so engines sharing a database
/// are expected to keep their clocks in step.
///
/// Inside a maintenance window covering the monitor (project-wide or scoped
/// to it) only `last_checked_at` moves, so the stale-outcome guard keeps
/// working: no status or counter change (neither counter advances toward a
/// transition), no incident opened or resolved, no transition. An incident
/// already open stays open, and can resolve once the window is over. Coverage
/// is judged at the outcome's `checked_at` (when its check started, the same
/// instant the stale guard compares), not at the time it is applied.
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
        "select project_id, name, current_status, consecutive_failures, confirmation_threshold,
                consecutive_successes, recovery_threshold, paused, last_checked_at
         from monitors where id = $1 for update",
    )
    .bind(monitor.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(state) = row else {
        return Ok(None);
    };
    if state.paused {
        return Ok(None);
    }
    if state
        .last_checked_at
        .is_some_and(|last| last >= outcome.checked_at)
    {
        return Ok(None);
    }
    if maintenance::monitor_in_maintenance(
        &mut *tx,
        state.project_id,
        monitor.id,
        outcome.checked_at,
    )
    .await?
    {
        // The check itself is recorded (ClickHouse history, last_checked_at);
        // the state machine stands still until the window ends.
        sqlx::query("update monitors set last_checked_at = $2 where id = $1")
            .bind(monitor.id)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(None);
    }

    let transition = match outcome.status {
        CheckStatus::Up => {
            let was_down = state.current_status == "down";
            let successes = state.consecutive_successes.saturating_add(1);
            // Only a down monitor counts successes, and only the one that
            // reaches the threshold brings it back.
            let recovers = was_down && successes >= state.recovery_threshold;
            let still_down = was_down && !recovers;
            let (status, counted) = if still_down {
                ("down", successes)
            } else {
                ("up", 0)
            };
            sqlx::query(
                "update monitors
                 set current_status = $2,
                     consecutive_failures = 0,
                     consecutive_successes = $3,
                     last_checked_at = $4
                 where id = $1",
            )
            .bind(monitor.id)
            .bind(status)
            .bind(counted)
            .bind(outcome.checked_at)
            .execute(&mut *tx)
            .await?;
            if recovers {
                incident::resolve_for_monitor(&mut tx, monitor.id, outcome.checked_at).await?;
                Some(Transition {
                    kind: TransitionKind::Recovered,
                    subject: Subject::Monitor(monitor.id),
                    project_id: state.project_id,
                    name: state.name,
                    cause: None,
                    details: None,
                    at: outcome.checked_at,
                })
            } else {
                None
            }
        }
        CheckStatus::Down => {
            let failures = state.consecutive_failures.saturating_add(1);
            let goes_down =
                failures >= state.confirmation_threshold && state.current_status != "down";
            sqlx::query(
                "update monitors
                 set consecutive_failures = $2,
                     consecutive_successes = 0,
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
                    state.project_id,
                    monitor.id,
                    &format!("{} is down", state.name),
                    cause.as_deref(),
                    outcome.checked_at,
                )
                .await?;
                // `None`: an incident was somehow already open — it already
                // alerted, so this must not alert again.
                opened.map(|_| Transition {
                    kind: TransitionKind::Down,
                    subject: Subject::Monitor(monitor.id),
                    project_id: state.project_id,
                    name: state.name,
                    cause,
                    details: None,
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
/// checks or the sweep: a pass or a sweep hands its transitions over and moves
/// on, whatever the SMTP server is doing. Cheap to clone; clones share one
/// set. `run_forever` owns the set, and the tasks stop when the last handle is
/// dropped.
///
/// Bounded: at most `limit` batches send at once. A pass starts one batch for
/// all of its transitions (up to `CLAIM_BATCH` of them, handed over one by one
/// as its checks end, see [`PassAlerts`]), a sweep one for its transitions (up
/// to `SWEEP_BATCH`), and a batch sends them one after the other, so a burst of
/// alerts after a shared dependency fails is never refused, and never opens a
/// stampede of SMTP connections. Only when `limit` batches are already stuck
/// (an SMTP server that has not answered for a long while) is a new one
/// refused, never waited for.
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

/// Logs alerts that could not be handed to a sending task because
/// `MAX_NOTIFICATION_BATCHES` batches are still sending.
fn log_dropped(affected: &[(TransitionKind, Subject)]) {
    tracing::error!(
        limit = MAX_NOTIFICATION_BATCHES,
        alerts = ?affected,
        "alerts dropped: too many alert batches are still sending (is the SMTP server hung?)"
    );
}

/// Hands `transitions` to `notifications` as one batch, which a background
/// task sends one alert after the other. Never waits: when
/// `MAX_NOTIFICATION_BATCHES` batches are still sending, the new batch is
/// refused and its alerts are logged as dropped.
pub(crate) fn spawn_alerts(
    state: &AppState,
    notifications: &Notifications,
    transitions: Vec<Transition>,
) {
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
        log_dropped(&affected);
    }
}

/// The alerts of one pass, sent by one background task (one batch in
/// [`Notifications`]) that the pass starts with its first transition and feeds
/// the others to as their checks end. Each alert leaves as soon as its outcome
/// is applied, and they still go one after the other. The task sends what it
/// was given and ends once the pass drops this.
struct PassAlerts<'a> {
    state: &'a AppState,
    notifications: &'a Notifications,
    queue: Option<mpsc::UnboundedSender<Transition>>,
}

impl<'a> PassAlerts<'a> {
    fn new(state: &'a AppState, notifications: &'a Notifications) -> Self {
        Self {
            state,
            notifications,
            queue: None,
        }
    }

    /// Hands `transition` to the pass's sending task, starting it first if
    /// there is none yet (or it is gone because it panicked). Never waits: if
    /// `MAX_NOTIFICATION_BATCHES` batches are still sending, the alert is
    /// logged as dropped and the next transition tries again.
    fn send(&mut self, transition: Transition) {
        let transition = match &self.queue {
            Some(queue) => match queue.send(transition) {
                Ok(()) => return,
                Err(mpsc::error::SendError(transition)) => transition,
            },
            None => transition,
        };
        let (queue, mut queued) = mpsc::unbounded_channel::<Transition>();
        let alert_state = self.state.clone();
        let started = self.notifications.spawn(async move {
            while let Some(transition) = queued.recv().await {
                alerts::notify_transition(&alert_state, &transition).await;
            }
        });
        let affected = [(transition.kind, transition.subject)];
        // The task owns the receiving end from the start, so the first send
        // only fails if the task was refused (and dropped its receiver).
        if started && queue.send(transition).is_ok() {
            self.queue = Some(queue);
        } else {
            log_dropped(&affected);
        }
    }
}

/// What the passes of one worker share: the places for running checks
/// ([`MAX_IN_FLIGHT`]) and for checks claimed and not yet finished
/// ([`MAX_PENDING_CHECKS`]). Cheap to clone; clones share both.
#[derive(Clone)]
struct CheckLimits {
    in_flight: Arc<Semaphore>,
    pending: Arc<Semaphore>,
}

impl CheckLimits {
    fn new() -> Self {
        Self {
            in_flight: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            pending: Arc::new(Semaphore::new(MAX_PENDING_CHECKS)),
        }
    }

    /// Claims as many due monitors as there is room for (up to
    /// [`CLAIM_BATCH`]), each with its pending place, which its check holds
    /// until it has ended.
    async fn claim(
        &self,
        pool: &PgPool,
    ) -> Result<Vec<(Monitor, OwnedSemaphorePermit)>, sqlx::Error> {
        let room = self.pending.available_permits().min(CLAIM_BATCH as usize);
        if room == 0 {
            return Ok(Vec::new());
        }
        // Reserved before claiming, so that passes claiming at the same time
        // never take more than there is room for. If another pass took some of
        // it in between, this one claims nothing; the next pass will.
        let Ok(mut reserved) = Arc::clone(&self.pending).try_acquire_many_owned(room as u32) else {
            return Ok(Vec::new());
        };
        let monitors = claim_due_monitors(pool, room as i64).await?;
        // The places nobody claimed go back when `reserved` is dropped.
        Ok(monitors
            .into_iter()
            .map(|monitor| {
                let place = reserved
                    .split(1)
                    .expect("a reserved place for every claimed monitor");
                (monitor, place)
            })
            .collect())
    }
}

/// The checks of one pass, running or waiting for a place to run, handed out
/// one by one as they end, in whatever order that is.
struct RunningChecks {
    tasks: JoinSet<CheckOutcome>,
    monitors: HashMap<tokio::task::Id, Monitor>,
}

impl RunningChecks {
    /// Starts `check` for every monitor in a task of its own. A task runs its
    /// check once it holds one of `in_flight`'s places, and keeps the pending
    /// place it was given (if any) until the check has ended.
    fn start<F, Fut>(
        monitors: Vec<(Monitor, Option<OwnedSemaphorePermit>)>,
        in_flight: &Arc<Semaphore>,
        check: F,
    ) -> Self
    where
        F: Fn(Monitor) -> Fut,
        Fut: Future<Output = CheckOutcome> + Send + 'static,
    {
        let mut tasks = JoinSet::new();
        let mut started = HashMap::new();
        for (monitor, pending_place) in monitors {
            let work = check(monitor.clone());
            let in_flight = Arc::clone(in_flight);
            let task = tasks.spawn(async move {
                let _pending_place = pending_place;
                let _running = in_flight
                    .acquire_owned()
                    .await
                    .expect("the check semaphore is never closed");
                work.await
            });
            started.insert(task.id(), monitor);
        }
        Self {
            tasks,
            monitors: started,
        }
    }

    /// The next check to end, with its monitor, or `None` once all have. A
    /// check whose task panicked does not vanish: its monitor gets a `Down`
    /// outcome with a generic error, so it still gets a result (and the panic
    /// is logged with the monitor's id).
    async fn next(&mut self) -> Option<(Monitor, CheckOutcome)> {
        while let Some(joined) = self.tasks.join_next_with_id().await {
            let (id, outcome) = match joined {
                Ok((id, outcome)) => (id, Ok(outcome)),
                Err(e) => (e.id(), Err(e)),
            };
            let Some(monitor) = self.monitors.remove(&id) else {
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
            return Some((monitor, outcome));
        }
        None
    }
}

/// One pass: claims the due monitors there is room for and checks them. As
/// each check ends, its outcome is applied to its monitor (state and incident)
/// and its alert handed over (see [`PassAlerts`]): a slow check holds up no
/// other monitor's state change or alert. Once the last check has ended, the
/// results are stored in ClickHouse; history comes last because it is the one
/// step that can be slow without anyone being hurt (a hung insert only delays
/// the end of this pass, by at most the client's own 30 s timeout). Returns
/// how many checks ran.
async fn pass(
    state: &AppState,
    notifications: &Notifications,
    limits: &CheckLimits,
) -> Result<usize, AppError> {
    let claimed = limits.claim(&state.db).await?;
    if claimed.is_empty() {
        return Ok(0);
    }

    // Built per pass and dropped with it, never kept between passes: see
    // `CheckContext` for why.
    let ctx = Arc::new(CheckContext::for_state(state));
    let claimed = claimed
        .into_iter()
        .map(|(monitor, place)| (monitor, Some(place)))
        .collect();
    let mut checks = RunningChecks::start(claimed, &limits.in_flight, move |monitor| {
        let ctx = Arc::clone(&ctx);
        async move { checker::perform_check(&ctx, &monitor).await }
    });

    let mut alerts = PassAlerts::new(state, notifications);
    let mut rows = Vec::new();
    while let Some((monitor, outcome)) = checks.next().await {
        match apply_outcome(&state.db, &monitor, &outcome).await {
            Ok(Some(transition)) => alerts.send(transition),
            Ok(None) => {}
            Err(e) => {
                tracing::error!(error = %e, monitor_id = %monitor.id, "failed to apply check outcome")
            }
        }
        rows.push(UptimeCheckRow::from_outcome(&monitor, &outcome));
    }
    // Every alert of the pass is handed over: its task sends the rest and ends.
    drop(alerts);

    // ClickHouse holds history only: if it is unavailable, the state changes
    // and alerts above have already happened.
    if let Err(e) = store::insert_checks(&state.clickhouse, &rows).await {
        tracing::error!(error = %e, count = rows.len(), "failed to store uptime checks");
    }
    Ok(rows.len())
}

/// One pass over due monitors on its own, with limits of its own (see
/// [`pass`]); `run_forever`'s passes share theirs. Returns how many checks ran.
pub async fn run_pass(state: &AppState, notifications: &Notifications) -> Result<usize, AppError> {
    pass(state, notifications, &CheckLimits::new()).await
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
/// sends them in the background like the alerts of a check pass. Returns how
/// many went down.
pub async fn sweep_pass(
    state: &AppState,
    notifications: &Notifications,
) -> Result<usize, AppError> {
    let transitions = heartbeat::sweep_overdue(&state.db, SWEEP_BATCH).await?;
    let swept = transitions.len();
    spawn_alerts(state, notifications, transitions);
    Ok(swept)
}

/// [`sweep_pass`] that also waits for the alerts it started to be sent, so the
/// caller knows they are out when it returns. Returns how many heartbeats went
/// down.
pub async fn sweep_heartbeats(state: &AppState) -> Result<usize, AppError> {
    let notifications = Notifications::new();
    let swept = sweep_pass(state, &notifications).await?;
    notifications.drain().await;
    Ok(swept)
}

/// Runs the worker forever: the check loop and the sweep loop side by side
/// (see the module docs). Both fire at once when started, then every [`TICK`]
/// and [`SWEEP_INTERVAL`]. Every pass and every sweep runs in a task of its
/// own, so a panic inside one is logged instead of stopping the worker.
pub async fn run_forever(state: AppState) {
    let notifications = Notifications::new();
    tokio::join!(
        run_checks(state.clone(), notifications.clone()),
        run_sweeps(state, notifications),
    );
}

/// Starts a pass every [`TICK`], never waiting for the passes before it. They
/// share one set of [`CheckLimits`], and they all stop when this is dropped.
async fn run_checks(state: AppState, notifications: Notifications) {
    let limits = CheckLimits::new();
    let mut passes = JoinSet::new();
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        notifications.reap();
        while let Some(ended) = passes.try_join_next() {
            if let Err(e) = ended {
                tracing::error!(error = %e, "uptime check pass panicked");
            }
        }
        let (state, notifications, limits) = (state.clone(), notifications.clone(), limits.clone());
        passes.spawn(async move {
            if let Err(e) = pass(&state, &notifications, &limits).await {
                tracing::error!(error = ?e, "uptime check pass failed");
            }
        });
    }
}

/// Every [`SWEEP_INTERVAL`]: records that the worker is alive, then sweeps the
/// overdue heartbeats, one run after the other. Until the first liveness record
/// after the start has succeeded, nothing is swept (see [`sweep_tick`]).
async fn run_sweeps(state: AppState, notifications: Notifications) {
    let mut starting = true;
    let mut ticker = tokio::time::interval(SWEEP_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let tick = tokio::spawn(sweep_tick(state.clone(), notifications.clone(), starting));
        match tick.await {
            Ok(recorded) => starting &= !recorded,
            Err(e) => tracing::error!(error = %e, "heartbeat sweep panicked"),
        }
    }
}

/// Records that the worker is alive (making up for an outage before, see
/// [`heartbeat::note_worker_alive`]) and then sweeps. Returns whether the
/// liveness was recorded. If it was not, nothing is swept either: a sweep must
/// never judge a heartbeat before an outage it may have to make up for has been
/// accounted for.
async fn sweep_tick(state: AppState, notifications: Notifications, starting: bool) -> bool {
    match heartbeat::note_worker_alive(&state.db, starting).await {
        Ok(Some(outage)) => tracing::warn!(
            from = %outage.from,
            to = %outage.to,
            extended = outage.extended,
            "no pings could be received during an outage; heartbeats that fell due during it get a full period"
        ),
        Ok(None) => {}
        Err(e) => {
            tracing::error!(error = %e, "could not record that the uptime worker is alive; heartbeat sweep skipped");
            return false;
        }
    }
    if let Err(e) = sweep_pass(&state, &notifications).await {
        tracing::error!(error = ?e, "heartbeat sweep failed");
    }
    true
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
            follow_redirects: true,
            verify_ssl: true,
            ..Monitor::default()
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

    /// Runs `check` for every monitor, at most `MAX_IN_FLIGHT` at a time, and
    /// returns the outcomes in the order the checks ended.
    async fn check_all<F, Fut>(monitors: Vec<Monitor>, check: F) -> Vec<(Monitor, CheckOutcome)>
    where
        F: Fn(Monitor) -> Fut,
        Fut: Future<Output = CheckOutcome> + Send + 'static,
    {
        let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
        let monitors = monitors
            .into_iter()
            .map(|monitor| (monitor, None))
            .collect();
        let mut checks = RunningChecks::start(monitors, &in_flight, check);
        let mut results = Vec::new();
        while let Some(result) = checks.next().await {
            results.push(result);
        }
        results
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

    /// The pass applies each outcome as its check ends: a fast check is handed
    /// out while a slow one started before it is still running.
    #[tokio::test]
    async fn checks_are_handed_out_as_they_end_not_in_the_order_they_started() {
        let (slow, fast) = (monitor("slow"), monitor("fast"));
        let fast_id = fast.id;
        let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
        let mut checks = RunningChecks::start(
            vec![(slow, None), (fast, None)],
            &in_flight,
            move |monitor| {
                let released = Arc::clone(&released);
                async move {
                    if monitor.name == "slow" {
                        let wait = released.lock().await.take().unwrap();
                        let _ = wait.await;
                    }
                    up()
                }
            },
        );

        let (first, _) = tokio::time::timeout(Duration::from_secs(5), checks.next())
            .await
            .expect("the fast check is handed out while the slow one still runs")
            .unwrap();
        assert_eq!(first.id, fast_id);

        release.send(()).unwrap();
        let (second, _) = checks.next().await.unwrap();
        assert_eq!(second.name, "slow");
        assert!(checks.next().await.is_none());
    }

    /// A check keeps its pending place until it has ended, so the room for new
    /// claims only comes back as checks end.
    #[tokio::test]
    async fn a_check_holds_its_pending_place_until_it_ends() {
        let pending = Arc::new(Semaphore::new(1));
        let place = Arc::clone(&pending).try_acquire_owned().unwrap();
        let in_flight = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
        let (release, released) = tokio::sync::oneshot::channel::<()>();
        let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
        let mut checks = RunningChecks::start(
            vec![(monitor("held"), Some(place))],
            &in_flight,
            move |_| {
                let released = Arc::clone(&released);
                async move {
                    let wait = released.lock().await.take().unwrap();
                    let _ = wait.await;
                    up()
                }
            },
        );

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(pending.available_permits(), 0, "the running check holds it");
        release.send(()).unwrap();
        assert!(checks.next().await.is_some());
        assert_eq!(pending.available_permits(), 1, "given back when it ended");
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
