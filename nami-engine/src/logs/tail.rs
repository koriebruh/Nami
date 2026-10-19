//! Live tail (`GET /projects/{id}/logs/tail`): Server-Sent Events of rows
//! as they arrive.
//!
//! Every [`TailTimings::poll`] the engine reads the newest rows (at most
//! [`TAIL_BATCH`]) with `dt` in the last [`TailTimings::lookback`] and sends
//! the ones it has not sent yet as `event: log`, oldest first. Remembering
//! sent ids instead of keeping only the last `(dt, id)` means a row that
//! arrives late (a batching shipper's row whose `dt` is older than rows
//! already shown) is still delivered, as long as its `dt` is within the
//! look-back. Rows already stored when the tail starts are not sent.
//!
//! The session is re-checked every [`TailTimings::recheck`] (logged out,
//! password changed, or removed from the project ends the stream with
//! `event: end` / `{"reason":"session"}`), and the stream ends after
//! [`TailTimings::max_duration`] with `{"reason":"timeout"}`; the client
//! reconnects. A comment line keeps idle connections open.
//!
//! Resource safety: streams are capped per user and globally
//! ([`TailSlots`]); the sent-id set is bounded ([`MAX_SENT_IDS`]); no
//! database connection is held while the stream sleeps (every poll and every
//! re-check borrows one from the pool for the duration of one query); the
//! task ends as soon as the client goes away, even in the middle of a query.

use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::response::sse::Event;
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::{sync::mpsc, time::Instant};
use tower_sessions::Session;
use uuid::Uuid;

use super::{
    LogsState,
    query::{self, LogFilters, Sql},
    search::{self, ENTRY_COLUMNS, LogEntry, RawEntry},
};
use crate::{
    error::AppError,
    middleware::rbac::{require_project_role, user_from_session},
    state::AppState,
};

/// Most rows one poll reads; beyond that the newest are shown.
pub const TAIL_BATCH: u32 = 1000;

/// Concurrent tail streams one user may hold; the next is refused with 429.
pub const MAX_TAILS_PER_USER: usize = 5;

/// Concurrent tail streams of the whole instance.
pub const MAX_TAILS_GLOBAL: usize = 200;

/// Most sent ids remembered per stream. The look-back already bounds the set
/// to the rows of one window; this is the hard ceiling (the oldest rows go
/// first, so the worst case is a repeated old row, never unbounded memory).
pub const MAX_SENT_IDS: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailTimings {
    pub poll: Duration,
    pub keep_alive: Duration,
    pub recheck: Duration,
    pub max_duration: Duration,
    pub lookback: Duration,
}

impl Default for TailTimings {
    fn default() -> Self {
        TailTimings {
            poll: Duration::from_secs(1),
            keep_alive: Duration::from_secs(15),
            recheck: Duration::from_secs(60),
            max_duration: Duration::from_secs(30 * 60),
            lookback: Duration::from_secs(60),
        }
    }
}

pub type EventSender = mpsc::Sender<Result<Event, Infallible>>;

#[derive(Debug, Default)]
struct SlotCounts {
    total: usize,
    per_user: HashMap<Uuid, usize>,
}

/// How many tail streams are open, in total and per user.
#[derive(Debug, Default)]
pub struct TailSlots {
    counts: Mutex<SlotCounts>,
}

/// One open stream. Dropping it (when the streaming task ends, for whatever
/// reason) gives the slot back.
#[derive(Debug)]
pub struct TailPermit {
    logs: Arc<LogsState>,
    user_id: Uuid,
}

impl TailSlots {
    fn try_acquire(&self, user_id: Uuid, per_user: usize, global: usize) -> bool {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        if counts.total >= global || counts.per_user.get(&user_id).copied().unwrap_or(0) >= per_user
        {
            return false;
        }
        counts.total += 1;
        *counts.per_user.entry(user_id).or_insert(0) += 1;
        true
    }

    fn release(&self, user_id: Uuid) {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts.total = counts.total.saturating_sub(1);
        if let Some(open) = counts.per_user.get_mut(&user_id) {
            *open -= 1;
            if *open == 0 {
                counts.per_user.remove(&user_id);
            }
        }
    }

