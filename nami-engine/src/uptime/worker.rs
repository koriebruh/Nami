//! The background uptime worker: every tick it claims due monitors, checks
//! them, stores the results, applies the state machine, and sends alerts.

use std::{sync::Arc, time::Duration};

use sqlx::PgPool;
use tokio::{sync::Semaphore, task::JoinSet};
use uuid::Uuid;

use super::{
    alerts::{self, Subject, Transition, TransitionKind},
    checker::{self, CheckOutcome, CheckStatus},
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
/// Checks in flight at once.
pub const MAX_IN_FLIGHT: usize = 20;

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
/// A monitor deleted or paused while its check was in flight is left alone.
pub async fn apply_outcome(
    pool: &PgPool,
    monitor: &Monitor,
    outcome: &CheckOutcome,
) -> Result<Option<Transition>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let row: Option<(Uuid, String, String, i32, i32, bool)> = sqlx::query_as(
        "select project_id, name, current_status, consecutive_failures, confirmation_threshold, paused
         from monitors where id = $1 for update",
    )
    .bind(monitor.id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((project_id, name, status, failures, threshold, paused)) = row else {
        return Ok(None);
    };
    if paused {
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
                let opened = incident::open_for_monitor(
                    &mut tx,
                    project_id,
                    monitor.id,
                    &format!("{name} is down"),
                    outcome.error.as_deref(),
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
                    cause: outcome.error.clone(),
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

/// One worker pass over due monitors. Returns how many checks ran.
pub async fn run_due_checks(state: &AppState) -> Result<usize, AppError> {
    let monitors = claim_due_monitors(&state.db, CLAIM_BATCH).await?;
    if monitors.is_empty() {
        return Ok(0);
    }

    // Built per pass: cheap next to the checks themselves, and it never
    // outlives the runtime that drives its connections.
    let http = checker::build_http_client();
    let semaphore = Arc::new(Semaphore::new(MAX_IN_FLIGHT));
    let mut checks = JoinSet::new();
    for monitor in monitors {
        let http = http.clone();
        let semaphore = Arc::clone(&semaphore);
        checks.spawn(async move {
            let _permit = semaphore
                .acquire_owned()
                .await
                .expect("the check semaphore is never closed");
            let outcome = checker::perform_check(&http, &monitor).await;
            (monitor, outcome)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = checks.join_next().await {
        match joined {
            Ok(result) => results.push(result),
            Err(e) => tracing::error!(error = %e, "uptime check task failed"),
        }
    }

    // ClickHouse holds history only: if it is unavailable, still apply state
    // changes and send alerts.
    let rows: Vec<UptimeCheckRow> = results
        .iter()
        .map(|(m, o)| UptimeCheckRow::from_outcome(m, o))
        .collect();
    if let Err(e) = store::insert_checks(&state.clickhouse, &rows).await {
        tracing::error!(error = %e, count = rows.len(), "failed to store uptime checks");
    }

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
    for transition in &transitions {
        alerts::notify_transition(state, transition).await;
    }
    Ok(results.len())
}

/// Ticks every [`TICK`] forever. Each tick runs in its own task, so even a
/// panic inside it is logged here instead of stopping the worker.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let tick_state = state.clone();
        if let Err(e) = tokio::spawn(async move { tick(&tick_state).await }).await {
            tracing::error!(error = %e, "uptime worker tick panicked");
        }
    }
}

async fn tick(state: &AppState) {
    if let Err(e) = run_due_checks(state).await {
        tracing::error!(error = ?e, "uptime check pass failed");
    }
}
