//! `GET /projects/{id}/monitors/{monitor_id}/checks/export?from=&to=`: the
//! monitor's check history as a streamed CSV file.
//!
//! The file is never built in memory. ClickHouse is read one day of the range
//! at a time ([`store::checks_between`], at most
//! [`store::CHECKS_BETWEEN_LIMIT`] rows per query), the lines are batched into
//! chunks of about [`CHUNK_BYTES`] and sent over a channel of capacity 2 to
//! the response body, so one export holds at most a few chunks plus one slice.
//! At most [`MAX_EXPORTS_IN_FLIGHT`] exports run at once per engine; the slot
//! is an owned semaphore permit held by the producer task, which ends (and
//! frees it) when the client goes away, when the client stops reading for
//! [`SEND_STALL_TIMEOUT`], when the export has run for
//! [`EXPORT_TOTAL_DEADLINE`], on a ClickHouse error, or at the end of the
//! range. One user may hold at most [`MAX_EXPORTS_PER_USER`] of the slots.
//!
//! The first slice is read before the response starts, so a ClickHouse outage
//! is the usual JSON 500; a failure later can only abort the body (the client
//! sees a cut connection, never a short file that looks complete).
//!
//! Every cell goes through [`neutralize`]: a value a spreadsheet would read as
//! a formula (`=`, `+`, `-`, `@`, tab, CR first) gets a leading `'`. Check
//! errors carry text from the monitored server (status lines, TLS and DNS
//! messages), so they are attacker-controlled.

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{
    Extension,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::header,
    response::{IntoResponse, Response},
    routing::{MethodRouter, get},
};
use chrono::{DateTime, TimeDelta, Utc};
use tokio::{
    sync::{Semaphore, mpsc},
    time::{Instant, timeout_at},
};
use uuid::Uuid;

use super::{
    availability::RangeQuery,
    checker::CheckStatus,
    store::{self, CHECKS_BETWEEN_LIMIT, UptimeCheckRow},
};
pub use crate::download::neutralize;
use crate::{
    clickhouse::ClickHouseError,
    download::{AbortableBody, instance_busy, user_busy},
    error::AppError,
    logs::slots::UserSlots,
    middleware::rbac::{CurrentUser, require_project_role},
    models::monitor,
    state::AppState,
};

/// Longest range one export may cover (`uptime_checks` keeps 180 days).
pub const MAX_EXPORT_DAYS: i64 = 180;
/// First line of every export.
pub const HEADER: &str = "checked_at,status,status_code,response_time_ms,error\r\n";
/// Exports running at once across the engine; one more is a 503
/// (`Retry-After: 5`).
pub const MAX_EXPORTS_IN_FLIGHT: usize = 4;
/// Size the producer batches lines into before sending them to the body.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// Chunks buffered between the producer and the response body.
const CHANNEL_CAPACITY: usize = 2;
/// How long the producer waits for the client to take the next chunk. A
/// client that stops reading longer than this loses its export (the body is
/// aborted) and its slot is freed, so four stalled downloads cannot hold every
/// slot forever.
pub const SEND_STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest one export may run, from the moment its producer starts. A
/// client that reads just fast enough to dodge [`SEND_STALL_TIMEOUT`] (one
/// chunk every 29 s) still gives its slot back after this, aborted.
pub const EXPORT_TOTAL_DEADLINE: Duration = Duration::from_secs(600);
/// Exports one user may run at once, so one viewer cannot take every slot;
/// one more is a 429 (`Retry-After: 5`).
pub const MAX_EXPORTS_PER_USER: usize = 2;

/// The export slots: one semaphore per router, so one per engine process
/// (each test app builds its own router and gets its own slots), plus the
/// per-user counts.
#[derive(Clone)]
pub struct ExportSlots {
    global: Arc<Semaphore>,
    users: Arc<UserSlots>,
}

impl Default for ExportSlots {
    fn default() -> Self {
        ExportSlots {
            global: Arc::new(Semaphore::new(MAX_EXPORTS_IN_FLIGHT)),
            users: Arc::default(),
        }
    }
}

