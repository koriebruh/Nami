//! The incidents worker: an escalation tick and a delivery drain every
//! [`TICK`] ([`run_forever`]), or at once ([`kick`]), and the hourly prune
//! of spent action link tokens.

use std::time::{Duration, Instant};

use tokio::{sync::Semaphore, time::MissedTickBehavior};

use crate::{
    core::state::AppState,
    incidents::{action, deliveries, escalation, send::Senders},
};

/// How often the worker loop escalates and drains the queue.
pub const TICK: Duration = Duration::from_secs(2);
/// How often [`run_forever`] prunes spent action link tokens.
pub const PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// Most kicks running at once in this process; the rest wait (R17).
pub const MAX_CONCURRENT_KICKS: usize = 2;

static KICKS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_KICKS);

/// Kicks every [`TICK`] (missed ticks delayed), one at a time, each in a task
/// of its own so a panic is logged and never stops the loop, and prunes spent
/// action link tokens every [`PRUNE_EVERY`] (a failure is logged). `main`
/// spawns it unless `INCIDENTS_WORKER_ENABLED=false`.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pruned_at: Option<Instant> = None;
    loop {
        ticker.tick().await;
        if pruned_at.is_none_or(|at| at.elapsed() >= PRUNE_EVERY) {
            pruned_at = Some(Instant::now());
            if let Err(e) = action::prune_spent(&state.db).await {
                tracing::error!(error = %e, "pruning spent action link tokens failed");
            }
        }
        let state = state.clone();
        if let Err(e) = tokio::spawn(async move { kick(&state).await }).await {
            tracing::error!(error = %e, "incidents worker iteration panicked");
        }
    }
}

/// One escalation tick, then one drain, now. Used by
/// `uptime::alerts::notify_transition`, the log alert PATCH and tests.
/// Returns at once when
/// `!state.config.incidents_worker_enabled`. Otherwise waits for one of the
/// [`MAX_CONCURRENT_KICKS`] permits, then runs both, each bounded; a failed
/// tick is logged and never skips the drain.
pub async fn kick(state: &AppState) {
    if !state.config.incidents_worker_enabled {
        return;
    }
    // The semaphore is never closed.
    let Ok(_permit) = KICKS.acquire().await else {
        return;
    };
    if let Err(e) = escalation::run::tick(state).await {
        tracing::error!(error = ?e, "escalation tick failed");
    }
    let senders = Senders::from_config(&state.config);
    if let Err(e) = deliveries::drain(state, &senders).await {
        tracing::error!(error = ?e, "notification drain failed");
    }
}
