//! Live tail (`GET /projects/{id}/logs/tail`): Server-Sent Events of rows
//! as they arrive.
//!
//! Every [`TailTimings::poll`] the engine reads the ids of the newest rows
//! (at most [`TAIL_BATCH`]) with `dt` in the last [`TailTimings::lookback`],
//! then the list rows (`search::ENTRY_COLUMNS`, long messages cut) of only
//! the ones it has not sent yet, and sends those as `event: log`, oldest
//! first: a poll that finds nothing new reads ids and nothing else. Remembering
//! sent ids instead of keeping only the last `(dt, id)` means a row that
//! arrives late (a batching shipper's row whose `dt` is older than rows
//! already shown) is still delivered, as long as its `dt` is within the
//! look-back. Rows already stored when the tail starts are not sent.
//!
//! Every `event: log` carries `id:` [`Cursor::encode`] of its row. A client
//! that reconnects with that `Last-Event-ID` ([`resume_cursor`]: dated within
//! the last [`RESUME_WINDOW`]) gets the rows after it on its first poll,
//! oldest first, at most [`TailTimings::batch`] of them (with more, the
//! newest are replayed and the older part of the gap is lost). Late rows
//! dated at or before the cursor that arrived during the gap are not
//! replayed. The id is the row's own, so after a late row (sent after newer
//! ones) the resume sends those newer rows again; the explorer drops them by
//! id. A quiet tail whose last row is older than [`RESUME_WINDOW`] when it
//! reconnects (the 30-minute timeout included) starts normally, and rows
//! stored during the reconnect delay are not sent. The cursor only moves the
//! first poll's lower `dt` bound (never more than [`RESUME_WINDOW`] back)
//! and sets a bound flag column: the project and filters are the request's,
//! as for any poll.
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
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::response::sse::Event;
use chrono::{DateTime, Utc};
use serde_json::json;
use tokio::{sync::mpsc, time::Instant};
use tower_sessions::Session;
use uuid::Uuid;

use crate::logs::{
    LogsState,
    query::search::{self, ENTRY_COLUMNS, LogEntry, RawEntry},
    query::{self, Cursor, LogFilters, Sql},
};
use crate::{
    core::error::AppError,
    core::middleware::rbac::{Role, require_project_role, user_from_session},
    core::state::AppState,
};

/// Most rows one poll reads; beyond that the newest are shown.
pub const TAIL_BATCH: u32 = 200;

/// Rows dated further ahead than this are not tailed yet (ingest accepts a
/// `dt` up to a day ahead; without an upper bound such rows would fill the
/// newest-first batch and starve current ones). They arrive once their `dt`
/// is within this much of now.
pub const FUTURE_ALLOWANCE: Duration = Duration::from_secs(5);

/// Consecutive failed session re-checks (store or database down) after which
/// the stream ends: a session that cannot be verified is not served for the
/// whole maximum duration.
pub const MAX_UNKNOWN_CHECKS: u32 = 3;

/// Concurrent tail streams one user may hold; the next is refused with 429.
pub const MAX_TAILS_PER_USER: usize = 5;

/// Concurrent tail streams of the whole instance.
pub const MAX_TAILS_GLOBAL: usize = 200;

/// Most sent ids remembered per stream. The look-back already bounds the set
/// to the rows of one window; this is the hard ceiling (the oldest rows go
/// first, so the worst case is a repeated old row, never unbounded memory).
pub const MAX_SENT_IDS: usize = 20_000;

/// How far back a `Last-Event-ID` may point and still be resumed from.
pub const RESUME_WINDOW: Duration = Duration::from_secs(300);

/// Longest `Last-Event-ID` decoded; anything longer is ignored unread.
pub const MAX_RESUME_ID_BYTES: usize = 128;