/// The method router of the export route, with its slots attached.
pub fn route() -> MethodRouter<AppState> {
    get(export_checks).layer(Extension(ExportSlots::default()))
}

/// One RFC 4180 record (CRLF-terminated) for `row`, every cell neutralized.
pub fn csv_line(row: &UptimeCheckRow) -> String {
    let checked_at = row.checked_at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string();
    let status = match row.status {
        CheckStatus::Up => "up",
        CheckStatus::Down => "down",
    };
    let status_code = row.status_code.map(|c| c.to_string()).unwrap_or_default();
    let response_time = row
        .response_time_ms
        .map(|ms| ms.to_string())
        .unwrap_or_default();
    let error = row.error.as_deref().unwrap_or_default();
    let mut writer = csv::WriterBuilder::new()
        .terminator(csv::Terminator::CRLF)
        .quote_style(csv::QuoteStyle::Necessary)
        .buffer_capacity(512)
        .from_writer(Vec::with_capacity(128));
    writer
        .write_record([
            neutralize(&checked_at).as_ref(),
            neutralize(status).as_ref(),
            neutralize(&status_code).as_ref(),
            neutralize(&response_time).as_ref(),
            neutralize(error).as_ref(),
        ])
        .expect("writing to a Vec cannot fail");
    let bytes = writer.into_inner().expect("flushing to a Vec cannot fail");
    String::from_utf8(bytes).expect("every cell is UTF-8")
}

/// `from < to` and at most [`MAX_EXPORT_DAYS`] apart.
pub fn check_range(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<(), String> {
    if from >= to {
        return Err("from must be before to".into());
    }
    if to - from > TimeDelta::days(MAX_EXPORT_DAYS) {
        return Err(format!("the range must be at most {MAX_EXPORT_DAYS} days"));
    }
    Ok(())
}

/// End of the slice that starts at `start`: one day later, or `to`.
fn slice_end(start: DateTime<Utc>, to: DateTime<Utc>) -> DateTime<Utc> {
    (start + TimeDelta::days(1)).min(to)
}

async fn export_checks(
    State(state): State<AppState>,
    Extension(slots): Extension<ExportSlots>,
    user: CurrentUser,
    Path((project_id, monitor_id)): Path<(Uuid, Uuid)>,
    Query(range): Query<RangeQuery>,
) -> Result<Response, AppError> {
    require_project_role(&state.db, &user, project_id, "viewer").await?;
    check_range(range.from, range.to).map_err(AppError::Validation)?;
    monitor::find_in_project(&state.db, project_id, monitor_id)
        .await?
        .ok_or(AppError::NotFound)?;
    // Both permits are RAII guards: every early return below (and the end of
    // the producer task) gives them back.
    let Ok(user_permit) = slots.users.acquire(user.id, MAX_EXPORTS_PER_USER) else {
        return Ok(user_busy(MAX_EXPORTS_PER_USER));
    };
    let Ok(permit) = slots.global.try_acquire_owned() else {
        return Ok(instance_busy());
    };

    // Before the response starts: an outage here is the JSON 500.
    let first_end = slice_end(range.from, range.to);
    let first = store::checks_between(
        &state.clickhouse,
        project_id,
        monitor_id,
        range.from,
        first_end,
    )
    .await?;

    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let aborted = Arc::new(AtomicBool::new(false));
    let clickhouse = state.clickhouse.clone();
    let fetch = move |from: DateTime<Utc>, to: DateTime<Utc>| {
        let clickhouse = clickhouse.clone();
        async move { store::checks_between(&clickhouse, project_id, monitor_id, from, to).await }
    };
    let producer = Producer {
        monitor_id,
        to: range.to,
        tx,
        aborted: aborted.clone(),
        stall: SEND_STALL_TIMEOUT,
        deadline: Instant::now() + EXPORT_TOTAL_DEADLINE,
    };
    tokio::spawn(async move {
        // The slots are held for as long as the producer runs, no longer.
        let _permit = permit;
        let _user_permit = user_permit;
        producer.run(first, first_end, fetch).await;
    });

    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"checks-{monitor_id}.csv\""),
            ),
            (header::CACHE_CONTROL, "no-store".to_string()),
        ],
        Body::from_stream(AbortableBody::new(rx, aborted)),
    )
        .into_response())
}

