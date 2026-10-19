//! `GET /projects/{id}/logs/export?format=csv|ndjson`: the rows of a search
//! (same window and filters) as a streamed file, newest first, at most
//! [`MAX_EXPORT_ROWS`] of them.
//!
//! The file is never held in memory, here or in ClickHouse's answer: the
//! query is read line by line as ClickHouse produces it
//! ([`ClickHouse::stream_rows_with`], not the 32 MiB buffered read of the
//! search path), each line is turned into its output row, and the rows are
//! batched into chunks of at most [`EXPORT_CHUNK_BYTES`] that go over a
//! channel of [`EXPORT_CHANNEL_CHUNKS`] to the response body. One export
//! therefore holds one ClickHouse line (at most [`MAX_EXPORT_LINE_BYTES`]),
//! one output row and the queued chunks.
//!
//! **Caps** (R-P2-13). The SQL `LIMIT` is the row cap; reaching it is a
//! normal end (the UI warned first, `X-Nami-Export-Limit` tells API
//! clients). At most [`MAX_EXPORT_BYTES`] are written. The streaming task
//! ends at [`ExportLimits::deadline`] whatever the client does: every read
//! from ClickHouse and every send to the body runs under that deadline, so a
//! client that stays connected but stops reading cannot hold its slot.
//!
//! **Ending early.** An error before the first byte (a bad RE2, ClickHouse
//! down) is the usual JSON 400/500. After that, the file ends with an
//! explicit trailer, never just stops as if complete: NDJSON
//! `{"_nami_export_error":"<reason>"}`, CSV a last record
//! `# export stopped early: <reason>`. When even the trailer cannot be
//! delivered (the client stopped reading), the body ends with an error, so
//! the connection is cut instead of closed cleanly.
//!
//! **Mid-stream ClickHouse errors** (R-P2-12) are written into the body by
//! ClickHouse, in a shape that differs by version. So nothing is looked for:
//! every line must be a JSON object starting with the first selected
//! column's key (`{"id":` NDJSON, `{"dt":` CSV), and anything else ends the
//! file with the "query failed" trailer without being written.
//!
//! CSV cells go through [`crate::core::download::neutralize`] (formula guard), the
//! header included (a column path may start with `-`), and are always quoted
//! ([`push_csv_field`]).

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::{
    sync::mpsc,
    time::{Instant, timeout_at},
};
use uuid::Uuid;

use crate::core::clickhouse::{ClickHouseError, RowStream};
use crate::logs::query::{self, LogFilters, Sql, Window};

/// Rows one export carries at most (the SQL `LIMIT`).
pub const MAX_EXPORT_ROWS: u64 = 100_000;
/// Bytes one export writes at most (the trailer aside).
pub const MAX_EXPORT_BYTES: u64 = 512 * 1024 * 1024;
/// The longest ClickHouse line an export reads. A stored row is at most
/// 64 KiB of message plus 256 KiB of fields; JSON escaping makes that about
/// 2 MiB at worst.
pub const MAX_EXPORT_LINE_BYTES: usize = 4 * 1024 * 1024;
/// Exports one user may run at once (429 past it).
pub const MAX_EXPORTS_PER_USER: usize = 2;
/// Exports one engine instance runs at once, all users together (429 past
/// it, R-P2-15): each runs a ClickHouse query for up to 120 s.
pub const MAX_EXPORTS_PER_INSTANCE: usize = 8;
/// When the streaming task ends whatever the client does (R-P2-13). The
/// ClickHouse request timeout ([`crate::core::clickhouse::STREAM_REQUEST_TIMEOUT`])
/// is 5 s longer, so a long export ends here, with the time-limit trailer.
///
/// These caps are the export's own. ClickHouse has one more: its
/// `http_send_timeout` (server default 30 s) ends a query whose client has
/// not taken data for that long. A reader that pauses that long (the queue
/// here and the socket buffers full) can therefore be cut before the
/// deadline; the export then ends with the "interrupted" trailer (or a cut
/// connection when that cannot be sent).
pub const EXPORT_DEADLINE: Duration = Duration::from_secs(150);
/// How long past the deadline the trailer may take to go out.
pub const TRAILER_GRACE: Duration = Duration::from_secs(1);
/// CSV field-path columns at most.
pub const MAX_EXPORT_COLUMNS: usize = 10;
/// The largest chunk sent to the body. A larger row is split, so the
/// queued chunks never weigh more than `EXPORT_CHANNEL_CHUNKS` of these.
pub const EXPORT_CHUNK_BYTES: usize = 64 * 1024;
/// Chunks queued between the streaming task and the response body.
pub const EXPORT_CHANNEL_CHUNKS: usize = 16;
/// The header that announces the row cap.
pub const EXPORT_LIMIT_HEADER: &str = "x-nami-export-limit";