/// A `Last-Event-ID` worth resuming from: a cursor of at most
/// [`MAX_RESUME_ID_BYTES`], dated within the last [`RESUME_WINDOW`] and not
/// more than [`FUTURE_ALLOWANCE`] ahead of `now`. Anything else is `None`
/// (a normal start).
pub fn resume_cursor(header: Option<&str>, now: DateTime<Utc>) -> Option<Cursor> {
    let raw = header?;
    if raw.len() > MAX_RESUME_ID_BYTES {
        return None;
    }
    let cursor = Cursor::decode(raw)?;
    let age_ms = now.timestamp_millis().checked_sub(cursor.dt_ms)?;
    let window_ms = i64::try_from(RESUME_WINDOW.as_millis()).ok()?;
    let ahead_ms = i64::try_from(FUTURE_ALLOWANCE.as_millis()).ok()?;
    (-ahead_ms..=window_ms).contains(&age_ms).then_some(cursor)
}

/// What the tails of this instance read (R-P2-25): id polls that succeeded
/// and list rows fetched. Relaxed counters, for tests and later metrics.
#[derive(Debug, Default)]
pub struct TailStats {
    pub id_polls: AtomicU64,
    pub rows_fetched: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailTimings {
    pub poll: Duration,
    pub keep_alive: Duration,
    pub recheck: Duration,
    pub max_duration: Duration,
    pub lookback: Duration,
    /// How long one event may wait for a client that stopped reading before
    /// the stream is dropped.
    pub send_timeout: Duration,
    /// Most rows one poll reads (tests shrink it).
    pub batch: u32,
}

impl Default for TailTimings {
    fn default() -> Self {
        TailTimings {
            poll: Duration::from_secs(1),
            keep_alive: Duration::from_secs(15),
            recheck: Duration::from_secs(60),
            max_duration: Duration::from_secs(30 * 60),
            lookback: Duration::from_secs(60),
            send_timeout: Duration::from_secs(10),
            batch: TAIL_BATCH,
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

#[derive(Debug, serde::Deserialize)]
struct IdRow {
    id: Uuid,
    dt: DateTime<Utc>,
    /// After the resume cursor (only read by a resume poll).
    #[serde(default)]
    after: bool,
}

/// The ids (and `dt`) of the newest rows of the project matching `filters`
/// with `dt` in `since..=until`, newest first, each flagged `after` when it
/// comes after `resume`. Same builder and the same read-only, time-limited
/// client path as search.
async fn fetch_ids(
    state: &AppState,
    project_id: Uuid,
    filters: &LogFilters,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
    batch: u32,
    resume: Option<Cursor>,
) -> Result<Vec<IdRow>, AppError> {
    let mut sql = Sql::new();
    match resume {
        // ClickHouse compares the tuple: its UUID order is the ids'
        // generation order, which Rust's `Uuid` order is not.
        Some(cursor) => {
            sql.push("SELECT id, dt, toBool((dt, id) > (fromUnixTimestamp64Milli(")
                .value("Int64", cursor.dt_ms.to_string())
                .push(", 'UTC'), ")
                .value("UUID", cursor.id.to_string())
                .push(")) AS after FROM logs");
        }
        None => {
            sql.push("SELECT id, dt FROM logs");
        }
    }
    query::push_where(&mut sql, project_id, filters);
    push_range(&mut sql, since, until);
    sql.push(" ORDER BY dt DESC, id DESC LIMIT ")
        .value("UInt32", batch.to_string());
    let rows = state
        .clickhouse
        .query_rows_with(sql.text(), &sql.params(), &search::READ_SETTINGS)
        .await
        .map_err(search::user_error)?;
    state.logs.tail_stats.id_polls.fetch_add(1, Ordering::Relaxed);
    Ok(rows)
}

/// The list rows of `ids` (rows of the project with `dt` in
/// `since..=until`), oldest first.
async fn fetch_rows(
    state: &AppState,
    project_id: Uuid,
    ids: &[Uuid],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<Vec<LogEntry>, AppError> {
    let mut sql = Sql::new();
    sql.push(ENTRY_COLUMNS)
        .push(" WHERE project_id = ")
        .value("UUID", project_id.to_string());
    push_range(&mut sql, since, until);
    sql.push(" AND id IN (");
    sql.list("UUID", ids);
    sql.push(") ORDER BY dt, id");
    let rows: Vec<RawEntry> = state
        .clickhouse
        .query_rows_with(sql.text(), &sql.params(), &search::READ_SETTINGS)
        .await
        .map_err(search::user_error)?;
    state
        .logs
        .tail_stats
        .rows_fetched
        .fetch_add(rows.len() as u64, Ordering::Relaxed);
    Ok(rows.into_iter().map(LogEntry::from).collect())
}

fn push_range(sql: &mut Sql, since: DateTime<Utc>, until: DateTime<Utc>) {
    sql.push(" AND dt >= fromUnixTimestamp64Milli(")
        .value("Int64", since.timestamp_millis().to_string())
        .push(", 'UTC') AND dt <= fromUnixTimestamp64Milli(")
        .value("Int64", until.timestamp_millis().to_string())
        .push(", 'UTC')");
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
    match require_project_role(&state.db, &user, project_id, Role::Viewer).await {
        Ok(_) => SessionCheck::Valid,
        Err(AppError::Forbidden) => SessionCheck::Gone,
        Err(_) => SessionCheck::Unknown,
    }
}

/// Sends `event`, waiting at most `send_timeout` and never past `deadline`.
/// False when the client is gone or not reading: the stream must end, so a
/// stalled client cannot pin its slot (and skip the deadline and re-checks).
async fn send_bounded(
    tx: &EventSender,
    event: Event,
    deadline: Instant,
    send_timeout: Duration,
) -> bool {
    let limit = deadline.min(Instant::now() + send_timeout);
    matches!(
        tokio::time::timeout_at(limit, tx.send(Ok(event))).await,
        Ok(Ok(()))
    )
}

/// Streams into `tx` until the client goes away, the session becomes
/// invalid, or the maximum duration passes. With `resume` (see
/// [`resume_cursor`]) the first poll sends the rows after it.
pub async fn run(
    state: AppState,
    session: Session,
    project_id: Uuid,
    filters: LogFilters,
    mut resume: Option<Cursor>,
    tx: EventSender,
) {
    let timings = state.logs.tail;
    let lookback = chrono::Duration::from_std(timings.lookback)
        .unwrap_or_else(|_| chrono::Duration::seconds(60));
    let resume_window =
        chrono::Duration::from_std(RESUME_WINDOW).unwrap_or_else(|_| chrono::Duration::minutes(5));
    let deadline = Instant::now() + timings.max_duration;
    let mut next_recheck = Instant::now() + timings.recheck;
    let mut sent = SentIds::default();
    let mut primed = false;
    let mut unknown_checks = 0u32;
    let mut ticker = tokio::time::interval(timings.poll);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = tx.closed() => return,
        }
        if Instant::now() >= deadline {
            let _ = send_bounded(
                &tx,
                end_event("timeout"),
                Instant::now() + timings.send_timeout,
                timings.send_timeout,
            )
            .await;
            return;
        }
        if Instant::now() >= next_recheck {
            let check = tokio::select! {
                check = check_session(&state, &session, project_id) => check,
                _ = tx.closed() => return,
            };
            match check {
                SessionCheck::Valid => {
                    unknown_checks = 0;
                    next_recheck = Instant::now() + timings.recheck;
                }
                SessionCheck::Gone => {
                    let _ = send_bounded(
                        &tx,
                        end_event("session"),
                        Instant::now() + timings.send_timeout,
                        timings.send_timeout,
                    )
                    .await;
                    return;
                }
                SessionCheck::Unknown => {
                    unknown_checks += 1;
                    if unknown_checks >= MAX_UNKNOWN_CHECKS {
                        let _ = send_bounded(
                            &tx,
                            end_event("session"),
                            Instant::now() + timings.send_timeout,
                            timings.send_timeout,
                        )
                        .await;
                        return;
                    }
                    next_recheck = Instant::now() + timings.recheck / 4;
                }
            }
        }
        let now = Utc::now();
        let since = now - lookback;
        let until = now + chrono::Duration::from_std(FUTURE_ALLOWANCE).unwrap_or_default();
        // A resume poll reads back to the cursor, and at least the usual
        // look-back, so every row a later poll can see is sent or remembered.
        // A resume retried for a while never reaches past RESUME_WINDOW.
        let poll_since = match resume {
            Some(cursor) => DateTime::from_timestamp_millis(cursor.dt_ms)
                .map_or(since, |at| at.min(since))
                .max(now - resume_window),
            None => since,
        };
        let polled = tokio::select! {
            polled = fetch_ids(&state, project_id, &filters, poll_since, until, timings.batch, resume) => polled,
            _ = tx.closed() => return,
        };
        let ids = match polled {
            Ok(ids) => ids,
            Err(error) => {
                tracing::warn!(error = ?error, "live tail poll failed");
                continue;
            }
        };
        // Until one poll succeeded, rows are only remembered (they were stored
        // before the tail started), except those after a resume cursor.
        let (fresh, known): (Vec<&IdRow>, Vec<&IdRow>) = ids
            .iter()
            .filter(|row| !sent.contains(&row.id))
            .partition(|row| primed || row.after);
        if fresh.is_empty() {
            for row in known {
                sent.insert(row.id, row.dt);
            }
            primed = true;
            resume = None;
            sent.prune(since, MAX_SENT_IDS);
            continue;
        }
        let fresh_ids: Vec<Uuid> = fresh.iter().map(|row| row.id).collect();
        let fetched = tokio::select! {
            fetched = fetch_rows(&state, project_id, &fresh_ids, poll_since, until) => fetched,
            _ = tx.closed() => return,
        };
        let rows = match fetched {
            Ok(rows) => rows,
            Err(error) => {
                // Not remembered (and a resume is kept): the next poll tries
                // these rows again.
                tracing::warn!(error = ?error, "live tail poll failed");
                continue;
            }
        };
        // Remembered even when not returned (gone in between): asked once.
        for row in fresh.into_iter().chain(known) {
            sent.insert(row.id, row.dt);
        }
        primed = true;
        resume = None;
        for row in rows {
            let Ok(data) = serde_json::to_string(&row) else {
                continue;
            };
            let id = Cursor {
                dt_ms: row.dt.timestamp_millis(),
                id: row.id,
            }
            .encode();
            let event = Event::default().event("log").id(id).data(data);
            if !send_bounded(&tx, event, deadline, timings.send_timeout).await {
                return;
            }
        }
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
    fn only_recent_well_formed_cursors_resume() {
        let now = Utc::now();
        let at = |offset: chrono::Duration| {
            Cursor {
                dt_ms: (now + offset).timestamp_millis(),
                id: Uuid::new_v4(),
            }
            .encode()
        };
        assert!(resume_cursor(Some(&at(-chrono::Duration::seconds(30))), now).is_some());
        assert!(
            resume_cursor(Some(&at(-chrono::Duration::minutes(6))), now).is_none(),
            "too old"
        );
        assert!(
            resume_cursor(Some(&at(chrono::Duration::minutes(5))), now).is_none(),
            "in the future"
        );
        assert!(resume_cursor(Some("not a cursor"), now).is_none());
        assert!(
            resume_cursor(Some(&"A".repeat(129)), now).is_none(),
            "over 128 bytes: not even decoded"
        );
        assert!(resume_cursor(None, now).is_none());

        let ms = chrono::Duration::milliseconds;
        let window = chrono::Duration::from_std(RESUME_WINDOW).unwrap();
        let ahead = chrono::Duration::from_std(FUTURE_ALLOWANCE).unwrap();
        assert!(resume_cursor(Some(&at(-window)), now).is_some(), "at the window");
        assert!(resume_cursor(Some(&at(-window - ms(1))), now).is_none());
        assert!(resume_cursor(Some(&at(ahead)), now).is_some(), "at the allowance");
        assert!(resume_cursor(Some(&at(ahead + ms(1))), now).is_none());
    }

    #[test]
    fn a_resume_id_is_read_up_to_its_length_cap() {
        use base64::Engine as _;
        let now = Utc::now();
        // Leading zeros still parse as the same `dt_ms`, so a valid cursor
        // can be padded to any length.
        let padded = |encoded_len: usize| {
            let raw_len = encoded_len * 3 / 4;
            let tail = format!("{}:{}", now.timestamp_millis(), Uuid::new_v4());
            let text = format!("{}{tail}", "0".repeat(raw_len - tail.len()));
            let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text);
            assert_eq!(encoded.len(), encoded_len);
            encoded
        };
        assert!(resume_cursor(Some(&padded(MAX_RESUME_ID_BYTES)), now).is_some());
        assert!(resume_cursor(Some(&padded(MAX_RESUME_ID_BYTES + 4)), now).is_none());
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