    /// (total, open streams of `user_id`).
    pub fn open(&self, user_id: Uuid) -> (usize, usize) {
        let counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        (
            counts.total,
            counts.per_user.get(&user_id).copied().unwrap_or(0),
        )
    }
}

impl TailPermit {
    /// A slot for `user_id`, or `RateLimited` (429) when the user or the
    /// instance is at its cap.
    pub fn acquire(logs: &Arc<LogsState>, user_id: Uuid) -> Result<TailPermit, AppError> {
        Self::acquire_with(logs, user_id, MAX_TAILS_PER_USER, MAX_TAILS_GLOBAL)
    }

    pub fn acquire_with(
        logs: &Arc<LogsState>,
        user_id: Uuid,
        per_user: usize,
        global: usize,
    ) -> Result<TailPermit, AppError> {
        if logs.tail_slots.try_acquire(user_id, per_user, global) {
            Ok(TailPermit {
                logs: logs.clone(),
                user_id,
            })
        } else {
            Err(AppError::RateLimited)
        }
    }
}

impl Drop for TailPermit {
    fn drop(&mut self) {
        self.logs.tail_slots.release(self.user_id);
    }
}

/// Ids already sent, with the `dt` of each so old ones can be forgotten.
#[derive(Debug, Default)]
struct SentIds(HashMap<Uuid, DateTime<Utc>>);

impl SentIds {
    fn contains(&self, id: &Uuid) -> bool {
        self.0.contains_key(id)
    }

    fn insert(&mut self, id: Uuid, dt: DateTime<Utc>) {
        self.0.insert(id, dt);
    }

    /// Forgets rows older than `since` (never read again), then, if still
    /// over `max`, the oldest ones.
    fn prune(&mut self, since: DateTime<Utc>, max: usize) {
        self.0.retain(|_, dt| *dt >= since);
        if self.0.len() > max {
            let mut by_age: Vec<(Uuid, DateTime<Utc>)> =
                self.0.iter().map(|(id, dt)| (*id, *dt)).collect();
            by_age.sort_by_key(|(_, dt)| *dt);
            for (id, _) in by_age.into_iter().take(self.0.len() - max) {
                self.0.remove(&id);
            }
        }
    }
}

fn end_event(reason: &str) -> Event {
    Event::default()
        .event("end")
        .data(json!({ "reason": reason }).to_string())
}

/// The newest rows of the project matching `filters` with `dt >= since`.
/// Same builder and the same read-only, time-limited client path as search.
async fn fetch(
    state: &AppState,
    project_id: Uuid,
    filters: &LogFilters,
    since: DateTime<Utc>,
) -> Result<Vec<LogEntry>, AppError> {
    let mut sql = Sql::new();
    sql.push(ENTRY_COLUMNS);
    query::push_where(&mut sql, project_id, filters);
    sql.push(" AND dt >= fromUnixTimestamp64Milli(")
        .value("Int64", since.timestamp_millis().to_string())
        .push(", 'UTC') ORDER BY dt DESC, id DESC LIMIT ")
        .value("UInt32", TAIL_BATCH.to_string());
    let rows: Vec<RawEntry> = state
        .clickhouse
        .query_rows(sql.text(), &sql.params())
        .await
        .map_err(search::user_error)?;
    Ok(rows.into_iter().map(LogEntry::from).collect())
}

enum SessionCheck {
    Valid,
    Gone,
    /// The check itself failed (database or session store down): keep the
    /// stream and ask again on the next tick.
    Unknown,
}

/// Reloads the session from the store and checks it still belongs to a
/// user (not logged out, no password change since) who may read the project.
async fn check_session(state: &AppState, session: &Session, project_id: Uuid) -> SessionCheck {
    if session.load().await.is_err() {
        return SessionCheck::Unknown;
    }
    let user = match user_from_session(state, session).await {
        Ok(user) => user,
        Err(AppError::Unauthorized) => return SessionCheck::Gone,
        Err(_) => return SessionCheck::Unknown,
    };
    match require_project_role(&state.db, &user, project_id, "viewer").await {
        Ok(_) => SessionCheck::Valid,
        Err(AppError::Forbidden) => SessionCheck::Gone,
        Err(_) => SessionCheck::Unknown,
    }
}