/// Why an export stopped early (the trailer text). Fixed, user-safe texts.
pub const REASON_TIME: &str =
    "the export reached its time limit: narrow the time window or add filters";
pub const REASON_QUERY_FAILED: &str =
    "the query failed while streaming: narrow the time window or add filters";
pub const REASON_ROW_TOO_LARGE: &str = "a row is larger than an export may carry";
pub const REASON_INTERRUPTED: &str =
    "the export was interrupted: try again or narrow the time window";
pub const REASON_SIZE: &str =
    "the export reached its 512 MiB size limit: narrow the time window or add filters";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Ndjson,
}

/// The caps of one export ([`Default`] = the constants; tests shrink them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportLimits {
    pub rows: u64,
    pub bytes: u64,
    pub line_bytes: usize,
    pub deadline: Duration,
}

impl Default for ExportLimits {
    fn default() -> Self {
        ExportLimits {
            rows: MAX_EXPORT_ROWS,
            bytes: MAX_EXPORT_BYTES,
            line_bytes: MAX_EXPORT_LINE_BYTES,
            deadline: EXPORT_DEADLINE,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportParams {
    pub window: Window,
    pub filters: LogFilters,
    pub format: ExportFormat,
    /// CSV only: field paths, each a list of segments.
    pub columns: Vec<Vec<String>>,
}

/// The search window and filters, then `format` (`csv` | `ndjson`) and the
/// repeatable `columns` (search-syntax paths, at most
/// [`MAX_EXPORT_COLUMNS`]).
pub fn parse_export(
    pairs: &[(String, String)],
    now: DateTime<Utc>,
) -> Result<ExportParams, String> {
    let window = query::parse_window(pairs, now)?;
    let filters = query::parse_filters(pairs)?;
    let format = match query::last(pairs, "format").map(str::trim) {
        Some("csv") => ExportFormat::Csv,
        Some("ndjson") => ExportFormat::Ndjson,
        _ => return Err("format must be csv or ndjson".to_string()),
    };
    let mut columns = Vec::new();
    for raw in query::values(pairs, "columns") {
        if columns.len() == MAX_EXPORT_COLUMNS {
            return Err(format!("at most {MAX_EXPORT_COLUMNS} columns"));
        }
        columns.push(query::parse_path(raw.trim()).map_err(|e| format!("columns: {e}"))?);
    }
    Ok(ExportParams {
        window,
        filters,
        format,
        columns,
    })
}

/// Every stored column, `id` first (the line check relies on it).
const NDJSON_SELECT: &str = "SELECT id, project_id, source_id, dt, level, message, fields, trace_id, span_id, \
     pattern, toString(pattern_hash) AS pattern_hash, retention_days FROM logs";
/// The fixed CSV columns, `dt` first (the line check relies on it).
const CSV_SELECT: &str = "SELECT dt, level, source_id, message, trace_id";
const COLUMN_ALIASES: [&str; MAX_EXPORT_COLUMNS] = [
    " AS c0", " AS c1", " AS c2", " AS c3", " AS c4", " AS c5", " AS c6", " AS c7", " AS c8",
    " AS c9",
];
const COLUMN_KEYS: [&str; MAX_EXPORT_COLUMNS] =
    ["c0", "c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8", "c9"];

/// The export query: fixed fragments and numbered parameters only (column
/// paths, filters and the project are all bound), newest first, `limit`
/// rows at most.
pub fn export_sql(project_id: Uuid, params: &ExportParams, limit: u64) -> Sql {
    let mut sql = Sql::new();
    match params.format {
        ExportFormat::Ndjson => {
            sql.push(NDJSON_SELECT);
        }
        ExportFormat::Csv => {
            sql.push(CSV_SELECT);
            for (path, alias) in params.columns.iter().zip(COLUMN_ALIASES) {
                sql.push(", ");
                query::push_field_text(&mut sql, path);
                sql.push(alias);
            }
            sql.push(" FROM logs");
        }
    }
    query::push_where(&mut sql, project_id, &params.filters);
    query::push_window(&mut sql, &params.window);
    sql.push(" ORDER BY dt DESC, id DESC LIMIT ")
        .value("UInt64", limit.to_string());
    sql
}

pub use crate::core::download::{csv_record, push_csv_field};

/// `nami-logs-<to as %Y%m%dT%H%M%SZ>.<csv|ndjson>`: no user text.
pub fn file_name(format: ExportFormat, to: DateTime<Utc>) -> String {
    let extension = match format {
        ExportFormat::Csv => "csv",
        ExportFormat::Ndjson => "ndjson",
    };
    format!("nami-logs-{}.{extension}", to.format("%Y%m%dT%H%M%SZ"))
}

/// The CSV header record.
fn csv_header(columns: &[Vec<String>]) -> String {
    let paths: Vec<String> = columns.iter().map(|path| path.join(".")).collect();
    let mut fields = vec!["dt", "level", "source", "message", "trace_id"];
    fields.extend(paths.iter().map(String::as_str));
    csv_record(&fields)
}

/// The trailer that ends a file cut short.
fn trailer(format: ExportFormat, reason: &str) -> String {
    match format {
        ExportFormat::Ndjson => {
            let mut line = serde_json::json!({ "_nami_export_error": reason }).to_string();
            line.push('\n');
            line
        }
        ExportFormat::Csv => csv_record(&[&format!("# export stopped early: {reason}")]),
    }
}

/// The text of a JSON value as a CSV cell: strings as they are, other
/// values as JSON text, a missing one empty.
fn cell(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// The CSV record of one ClickHouse line, or `None` when it is not a JSON
/// object.
fn csv_row(line: &[u8], columns: usize, sources: &HashMap<Uuid, String>) -> Option<String> {
    let row: serde_json::Map<String, Value> = serde_json::from_slice(line).ok()?;
    let source_id = cell(row.get("source_id"));
    let source = source_id
        .parse::<Uuid>()
        .ok()
        .and_then(|id| sources.get(&id).cloned())
        .unwrap_or(source_id);
    let mut cells = vec![
        cell(row.get("dt")),
        cell(row.get("level")),
        source,
        cell(row.get("message")),
        cell(row.get("trace_id")),
    ];
    cells.extend(COLUMN_KEYS[..columns].iter().map(|key| cell(row.get(*key))));
    let refs: Vec<&str> = cells.iter().map(String::as_str).collect();
    Some(csv_record(&refs))
}

/// Why a send did not go through.
enum SendFailure {
    /// The body was dropped: the client went away.
    Gone,
    /// The client did not take the chunk before the deadline.
    Expired,
}

/// The output side: a buffer of at most about one chunk plus one row, sent
/// in pieces of at most [`EXPORT_CHUNK_BYTES`].
struct Output {
    tx: mpsc::Sender<Bytes>,
    buf: Vec<u8>,
    /// Bytes written so far (the trailer aside).
    written: u64,
}

impl Output {
    async fn push(&mut self, bytes: &[u8], until: Instant) -> Result<(), SendFailure> {
        self.buf.extend_from_slice(bytes);
        self.written += bytes.len() as u64;
        if self.buf.len() >= EXPORT_CHUNK_BYTES {
            self.flush(until).await
        } else {
            Ok(())
        }
    }

    /// Sends the buffer, in pieces, each under `until`.
    async fn flush(&mut self, until: Instant) -> Result<(), SendFailure> {
        for piece in self.buf.chunks(EXPORT_CHUNK_BYTES) {
            match timeout_at(until, self.tx.send(Bytes::copy_from_slice(piece))).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(SendFailure::Gone),
                Err(_) => return Err(SendFailure::Expired),
            }
        }
        self.buf.clear();
        // A huge row grew the buffer: give that memory back.
        self.buf.shrink_to(2 * EXPORT_CHUNK_BYTES);
        Ok(())
    }
}

/// A short, SQL-free name for `e` (ClickHouse's text can carry SQL).
fn error_kind(e: &ClickHouseError) -> &'static str {
    match e {
        ClickHouseError::Server { .. } => "server error",
        ClickHouseError::Transport(_) => "transport error",
        _ => "other error",
    }
}

/// The trailer for a read from ClickHouse that failed mid-stream.
fn stop_reason(e: &ClickHouseError) -> &'static str {
    match e {
        ClickHouseError::LineTooLong { .. } => REASON_ROW_TOO_LARGE,
        // reqwest's own timeout (5 s after the deadline): the same time
        // limit, said the same way.
        ClickHouseError::Transport(error) if error.is_timeout() => REASON_TIME,
        _ => {
            tracing::warn!(error = error_kind(e), "a log export was interrupted");
            REASON_INTERRUPTED
        }
    }
}

/// Writes the export: the CSV header, then one output row per ClickHouse
/// line, then (when it stopped early) the trailer. Returns at the end, at
/// the deadline, or at once when the client goes away (dropping `rows`
/// closes the ClickHouse connection, which cancels the query). Sets
/// `aborted` when the file could not be ended properly (the body then ends
/// with an error).
pub async fn stream(
    mut rows: RowStream,
    params: ExportParams,
    sources: HashMap<Uuid, String>,
    limits: ExportLimits,
    tx: mpsc::Sender<Bytes>,
    aborted: Arc<AtomicBool>,
) {
    let deadline = Instant::now() + limits.deadline;
    let mut out = Output {
        tx,
        buf: Vec::with_capacity(2 * EXPORT_CHUNK_BYTES),
        written: 0,
    };
    let first_key: &[u8] = match params.format {
        ExportFormat::Ndjson => b"{\"id\":",
        ExportFormat::Csv => b"{\"dt\":",
    };
    let abort = |failure: SendFailure| {
        if let SendFailure::Expired = failure {
            tracing::warn!("a log export was not read before its deadline; aborting it");
            aborted.store(true, Ordering::Release);
        }
    };
    if params.format == ExportFormat::Csv {
        let header = csv_header(&params.columns);
        if let Err(failure) = out.push(header.as_bytes(), deadline).await {
            return abort(failure);
        }
    }
    let mut count = 0u64;
    let reason = loop {
        if count >= limits.rows {
            break None;
        }
        let next = tokio::select! {
            biased;
            _ = out.tx.closed() => return,
            next = timeout_at(deadline, rows.next_line()) => next,
        };
        let line = match next {
            Err(_) => break Some(REASON_TIME),
            Ok(Ok(None)) => break None,
            Ok(Ok(Some(line))) => line,
            Ok(Err(e)) => break Some(stop_reason(&e)),
        };
        // `Err` gives the line back for the log.
        let row: Result<Vec<u8>, Vec<u8>> = if !line.starts_with(first_key) {
            Err(line)
        } else {
            match params.format {
                // Verbatim, but only a whole JSON object: a row cut short by
                // an error ClickHouse wrote after it is never passed on.
                ExportFormat::Ndjson => {
                    if serde_json::from_slice::<serde::de::IgnoredAny>(&line).is_ok() {
                        let mut row = line;
                        row.push(b'\n');
                        Ok(row)
                    } else {
                        Err(line)
                    }
                }
                ExportFormat::Csv => csv_row(&line, params.columns.len(), &sources)
                    .map(String::into_bytes)
                    .ok_or(line),
            }
        };
        let row = match row {
            Ok(row) => row,
            Err(line) => {
                let shown = &line[..line.len().min(200)];
                tracing::warn!(
                    line = %String::from_utf8_lossy(shown),
                    "a log export met a line that is not a row; ending it"
                );
                break Some(REASON_QUERY_FAILED);
            }
        };
        if out.written + row.len() as u64 > limits.bytes {
            break Some(REASON_SIZE);
        }
        if let Err(failure) = out.push(&row, deadline).await {
            return abort(failure);
        }
        count += 1;
    };
    drop(rows);
    let until = match reason {
        None => deadline,
        Some(reason) => {
            out.buf
                .extend_from_slice(trailer(params.format, reason).as_bytes());
            deadline.max(Instant::now()) + TRAILER_GRACE
        }
    };
    if let Err(failure) = out.flush(until).await {
        abort(failure);
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use super::*;
    use crate::{
        core::clickhouse::ClickHouse,
        core::download::{AbortableBody, neutralize},
    };

    #[test]
    fn csv_fields_follow_rfc_4180() {
        assert_eq!(
            csv_record(&["a", "b,c", "say \"hi\"", "line\nbreak", ""]),
            "\"a\",\"b,c\",\"say \"\"hi\"\"\",\"line\nbreak\",\"\"\r\n"
        );
    }

    #[test]
    fn formula_cells_are_prefixed() {
        for (input, expected) in [
            ("=cmd|' /C calc'!A0", "\"'=cmd|' /C calc'!A0\""),
            ("+1", "\"'+1\""),
            ("-2", "\"'-2\""),
            ("@SUM(1)", "\"'@SUM(1)\""),
            ("\tx", "\"'\tx\""),
            ("\rx", "\"'\rx\""),
            ("=1,2", "\"'=1,2\""),
            ("a=1", "\"a=1\""),
            (" =1", "\" =1\""),
            ("x;=2+5", "\"x;=2+5\""),
        ] {
            let mut out = String::new();
            push_csv_field(&mut out, input);
            assert_eq!(out, expected, "{input:?}");
        }
    }

    /// Whatever a cell holds, a CSV reader gets back exactly the fields
    /// written (formula-guarded), one record per row.
    #[test]
    fn any_cell_round_trips_through_a_csv_reader() {
        let cells = [
            "plain",
            "a,b",
            "\"q\"",
            "multi\nline\r\nend",
            "=cmd|' /C calc'!A0",
            "-1,@x",
            "ünïcödé ✓ 𝄞",
            "",
            ",",
            "\"",
        ];
        let record = csv_record(&cells);
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(record.as_bytes());
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        assert_eq!(records.len(), 1);
        let read: Vec<&str> = records[0].iter().collect();
        let expected: Vec<String> = cells.iter().map(|c| neutralize(c).into_owned()).collect();
        assert_eq!(read, expected);
    }

    #[test]
    fn export_parameters_are_validated() {
        let now = Utc::now();
        let pairs = |q: &str| query::query_pairs(Some(q));
        assert!(parse_export(&pairs("format=csv"), now).is_ok());
        assert_eq!(
            parse_export(&pairs("format=ndjson"), now).unwrap().format,
            ExportFormat::Ndjson
        );
        for bad in [
            "",
            "format=xlsx",
            "format=csv&columns=a..b",
            "format=csv&columns=",
            "format=csv&level=loud",
            "format=csv&from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z",
        ] {
            assert!(parse_export(&pairs(bad), now).is_err(), "{bad}");
        }
        let ten: String = (0..10).map(|n| format!("&columns=c{n}")).collect();
        assert_eq!(
            parse_export(&pairs(&format!("format=csv{ten}")), now)
                .unwrap()
                .columns
                .len(),
            10
        );
        let eleven: String = (0..11).map(|n| format!("&columns=c{n}")).collect();
        let error = parse_export(&pairs(&format!("format=csv{eleven}")), now).unwrap_err();
        assert!(error.contains("10"), "{error}");
    }

    #[test]
    fn the_sql_holds_no_user_text() {
        let now = Utc::now();
        let params = parse_export(
            &query::query_pairs(Some(
                "format=csv&columns=user.name&columns=http-status&q=%27%3B%20DROP%20TABLE%20logs%3B%20--",
            )),
            now,
        )
        .unwrap();
        let project = Uuid::new_v4();
        let sql = export_sql(project, &params, MAX_EXPORT_ROWS);
        let text = sql.text();
        for user_text in ["user", "name", "http-status", "DROP", &project.to_string()] {
            assert!(!text.contains(user_text), "{user_text} in {text}");
        }
        assert!(text.starts_with("SELECT dt, "), "{text}");
        assert!(text.contains(" AS c0") && text.contains(" AS c1") && !text.contains(" AS c2"));
        assert!(
            text.contains(" ORDER BY dt DESC, id DESC LIMIT {p"),
            "{text}"
        );
        let values: Vec<String> = sql.params().into_iter().map(|(_, v)| v).collect();
        assert!(values.contains(&"user".to_string()) && values.contains(&"100000".to_string()));

        let ndjson = ExportParams {
            format: ExportFormat::Ndjson,
            ..params
        };
        let text = export_sql(project, &ndjson, 7).text().to_string();
        assert!(text.starts_with("SELECT id, "), "{text}");
        assert!(!text.contains(" AS c0"), "NDJSON carries no picked columns");
    }

    #[test]
    fn file_names_carry_no_user_text() {
        let to = DateTime::parse_from_rfc3339("2026-10-19T08:05:03.250Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            file_name(ExportFormat::Csv, to),
            "nami-logs-20261019T080503Z.csv"
        );
        assert_eq!(
            file_name(ExportFormat::Ndjson, to),
            "nami-logs-20261019T080503Z.ndjson"
        );
    }

    #[test]
    fn the_default_limits_are_the_spec_values() {
        assert_eq!(
            ExportLimits::default(),
            ExportLimits {
                rows: 100_000,
                bytes: 512 * 1024 * 1024,
                line_bytes: 4 * 1024 * 1024,
                deadline: Duration::from_secs(150),
            }
        );
    }

    /// A request timeout is the time limit, not "try again"; and the
    /// request outlives the task's deadline, so the deadline comes first.
    #[tokio::test]
    async fn a_request_timeout_is_reported_as_the_time_limit() {
        assert_eq!(
            crate::core::clickhouse::STREAM_REQUEST_TIMEOUT,
            EXPORT_DEADLINE + Duration::from_secs(5)
        );
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(wiremock::ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
            .mount(&server)
            .await;
        let timeout = reqwest::Client::new()
            .get(server.uri())
            .timeout(Duration::from_millis(50))
            .send()
            .await
            .unwrap_err();
        assert!(timeout.is_timeout());
        assert_eq!(stop_reason(&ClickHouseError::from(timeout)), REASON_TIME);
        assert_eq!(
            stop_reason(&ClickHouseError::LineTooLong { limit: 1 }),
            REASON_ROW_TOO_LARGE
        );
        assert_eq!(
            stop_reason(&ClickHouseError::Server {
                status: 500,
                body: String::new()
            }),
            REASON_INTERRUPTED
        );
    }

    // ---- streaming --------------------------------------------------------

    /// A RowStream over a fake ClickHouse answering `body`.
    async fn rows_of(body: String, max_line: usize) -> (wiremock::MockServer, RowStream) {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
        let ch = ClickHouse::new(&server.uri(), "u", "p", "nami_test_export");
        let rows = ch
            .stream_rows_with("SELECT 1", &[], &[], max_line)
            .await
            .unwrap();
        (server, rows)
    }

    fn params(format: ExportFormat, columns: &[&str]) -> ExportParams {
        let now = Utc::now();
        ExportParams {
            window: Window {
                from: now - chrono::Duration::hours(1),
                to: now,
            },
            filters: LogFilters::default(),
            format,
            columns: columns
                .iter()
                .map(|c| query::parse_path(c).unwrap())
                .collect(),
        }
    }

    /// Runs `stream` to its end and reads the body as the client would:
    /// (text, the body ended with an error, chunk sizes).
    async fn export(
        body: String,
        params: ExportParams,
        sources: HashMap<Uuid, String>,
        limits: ExportLimits,
    ) -> (String, bool, Vec<usize>) {
        let (_server, rows) = rows_of(body, limits.line_bytes).await;
        let (tx, rx) = mpsc::channel(EXPORT_CHANNEL_CHUNKS);
        let aborted = Arc::new(AtomicBool::new(false));
        tokio::spawn(stream(rows, params, sources, limits, tx, aborted.clone()));
        let mut body = AbortableBody::new(rx, aborted);
        let (mut text, mut failed, mut sizes) = (Vec::new(), false, Vec::new());
        while let Some(item) = body.next().await {
            match item {
                Ok(chunk) => {
                    sizes.push(chunk.len());
                    text.extend_from_slice(&chunk);
                }
                Err(_) => failed = true,
            }
        }
        (String::from_utf8(text).unwrap(), failed, sizes)
    }

    /// One line as ClickHouse writes it: the columns in SELECT order (`id`
    /// first), strings JSON-escaped.
    fn ndjson_row(n: usize, message: &str) -> String {
        let text = |v: &str| Value::String(v.to_string()).to_string();
        format!(
            "{{\"id\":\"{}\",\"project_id\":\"{}\",\"source_id\":\"{}\",\"dt\":\"2026-10-19T08:05:03.250Z\",\
             \"level\":\"info\",\"message\":{},\"fields\":{},\"trace_id\":\"\",\"span_id\":\"\",\
             \"pattern\":\"x\",\"pattern_hash\":\"1\",\"retention_days\":30}}",
            Uuid::new_v4(),
            Uuid::nil(),
            Uuid::nil(),
            text(message),
            text(&format!("{{\"n\":{n}}}")),
        )
    }

    /// A CSV-query line: `dt` first, then the fixed columns and `c0..`.
    fn csv_line(
        dt: &str,
        level: &str,
        source: &str,
        message: &str,
        trace: &str,
        columns: &[&str],
    ) -> String {
        let text = |v: &str| Value::String(v.to_string()).to_string();
        let mut line = format!(
            "{{\"dt\":{},\"level\":{},\"source_id\":{},\"message\":{},\"trace_id\":{}",
            text(dt),
            text(level),
            text(source),
            text(message),
            text(trace)
        );
        for (i, value) in columns.iter().enumerate() {
            line.push_str(&format!(",\"c{i}\":{}", text(value)));
        }
        line.push('}');
        line
    }

    #[tokio::test]
    async fn ndjson_rows_pass_verbatim_and_stay_one_json_object_per_line() {
        let awkward = "line\nbreak \"quoted\" \\ back \u{1} ctl \u{2028} 𝄞 {\"id\":1}\r\n";
        let lines = [ndjson_row(0, awkward), ndjson_row(1, "plain")];
        let (text, failed, _) = export(
            format!("{}\n{}\n", lines[0], lines[1]),
            params(ExportFormat::Ndjson, &[]),
            HashMap::new(),
            ExportLimits::default(),
        )
        .await;
        assert!(!failed);
        assert_eq!(text, format!("{}\n{}\n", lines[0], lines[1]));
        let parsed: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["message"], awkward);
    }

    /// Whatever shape a mid-stream ClickHouse error takes, it is never
    /// written: the file ends with the "query failed" trailer instead.
    #[tokio::test]
    async fn a_clickhouse_error_in_the_body_never_lands_in_the_file() {
        let good = ndjson_row(0, "ok");
        let cut = &good[..good.len() / 2];
        for tail in [
            "{\"exception\": \"Code: 241. DB::Exception: SELECT secret FROM x\"}\n".to_string(),
            "__exception__\nabcd\nCode: 241. SELECT secret\n24 abcd\n__exception__\n".to_string(),
            format!("{cut}\n__exception__\nCode: 241. SELECT secret\n"),
            "Code: 241. DB::Exception: Memory limit (SELECT secret)\n".to_string(),
        ] {
            let (text, failed, _) = export(
                format!("{good}\n{tail}"),
                params(ExportFormat::Ndjson, &[]),
                HashMap::new(),
                ExportLimits::default(),
            )
            .await;
            assert!(!failed, "the trailer went out: a clean end");
            assert!(
                !text.contains("secret") && !text.contains("Code:"),
                "{text}"
            );
            let lines: Vec<&str> = text.lines().collect();
            assert_eq!(lines.len(), 2, "{text}");
            assert_eq!(lines[0], good);
            assert_eq!(
                lines[1],
                serde_json::json!({ "_nami_export_error": REASON_QUERY_FAILED }).to_string()
            );
        }
        // The same in CSV.
        let row = csv_line("2026-10-19T08:05:03.250Z", "info", "x", "m", "", &[]);
        let (text, _, _) = export(
            format!("{row}\n{{\"exception\": \"Code: 241. SELECT secret\"}}\n"),
            params(ExportFormat::Csv, &[]),
            HashMap::new(),
            ExportLimits::default(),
        )
        .await;
        assert!(!text.contains("secret"), "{text}");
        assert!(
            text.ends_with(&csv_record(&[&format!(
                "# export stopped early: {REASON_QUERY_FAILED}"
            )])),
            "{text}"
        );
    }

    #[tokio::test]
    async fn csv_has_the_header_names_and_guarded_cells() {
        let source = Uuid::new_v4();
        let gone = Uuid::new_v4();
        let rows = [
            csv_line(
                "2026-10-19T08:05:03.250Z",
                "info",
                &source.to_string(),
                "=cmd|' /C calc'!A0",
                "",
                &["@SUM(1)", "5"],
            ),
            csv_line(
                "2026-10-19T08:05:03.249Z",
                "warn",
                &gone.to_string(),
                "a, \"b\"\nc",
                "t",
                &["", "-2"],
            ),
        ];
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        let (text, failed, _) = export(
            body,
            params(ExportFormat::Csv, &["user.name", "-n"]),
            HashMap::from([(source, "api".to_string())]),
            ExportLimits::default(),
        )
        .await;
        assert!(!failed);
        assert_eq!(
            text,
            format!(
                "\"dt\",\"level\",\"source\",\"message\",\"trace_id\",\"user.name\",\"'-n\"\r\n\
                 \"2026-10-19T08:05:03.250Z\",\"info\",\"api\",\"'=cmd|' /C calc'!A0\",\"\",\"'@SUM(1)\",\"5\"\r\n\
                 \"2026-10-19T08:05:03.249Z\",\"warn\",\"{gone}\",\"a, \"\"b\"\"\nc\",\"t\",\"\",\"'-2\"\r\n"
            )
        );
    }

    #[tokio::test]
    async fn the_byte_cap_ends_the_file_with_a_trailer() {
        let body: String = (0..50).map(|n| ndjson_row(n, "x") + "\n").collect();
        let limits = ExportLimits {
            bytes: 2000,
            ..ExportLimits::default()
        };
        let (text, failed, _) = export(
            body,
            params(ExportFormat::Ndjson, &[]),
            HashMap::new(),
            limits,
        )
        .await;
        assert!(!failed);
        let last = text.lines().last().unwrap();
        assert_eq!(
            last,
            serde_json::json!({ "_nami_export_error": REASON_SIZE }).to_string()
        );
        assert!(text.len() - last.len() - 1 <= 2000, "{}", text.len());
    }

    #[tokio::test]
    async fn the_row_cap_is_a_normal_end() {
        let body: String = (0..5).map(|n| ndjson_row(n, "x") + "\n").collect();
        let limits = ExportLimits {
            rows: 2,
            ..ExportLimits::default()
        };
        let (text, failed, _) = export(
            body,
            params(ExportFormat::Ndjson, &[]),
            HashMap::new(),
            limits,
        )
        .await;
        assert!(!failed);
        assert_eq!(text.lines().count(), 2);
        assert!(!text.contains("_nami_export_error"));
    }

    #[tokio::test]
    async fn an_overlong_line_ends_the_file_with_a_trailer() {
        let body = format!(
            "{}\n{}\n",
            ndjson_row(0, "x"),
            ndjson_row(1, &"y".repeat(5000))
        );
        let limits = ExportLimits {
            line_bytes: 1000,
            ..ExportLimits::default()
        };
        let (text, _, _) = export(
            body,
            params(ExportFormat::Ndjson, &[]),
            HashMap::new(),
            limits,
        )
        .await;
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with(&trailer(ExportFormat::Ndjson, REASON_ROW_TOO_LARGE)));
    }

    /// Big rows go out in pieces of at most one chunk, so the queue between
    /// the task and the body never weighs more than its chunk count allows.
    #[tokio::test]
    async fn chunks_never_pass_the_chunk_size() {
        let body: String = (0..4)
            .map(|n| ndjson_row(n, &"z".repeat(300_000)) + "\n")
            .collect();
        let (text, failed, sizes) = export(
            body.clone(),
            params(ExportFormat::Ndjson, &[]),
            HashMap::new(),
            ExportLimits::default(),
        )
        .await;
        assert!(!failed);
        assert_eq!(text, body);
        assert!(sizes.iter().all(|s| *s <= EXPORT_CHUNK_BYTES), "{sizes:?}");
    }

    /// A client that stays connected but never reads: the task still ends
    /// at the deadline (plus the trailer's grace), and the body is marked to
    /// end with an error, since the trailer could not go out.
    #[tokio::test]
    async fn a_reader_that_stops_cannot_hold_the_task_past_its_deadline() {
        let body: String = (0..2000)
            .map(|n| ndjson_row(n, &"w".repeat(1000)) + "\n")
            .collect();
        let (_server, rows) = rows_of(body, MAX_EXPORT_LINE_BYTES).await;
        let (tx, rx) = mpsc::channel(EXPORT_CHANNEL_CHUNKS);
        let aborted = Arc::new(AtomicBool::new(false));
        let limits = ExportLimits {
            deadline: Duration::from_millis(300),
            ..ExportLimits::default()
        };
        let started = std::time::Instant::now();
        tokio::time::timeout(
            Duration::from_secs(5),
            stream(
                rows,
                params(ExportFormat::Ndjson, &[]),
                HashMap::new(),
                limits,
                tx,
                aborted.clone(),
            ),
        )
        .await
        .expect("the task ends at its deadline");
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(aborted.load(Ordering::Acquire));
        let mut body = AbortableBody::new(rx, aborted);
        let mut failed = false;
        while let Some(item) = body.next().await {
            failed |= item.is_err();
        }
        assert!(failed, "the body ends with an error, not a clean close");
    }

    /// The client went away: the task returns at once.
    #[tokio::test]
    async fn a_gone_client_ends_the_task_at_once() {
        let body: String = (0..2000).map(|n| ndjson_row(n, "x") + "\n").collect();
        let (_server, rows) = rows_of(body, MAX_EXPORT_LINE_BYTES).await;
        let (tx, rx) = mpsc::channel(EXPORT_CHANNEL_CHUNKS);
        drop(rx);
        let aborted = Arc::new(AtomicBool::new(false));
        tokio::time::timeout(
            Duration::from_secs(2),
            stream(
                rows,
                params(ExportFormat::Ndjson, &[]),
                HashMap::new(),
                ExportLimits::default(),
                tx,
                aborted.clone(),
            ),
        )
        .await
        .expect("the task returns");
    }
}