/// Why a send did not go through.
enum SendFailure {
    /// The body was dropped: the client went away.
    Gone,
    /// The client did not take the chunk within the stall deadline.
    Stalled,
    /// The export reached [`EXPORT_TOTAL_DEADLINE`].
    Expired,
}

/// The task that reads the slices and feeds the body.
struct Producer {
    monitor_id: Uuid,
    to: DateTime<Utc>,
    tx: mpsc::Sender<Bytes>,
    /// Set when the export ends early on the server's side (ClickHouse error,
    /// stalled client, total deadline); the body then ends with an error
    /// instead of cleanly.
    aborted: Arc<AtomicBool>,
    stall: Duration,
    /// When the export is cut off whatever the client does.
    deadline: Instant,
}

impl Producer {
    /// Sends `HEADER`, the lines of `first` (the slice `[from, first_end)`),
    /// then reads and sends each further slice up to `to`. A slice that comes
    /// back full ([`CHECKS_BETWEEN_LIMIT`] rows) is continued one millisecond
    /// after its last row, so no check is dropped.
    async fn run<F, Fut>(self, first: Vec<UptimeCheckRow>, first_end: DateTime<Utc>, mut fetch: F)
    where
        F: FnMut(DateTime<Utc>, DateTime<Utc>) -> Fut,
        Fut: Future<Output = Result<Vec<UptimeCheckRow>, ClickHouseError>>,
    {
        let mut buf = String::with_capacity(CHUNK_BYTES + 1024);
        buf.push_str(HEADER);
        let mut rows = first;
        let mut end = first_end;
        loop {
            let full = rows.len() >= CHECKS_BETWEEN_LIMIT as usize;
            let last = rows.last().map(|row| row.checked_at);
            for row in rows.drain(..) {
                buf.push_str(&csv_line(&row));
                if buf.len() >= CHUNK_BYTES && self.send(&mut buf).await.is_err() {
                    return;
                }
            }
            let start = match (full, last) {
                (true, Some(last)) => last + TimeDelta::milliseconds(1),
                _ => end,
            };
            if start >= self.to {
                break;
            }
            if start >= end {
                end = slice_end(start, self.to);
            }
            // Hand over what is ready before waiting on ClickHouse, so the
            // client keeps receiving data however sparse the history is.
            if !buf.is_empty() && self.send(&mut buf).await.is_err() {
                return;
            }
            // Stop at once, cancelling the query, if the client leaves while
            // ClickHouse works.
            let fetched = tokio::select! {
                _ = self.tx.closed() => return,
                _ = tokio::time::sleep_until(self.deadline) => {
                    self.expire();
                    return;
                }
                fetched = fetch(start, end) => fetched,
            };
            match fetched {
                Ok(next) => rows = next,
                Err(e) => {
                    // ClickHouse's text can carry SQL: the error is logged by
                    // kind only.
                    tracing::error!(
                        monitor_id = %self.monitor_id,
                        error = error_kind(&e),
                        "check export aborted by a ClickHouse error"
                    );
                    self.aborted.store(true, Ordering::Release);
                    return;
                }
            }
        }
        if !buf.is_empty() {
            let _ = self.send(&mut buf).await;
        }
    }

    /// Sends and empties `buf`. A stalled client marks the export aborted.
    async fn send(&self, buf: &mut String) -> Result<(), SendFailure> {
        let chunk = Bytes::from(std::mem::replace(
            buf,
            String::with_capacity(CHUNK_BYTES + 1024),
        ));
        let stalled_at = Instant::now() + self.stall;
        let failure = match timeout_at(stalled_at.min(self.deadline), self.tx.send(chunk)).await {
            Ok(Ok(())) => return Ok(()),
            Ok(Err(_)) => SendFailure::Gone,
            Err(_) if Instant::now() >= self.deadline => SendFailure::Expired,
            Err(_) => SendFailure::Stalled,
        };
        match failure {
            SendFailure::Gone => {}
            SendFailure::Stalled => {
                tracing::warn!(monitor_id = %self.monitor_id, "check export stalled; aborting it");
                self.aborted.store(true, Ordering::Release);
            }
            SendFailure::Expired => self.expire(),
        }
        Err(failure)
    }