/// Streams into `tx` until the client goes away, the session becomes
/// invalid, or the maximum duration passes.
pub async fn run(
    state: AppState,
    session: Session,
    project_id: Uuid,
    filters: LogFilters,
    tx: EventSender,
) {
    let timings = state.logs.tail;
    let lookback = chrono::Duration::from_std(timings.lookback)
        .unwrap_or_else(|_| chrono::Duration::seconds(60));
    let deadline = Instant::now() + timings.max_duration;
    let mut next_recheck = Instant::now() + timings.recheck;
    let mut sent = SentIds::default();
    // Until one poll succeeded, rows are only remembered, never sent: they
    // were stored before the tail started.
    let mut primed = false;
    let mut ticker = tokio::time::interval(timings.poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tx.closed() => return,
        }
        if Instant::now() >= deadline {
            let _ = tx.send(Ok(end_event("timeout"))).await;
            return;
        }
        if Instant::now() >= next_recheck {
            let check = tokio::select! {
                check = check_session(&state, &session, project_id) => check,
                _ = tx.closed() => return,
            };
            match check {
                SessionCheck::Valid => next_recheck = Instant::now() + timings.recheck,
                SessionCheck::Gone => {
                    let _ = tx.send(Ok(end_event("session"))).await;
                    return;
                }
                SessionCheck::Unknown => {}
            }
        }
        let since = Utc::now() - lookback;
        let polled = tokio::select! {
            polled = fetch(&state, project_id, &filters, since) => polled,
            _ = tx.closed() => return,
        };
        let rows = match polled {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(error = ?error, "live tail poll failed");
                continue;
            }
        };
        for row in rows.into_iter().rev() {
            if sent.contains(&row.id) {
                continue;
            }
            sent.insert(row.id, row.dt);
            if !primed {
                continue;
            }
            let Ok(data) = serde_json::to_string(&row) else {
                continue;
            };
            if tx
                .send(Ok(Event::default().event("log").data(data)))
                .await
                .is_err()
            {
                return;
            }
        }
        primed = true;
        sent.prune(since, MAX_SENT_IDS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_cap_per_user_and_globally_and_release_on_drop() {
        let logs = Arc::new(LogsState::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = TailPermit::acquire_with(&logs, a, 2, 3).unwrap();
        let _second = TailPermit::acquire_with(&logs, a, 2, 3).unwrap();
        assert!(matches!(
            TailPermit::acquire_with(&logs, a, 2, 3),
            Err(AppError::RateLimited)
        ));
        let _other = TailPermit::acquire_with(&logs, b, 2, 3).unwrap();
        assert!(
            matches!(
                TailPermit::acquire_with(&logs, b, 2, 3),
                Err(AppError::RateLimited)
            ),
            "the global cap holds even for a user under their own cap"
        );
        drop(first);
        assert_eq!(logs.tail_slots.open(a), (2, 1));
        let _again = TailPermit::acquire_with(&logs, b, 2, 3).unwrap();
    }

    #[test]
    fn the_sent_set_forgets_old_rows_and_never_exceeds_its_ceiling() {
        let now = Utc::now();
        let mut sent = SentIds::default();
        let old = Uuid::new_v4();
        sent.insert(old, now - chrono::Duration::seconds(120));
        let mut newest = Vec::new();
        for i in 0..10 {
            let id = Uuid::new_v4();
            sent.insert(id, now - chrono::Duration::seconds(10 - i));
            newest.push(id);
        }
        sent.prune(now - chrono::Duration::seconds(60), 4);
        assert!(!sent.contains(&old));
        assert_eq!(sent.0.len(), 4);
        assert!(
            newest[6..].iter().all(|id| sent.contains(id)),
            "the newest survive"
        );
    }
}
