//! The incidents worker: an escalation tick and a delivery drain every
//! [`TICK`] ([`run_forever`]), or at once ([`kick`]).

use std::time::Duration;

use tokio::{sync::Semaphore, time::MissedTickBehavior};

use crate::{
    core::state::AppState,
    incidents::{deliveries, escalation, send::Senders},
};

/// How often the worker loop escalates and drains the queue.
pub const TICK: Duration = Duration::from_secs(2);
/// Most kicks running at once in this process; the rest wait (R17).
pub const MAX_CONCURRENT_KICKS: usize = 2;

static KICKS: Semaphore = Semaphore::const_new(MAX_CONCURRENT_KICKS);

/// Kicks every [`TICK`] (missed ticks delayed), one at a time, each in a task
/// of its own so a panic is logged and never stops the loop. Not spawned yet:
/// NAMI-INC-10 spawns it from `main` unless `INCIDENTS_WORKER_ENABLED=false`.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(TICK);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let state = state.clone();
        if let Err(e) = tokio::spawn(async move { kick(&state).await }).await {
            tracing::error!(error = %e, "incidents worker iteration panicked");
        }
    }
}

/// One escalation tick, then one drain, now. Used by notify_transition
/// (Task 10) and tests. Returns at once when
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