    /// Marks the export aborted at [`EXPORT_TOTAL_DEADLINE`].
    fn expire(&self) {
        tracing::warn!(
            monitor_id = %self.monitor_id,
            "check export reached its total deadline; aborting it"
        );
        self.aborted.store(true, Ordering::Release);
    }
}

/// A short, SQL-free name for `e`.
fn error_kind(e: &ClickHouseError) -> &'static str {
    match e {
        ClickHouseError::Server { .. } => "server error",
        ClickHouseError::ResponseTooLarge { .. } => "response too large",
        ClickHouseError::Decode { .. } => "decode error",
        _ => "transport error",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio_stream::StreamExt;

    use super::*;

    fn row(error: Option<&str>) -> UptimeCheckRow {
        UptimeCheckRow {
            monitor_id: Uuid::nil(),
            project_id: Uuid::nil(),
            checked_at: DateTime::from_timestamp_millis(1_792_000_000_123).unwrap(),
            status: CheckStatus::Down,
            status_code: Some(503),
            response_time_ms: Some(42),
            error: error.map(String::from),
        }
    }

    #[test]
    fn formula_cells_get_a_leading_quote() {
        for (raw, safe) in [
            ("=1+1", "'=1+1"),
            ("+cmd", "'+cmd"),
            ("-2", "'-2"),
            ("@SUM(A1)", "'@SUM(A1)"),
            ("\tx", "'\tx"),
            ("\rx", "'\rx"),
            ("ok", "ok"),
            ("", ""),
            (" =x", " =x"),
            ("a=b", "a=b"),
        ] {
            assert_eq!(neutralize(raw), safe, "{raw:?}");
        }
    }

    #[test]
    fn rows_are_rfc4180_lines() {
        assert_eq!(
            csv_line(&row(None)),
            "2026-10-14T17:46:40.123Z,down,503,42,\r\n"
        );
        assert_eq!(
            csv_line(&row(Some("expected status 200-399, got 503"))),
            "2026-10-14T17:46:40.123Z,down,503,42,\"expected status 200-399, got 503\"\r\n"
        );
        assert_eq!(
            csv_line(&row(Some("=HYPERLINK(\"http://evil.test\")"))),
            "2026-10-14T17:46:40.123Z,down,503,42,\"'=HYPERLINK(\"\"http://evil.test\"\")\"\r\n"
        );
        assert_eq!(
            csv_line(&row(Some("line one\nline two"))),
            "2026-10-14T17:46:40.123Z,down,503,42,\"line one\nline two\"\r\n"
        );
        // A neutralized CR cell is still quoted (it contains a CR).
        assert_eq!(
            csv_line(&row(Some("\r=1"))),
            "2026-10-14T17:46:40.123Z,down,503,42,\"'\r=1\"\r\n"
        );
        assert_eq!(
            csv_line(&row(Some("-1,@x"))),
            "2026-10-14T17:46:40.123Z,down,503,42,\"'-1,@x\"\r\n"
        );
        let up = UptimeCheckRow {
            status: CheckStatus::Up,
            status_code: None,
            response_time_ms: None,
            error: None,
            ..row(None)
        };
        assert_eq!(csv_line(&up), "2026-10-14T17:46:40.123Z,up,,,\r\n");
    }

    /// Whatever an error holds, the line parses back as exactly five fields
    /// and the error reads back as written, plus the quote when it needed one.
    #[test]
    fn any_error_text_round_trips_through_a_csv_reader() {
        for error in [
            "plain",
            "a,b",
            "\"quoted\"",
            "multi\nline\r\nend",
            "=cmd|' /C calc'!A0",
            "@@",
            "\t\ttabs",
            "ünïcödé ✓ 𝄞",
            "",
            ",",
            "\"",
        ] {
            let line = csv_line(&row(Some(error)));
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .from_reader(line.as_bytes());
            let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
            assert_eq!(records.len(), 1, "{error:?}");
            assert_eq!(records[0].len(), 5, "{error:?}");
            assert_eq!(&records[0][4], neutralize(error).as_ref(), "{error:?}");
        }
    }

    #[test]
    fn export_ranges_are_capped_at_180_days() {
        let t = |hours: i64| DateTime::from_timestamp(1_792_000_000 + hours * 3600, 0).unwrap();
        assert_eq!(
            check_range(t(1), t(1)),
            Err("from must be before to".into())
        );
        assert_eq!(
            check_range(t(2), t(1)),
            Err("from must be before to".into())
        );
        assert!(check_range(t(0), t(24 * 180)).is_ok());
        assert_eq!(
            check_range(t(0), t(24 * 180) + TimeDelta::seconds(1)),
            Err("the range must be at most 180 days".into())
        );
    }

    /// The worst row a slice can hold (every error char 4 UTF-8 bytes, cut
    /// to `MAX_ERROR_CHARS + 1` by the query, the `+ 1` being the checker's
    /// `…` marker) times the slice limit stays under the ClickHouse client's
    /// response cap, with room to spare. This relies on the checker's
    /// `clean_error` stripping control characters before a row is stored: a
    /// control char is escaped as `\u00XX` (6 bytes) in ClickHouse's JSON, more
    /// than the 4 bytes per char assumed here.
    #[test]
    fn a_full_slice_fits_the_clickhouse_response_cap() {
        let worst = UptimeCheckRow {
            error: Some("𝄞".repeat(crate::uptime::checker::MAX_ERROR_CHARS + 1)),
            status_code: Some(u16::MAX),
            response_time_ms: Some(u32::MAX),
            ..row(None)
        };
        let json_line = serde_json::to_string(&worst).unwrap().len() + 1;
        let slice = json_line * CHECKS_BETWEEN_LIMIT as usize;
        assert!(
            slice * 5 / 4 < crate::clickhouse::MAX_RESPONSE_BYTES,
            "{slice} bytes per slice"
        );
    }

    // ---- producer -------------------------------------------------------

    const T0: i64 = 1_792_000_000_000;

    fn at(ms: i64) -> DateTime<Utc> {
        DateTime::from_timestamp_millis(T0 + ms).unwrap()
    }

    fn rows_between(from: DateTime<Utc>, to: DateTime<Utc>, every_ms: i64) -> Vec<UptimeCheckRow> {
        let mut t = from;
        let mut out = Vec::new();
        while t < to && out.len() < CHECKS_BETWEEN_LIMIT as usize {
            out.push(UptimeCheckRow {
                checked_at: t,
                ..row(Some("connection refused"))
            });
            t += TimeDelta::milliseconds(every_ms);
        }
        out
    }

    type Calls = Arc<Mutex<Vec<(DateTime<Utc>, DateTime<Utc>)>>>;

    /// A fake `checks_between` over a check every `every_ms` from `T0`, that
    /// records every call.
    fn fake(
        every_ms: i64,
        calls: Calls,
    ) -> impl FnMut(
        DateTime<Utc>,
        DateTime<Utc>,
    ) -> std::future::Ready<Result<Vec<UptimeCheckRow>, ClickHouseError>> {
        move |from, to| {
            calls.lock().unwrap().push((from, to));
            // Align to the grid of checks.
            let offset = (from - at(0)).num_milliseconds();
            let first = at((offset + every_ms - 1).div_euclid(every_ms) * every_ms);
            std::future::ready(Ok(rows_between(first, to, every_ms)))
        }
    }

    fn producer(to: DateTime<Utc>, tx: mpsc::Sender<Bytes>, aborted: Arc<AtomicBool>) -> Producer {
        Producer {
            monitor_id: Uuid::nil(),
            to,
            tx,
            aborted,
            stall: SEND_STALL_TIMEOUT,
            deadline: Instant::now() + EXPORT_TOTAL_DEADLINE,
        }
    }

    async fn collect(stream: AbortableBody) -> (String, bool) {
        let mut body = String::new();
        let mut failed = false;
        tokio::pin!(stream);
        while let Some(item) = stream.next().await {
            match item {
                Ok(chunk) => body.push_str(std::str::from_utf8(&chunk).unwrap()),
                Err(_) => failed = true,
            }
        }
        (body, failed)
    }

    fn day(n: i64) -> i64 {
        n * 86_400_000
    }

    #[tokio::test]
    async fn every_check_of_the_range_is_sent_once_in_order_in_bounded_chunks() {
        // 3.5 days, one check a minute: 5040 rows over 4 slices.
        let to = at(day(3) + day(1) / 2);
        let calls: Calls = Arc::default();
        let mut fetch = fake(60_000, calls.clone());
        let first = fetch(at(0), slice_end(at(0), to)).into_inner().unwrap();
        calls.lock().unwrap().clear();
        let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
        let aborted = Arc::new(AtomicBool::new(false));
        let task =
            tokio::spawn(producer(to, tx, aborted.clone()).run(first, slice_end(at(0), to), fetch));
        let mut body = String::new();
        while let Some(chunk) = rx.recv().await {
            assert!(chunk.len() < CHUNK_BYTES + 1024, "{} bytes", chunk.len());
            body.push_str(std::str::from_utf8(&chunk).unwrap());
        }
        task.await.unwrap();
        assert!(!aborted.load(Ordering::Acquire));
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (at(day(1)), at(day(2))),
                (at(day(2)), at(day(3))),
                (at(day(3)), to),
            ]
        );
        let lines: Vec<&str> = body.split_terminator("\r\n").collect();
        assert_eq!(lines[0], HEADER.trim_end());
        assert_eq!(lines.len(), 1 + 5040);
        let mut previous = String::new();
        for line in &lines[1..] {
            let stamp = line.split(',').next().unwrap().to_string();
            assert!(stamp > previous, "{stamp} after {previous}");
            previous = stamp;
        }
    }

    /// A slice that comes back with the full limit is continued after its
    /// last row instead of losing the rest of the day.
    #[tokio::test]
    async fn a_full_slice_is_continued_after_its_last_row() {
        // One check every 2 s: 43200 a day, more than two full queries.
        let to = at(day(1));
        let calls: Calls = Arc::default();
        let mut fetch = fake(2_000, calls.clone());
        let first = fetch(at(0), to).into_inner().unwrap();
        assert_eq!(first.len(), CHECKS_BETWEEN_LIMIT as usize);
        calls.lock().unwrap().clear();
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let aborted = Arc::new(AtomicBool::new(false));
        tokio::spawn(producer(to, tx, aborted.clone()).run(first, to, fetch));
        let (body, failed) = collect(AbortableBody::new(rx, aborted)).await;
        assert!(!failed);
        assert_eq!(body.split_terminator("\r\n").count(), 1 + 43_200);
        let limit = i64::from(CHECKS_BETWEEN_LIMIT);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                (at((limit - 1) * 2_000 + 1), to),
                (at((2 * limit - 1) * 2_000 + 1), to),
            ]
        );
    }

    /// The client went away: the producer stops at the failed send and reads
    /// no further slice.
    #[tokio::test]
    async fn a_dropped_body_stops_the_producer_without_further_queries() {
        let to = at(day(10));
        let calls: Calls = Arc::default();
        let mut fetch = fake(30_000, calls.clone());
        let first = fetch(at(0), slice_end(at(0), to)).into_inner().unwrap();
        calls.lock().unwrap().clear();
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        drop(rx);
        let aborted = Arc::new(AtomicBool::new(false));
        tokio::time::timeout(
            Duration::from_secs(5),
            producer(to, tx, aborted.clone()).run(first, slice_end(at(0), to), fetch),
        )
        .await
        .expect("the producer returns");
        assert!(calls.lock().unwrap().is_empty());
        assert!(!aborted.load(Ordering::Acquire));
    }

    /// A client that stops reading loses the export after the stall
    /// deadline: the producer returns (freeing its slot) and the body ends
    /// with an error, not as a complete-looking file.
    #[tokio::test(start_paused = true)]
    async fn a_stalled_client_is_cut_off_after_the_deadline() {
        let to = at(day(10));
        let calls: Calls = Arc::default();
        let mut fetch = fake(30_000, calls.clone());
        let first = fetch(at(0), slice_end(at(0), to)).into_inner().unwrap();
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let aborted = Arc::new(AtomicBool::new(false));
        let started = tokio::time::Instant::now();
        producer(to, tx, aborted.clone())
            .run(first, slice_end(at(0), to), fetch)
            .await;
        assert!(started.elapsed() >= SEND_STALL_TIMEOUT);
        assert!(aborted.load(Ordering::Acquire));
        let (body, failed) = collect(AbortableBody::new(rx, aborted)).await;
        assert!(failed, "the body must end with an error");
        assert!(body.starts_with(HEADER));
        assert!(body.len() <= CHANNEL_CAPACITY * (CHUNK_BYTES + 1024));
    }

    /// A ClickHouse failure after the first slice aborts the body after what
    /// was already sent.
    #[tokio::test]
    async fn a_later_clickhouse_error_aborts_the_body() {
        let to = at(day(2));
        let first = rows_between(at(0), at(day(1)), 600_000);
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let aborted = Arc::new(AtomicBool::new(false));
        let fetch = |_: DateTime<Utc>, _: DateTime<Utc>| {
            std::future::ready(Err::<Vec<UptimeCheckRow>, _>(ClickHouseError::Server {
                status: 500,
                body: "Code: 241. SELECT secret FROM x".into(),
            }))
        };
        tokio::spawn(producer(to, tx, aborted.clone()).run(first, at(day(1)), fetch));
        let (body, failed) = collect(AbortableBody::new(rx, aborted)).await;
        assert!(failed);
        assert!(body.starts_with(HEADER));
    }

    /// A client that takes one chunk every 29 s never trips the stall
    /// deadline, but is still cut off at the total deadline: the body ends
    /// with an error and the slot comes back.
    #[tokio::test(start_paused = true)]
    async fn a_trickling_reader_is_cut_off_at_the_total_deadline() {
        let to = at(day(180));
        let calls: Calls = Arc::default();
        let mut fetch = fake(30_000, calls.clone());
        let first = fetch(at(0), slice_end(at(0), to)).into_inner().unwrap();
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        let aborted = Arc::new(AtomicBool::new(false));
        let slots = Arc::new(Semaphore::new(1));
        let permit = slots.clone().try_acquire_owned().unwrap();
        let started = Instant::now();
        let producer = producer(to, tx, aborted.clone());
        let task = tokio::spawn(async move {
            let _permit = permit;
            producer.run(first, slice_end(at(0), to), fetch).await;
        });
        let mut stream = Box::pin(AbortableBody::new(rx, aborted.clone()));
        let mut chunks = 0;
        let failed = loop {
            tokio::time::sleep(Duration::from_secs(29)).await;
            match stream.next().await {
                Some(Ok(_)) => chunks += 1,
                Some(Err(_)) => break true,
                None => break false,
            }
        };
        task.await.unwrap();
        assert!(failed, "the body must end with an error");
        assert!(aborted.load(Ordering::Acquire));
        assert!(chunks >= 20, "{chunks} chunks");
        let ended = started.elapsed();
        assert!(ended >= EXPORT_TOTAL_DEADLINE, "{ended:?}");
        assert!(
            ended < EXPORT_TOTAL_DEADLINE + Duration::from_secs(4 * 29),
            "{ended:?}"
        );
        assert_eq!(slots.available_permits(), 1, "the slot is free again");
    }
}
