//! Log search (newest first, keyset-paginated) and the volume histogram.
//! Both are always scoped to one project (bound from the path) and run with
//! the ClickHouse client's server-side time limit (`query_rows`).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::{
    LogsState,
    query::{self, Cursor, ERROR_LEVELS_SQL, LogFilters, SearchParams, Sql, Window},
};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    error::AppError,
};

/// The most characters of a message a list row (search page, live tail)
/// carries; the whole message is read with [`entry`].
pub const LIST_MESSAGE_CHARS: usize = 4096;

/// The largest stored `fields` text a list row carries; a larger one is
/// replaced by [`LIST_FIELDS_OMITTED`] and read whole with [`entry`].
pub const LIST_FIELDS_BYTES: usize = 8192;

/// The `fields` of a list row whose stored fields are over
/// [`LIST_FIELDS_BYTES`].
pub const LIST_FIELDS_OMITTED: &str =
    r#"{"_nami_omitted":"fields over 8 KiB are not listed: open the entry"}"#;

/// The columns of a list row, in [`LogEntry`] order. The message is cut to
/// [`LIST_MESSAGE_CHARS`] and large fields are left out
/// ([`LIST_FIELDS_BYTES`]), with `truncated` set, so what a page or a tail
/// poll weighs does not depend on what senders stored: a stored row may hold
/// 64 KiB of message and 256 KiB of fields, and escaping can make that six
/// times larger on the wire. The aliases differ from the column names on
/// purpose: ClickHouse resolves an alias everywhere in the query, so a
/// `message` alias would make the filters match the cut message.
///
/// The numbers are literals (the text is a `&'static str`); a test pins them
/// to the constants.
pub const ENTRY_COLUMNS: &str = "SELECT id, source_id, dt, level, \
     substringUTF8(message, 1, 4096) AS list_message, \
     if(length(fields) > 8192, \
        '{\"_nami_omitted\":\"fields over 8 KiB are not listed: open the entry\"}', fields) \
        AS list_fields, \
     trace_id, span_id, toString(pattern_hash) AS pattern_hash, \
     toBool(lengthUTF8(message) > 4096 OR length(fields) > 8192) AS truncated FROM logs";

/// The columns of one whole row ([`entry`]).
pub const FULL_COLUMNS: &str = "SELECT id, source_id, dt, level, message, fields, trace_id, span_id, \
     toString(pattern_hash) AS pattern_hash FROM logs";

/// Extra settings of every log read: ClickHouse refuses a result larger than
/// this (in memory) rather than building it, and the client refuses a
/// response body over [`crate::clickhouse::MAX_RESPONSE_BYTES`] as it reads
/// it. With list rows cut ([`ENTRY_COLUMNS`]) and at most
/// [`query::MAX_LIMIT`] rows a page, neither is reached by a legitimate query.
pub const READ_SETTINGS: [(&str, &str); 2] = [
    ("max_result_bytes", "33554432"),
    ("result_overflow_mode", "throw"),
];

/// Concurrent searches and histograms one user may have running; the next is
/// refused with 429. A page load takes two (search and histogram).
pub const MAX_QUERIES_PER_USER: usize = 4;

/// One log row as the API returns it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LogEntry {
    pub id: Uuid,
    pub source_id: Uuid,
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    /// The stored JSON object.
    pub fields: Value,
    pub trace_id: String,
    pub span_id: String,
    /// Decimal string: a UInt64 does not fit a JavaScript number.
    pub pattern_hash: String,
    /// The message was cut or the fields left out (list rows only, see
    /// [`ENTRY_COLUMNS`]): [`entry`] returns the whole row.
    pub truncated: bool,
}

#[derive(Debug, Deserialize)]
pub struct RawEntry {
    id: Uuid,
    source_id: Uuid,
    dt: DateTime<Utc>,
    level: String,
    #[serde(alias = "list_message")]
    message: String,
    #[serde(alias = "list_fields")]
    fields: String,
    trace_id: String,
    span_id: String,
    pattern_hash: String,
    #[serde(default)]
    truncated: bool,
}

impl From<RawEntry> for LogEntry {
    fn from(raw: RawEntry) -> Self {
        LogEntry {
            id: raw.id,
            source_id: raw.source_id,
            dt: raw.dt,
            level: raw.level,
            message: raw.message,
            fields: serde_json::from_str(&raw.fields).unwrap_or(Value::String(raw.fields)),
            trace_id: raw.trace_id,
            span_id: raw.span_id,
            pattern_hash: raw.pattern_hash,
            truncated: raw.truncated,
        }
    }
}

impl LogEntry {
    pub fn cursor(&self) -> Cursor {
        Cursor {
            dt_ms: self.dt.timestamp_millis(),
            id: self.id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SearchPage {
    pub logs: Vec<LogEntry>,
    /// Pass back as `cursor` for the next (older) page; `null` on the last.
    pub next_cursor: Option<String>,
}

/// ClickHouse errors a user's input can cause become 400s with a useful
/// message; anything else stays a generic 500 (logged).
pub fn user_error(error: ClickHouseError) -> AppError {
    match &error {
        ClickHouseError::Server { body, .. } if body.contains("CANNOT_COMPILE_REGEXP") => {
            AppError::Validation("a filter regex is not valid RE2 syntax".to_string())
        }
        ClickHouseError::Server { body, .. } if body.contains("TOO_MANY_ROWS_OR_BYTES") => {
            AppError::Validation(too_large_message())
        }
        ClickHouseError::ResponseTooLarge { .. } => {
            tracing::warn!("a log query result passed the client's response cap");
            AppError::Validation(too_large_message())
        }
        ClickHouseError::Server { body, .. } if body.contains("TIMEOUT_EXCEEDED") => {
            AppError::Validation(
                "the query took too long: narrow the time window or add filters".to_string(),
            )
        }
        _ => AppError::ClickHouse(error),
    }
}

fn too_large_message() -> String {
    "the result is too large: narrow the time window or add filters".to_string()
}

/// One page, newest first: `(dt, id)` descending, strictly after the cursor.
pub async fn search(
    ch: &ClickHouse,
    project_id: Uuid,
    params: &SearchParams,
) -> Result<SearchPage, AppError> {
    let mut sql = Sql::new();
    sql.push(ENTRY_COLUMNS);
    query::push_where(&mut sql, project_id, &params.filters);
    query::push_window(&mut sql, &params.window);
    if let Some(cursor) = params.cursor {
        sql.push(" AND (dt, id) < (fromUnixTimestamp64Milli(")
            .value("Int64", cursor.dt_ms.to_string())
            .push(", 'UTC'), ")
            .value("UUID", cursor.id.to_string())
            .push(")");
    }
    // One extra row tells whether another page exists.
    sql.push(" ORDER BY dt DESC, id DESC LIMIT ")
        .value("UInt32", (params.limit + 1).to_string());
    let rows: Vec<RawEntry> = ch
        .query_rows_with(sql.text(), &sql.params(), &READ_SETTINGS)
        .await
        .map_err(user_error)?;
    let mut logs: Vec<LogEntry> = rows.into_iter().map(LogEntry::from).collect();
    let next_cursor = if logs.len() > params.limit as usize {
        logs.truncate(params.limit as usize);
        logs.last().map(|entry| entry.cursor().encode())
    } else {
        None
    };
    Ok(SearchPage { logs, next_cursor })
}

/// One whole row of the project (message and fields uncut), found by its
/// `dt` (part of the sort key, so the lookup reads one granule) and id;
/// `None` when the project has no such row.
pub async fn entry(
    ch: &ClickHouse,
    project_id: Uuid,
    id: Uuid,
    dt: DateTime<Utc>,
) -> Result<Option<LogEntry>, AppError> {
    let mut sql = Sql::new();
    sql.push(FULL_COLUMNS)
        .push(" WHERE project_id = ")
        .value("UUID", project_id.to_string())
        .push(" AND dt = fromUnixTimestamp64Milli(")
        .value("Int64", dt.timestamp_millis().to_string())
        .push(", 'UTC') AND id = ")
        .value("UUID", id.to_string())
        .push(" LIMIT 1");
    let rows: Vec<RawEntry> = ch
        .query_rows_with(sql.text(), &sql.params(), &READ_SETTINGS)
        .await
        .map_err(user_error)?;
    Ok(rows.into_iter().next().map(LogEntry::from))
}

/// How many searches and histograms each user has running.
#[derive(Debug, Default)]
pub struct QuerySlots {
    per_user: Mutex<HashMap<Uuid, usize>>,
}

/// One running search or histogram; dropping it (the request finished or
/// its client went away) gives the slot back.
#[derive(Debug)]
pub struct QueryPermit {
    logs: Arc<LogsState>,
    user_id: Uuid,
}

impl QuerySlots {
    /// Searches and histograms `user_id` has running.
    pub fn open(&self, user_id: Uuid) -> usize {
        let counts = self.per_user.lock().unwrap_or_else(PoisonError::into_inner);
        counts.get(&user_id).copied().unwrap_or(0)
    }
}

impl QueryPermit {
    /// A slot for `user_id`, or `RateLimited` (429) when the user already has
    /// [`MAX_QUERIES_PER_USER`] running.
    pub fn acquire(logs: &Arc<LogsState>, user_id: Uuid) -> Result<QueryPermit, AppError> {
        Self::acquire_with(logs, user_id, MAX_QUERIES_PER_USER)
    }

    pub fn acquire_with(
        logs: &Arc<LogsState>,
        user_id: Uuid,
        max: usize,
    ) -> Result<QueryPermit, AppError> {
        let mut counts = logs
            .query_slots
            .per_user
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = counts.entry(user_id).or_insert(0);
        if *count >= max {
            return Err(AppError::RateLimited);
        }
        *count += 1;
        Ok(QueryPermit {
            logs: logs.clone(),
            user_id,
        })
    }
}

impl Drop for QueryPermit {
    fn drop(&mut self) {
        let mut counts = self
            .logs
            .query_slots
            .per_user
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = counts.get_mut(&self.user_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.user_id);
            }
        }
    }
}

/// Bucket sizes the histogram picks from (seconds).
pub const HISTOGRAM_STEPS: [i64; 18] = [
    1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400,
];
pub const TARGET_BUCKETS: i64 = 60;

/// The smallest step giving at most [`TARGET_BUCKETS`] buckets.
pub fn histogram_step(window: &Window) -> i64 {
    let span = (window.to - window.from).num_milliseconds().max(1);
    HISTOGRAM_STEPS
        .iter()
        .copied()
        .find(|step| span <= step * 1000 * TARGET_BUCKETS)
        .unwrap_or(86_400)
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Histogram {
    pub step_seconds: i64,
    pub total: u64,
    pub buckets: Vec<HistogramBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistogramBucket {
    pub start: DateTime<Utc>,
    pub count: u64,
    /// Rows at `error` level or above.
    pub errors: u64,
    /// Rows at `warn` level.
    pub warnings: u64,
}

#[derive(Debug, Deserialize)]
struct BucketRow {
    bucket_ms: i64,
    count: u64,
    errors: u64,
    warnings: u64,
}

/// Row counts per bucket over the window, every bucket present (zeros
/// included), oldest first.
pub async fn histogram(
    ch: &ClickHouse,
    project_id: Uuid,
    window: &Window,
    filters: &LogFilters,
) -> Result<Histogram, AppError> {
    let step_seconds = histogram_step(window);
    let step_ms = step_seconds * 1000;
    let mut sql = Sql::new();
    let step = sql.bind(step_ms.to_string());
    sql.push("SELECT intDiv(toUnixTimestamp64Milli(dt), ")
        .placeholder(step, "Int64")
        .push(") * ")
        .placeholder(step, "Int64")
        .push(" AS bucket_ms, count() AS count, countIf(level IN ")
        .push(ERROR_LEVELS_SQL)
        .push(") AS errors, countIf(level = 'warn') AS warnings FROM logs");
    query::push_where(&mut sql, project_id, filters);
    query::push_window(&mut sql, window);
    sql.push(" GROUP BY bucket_ms ORDER BY bucket_ms");
    let rows: Vec<BucketRow> = ch
        .query_rows_with(sql.text(), &sql.params(), &READ_SETTINGS)
        .await
        .map_err(user_error)?;

    let first = window.from.timestamp_millis().div_euclid(step_ms) * step_ms;
    let last = window.to.timestamp_millis();
    let mut buckets = Vec::new();
    let mut found = rows.into_iter().peekable();
    let mut start = first;
    while start <= last {
        let row = found.next_if(|row| row.bucket_ms == start);
        buckets.push(HistogramBucket {
            start: DateTime::from_timestamp_millis(start).unwrap_or(window.from),
            count: row.as_ref().map_or(0, |r| r.count),
            errors: row.as_ref().map_or(0, |r| r.errors),
            warnings: row.as_ref().map_or(0, |r| r.warnings),
        });
        start += step_ms;
    }
    let total = buckets.iter().map(|bucket| bucket.count).sum();
    Ok(Histogram {
        step_seconds,
        total,
        buckets,
    })
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn window(minutes: i64) -> Window {
        let to: DateTime<Utc> = "2026-10-13T12:00:00Z".parse().unwrap();
        Window {
            from: to - Duration::minutes(minutes),
            to,
        }
    }

    #[test]
    fn steps_target_about_60_buckets() {
        assert_eq!(histogram_step(&window(15)), 15);
        assert_eq!(histogram_step(&window(60)), 60);
        assert_eq!(histogram_step(&window(6 * 60)), 600);
        assert_eq!(histogram_step(&window(24 * 60)), 1800);
        assert_eq!(histogram_step(&window(7 * 24 * 60)), 10_800);
        assert_eq!(histogram_step(&window(31 * 24 * 60)), 86_400);
        assert_eq!(
            histogram_step(&Window {
                from: window(1).to,
                to: window(1).to + Duration::milliseconds(1)
            }),
            1
        );
    }

    #[test]
    fn the_list_columns_use_the_documented_limits() {
        let squeezed: String = ENTRY_COLUMNS
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(squeezed.contains(&format!("substringUTF8(message, 1, {LIST_MESSAGE_CHARS})")));
        assert!(squeezed.contains(&format!("lengthUTF8(message) > {LIST_MESSAGE_CHARS}")));
        assert_eq!(
            squeezed
                .matches(&format!("length(fields) > {LIST_FIELDS_BYTES}"))
                .count(),
            2
        );
        assert!(
            squeezed.contains(&format!("'{LIST_FIELDS_OMITTED}'")),
            "the marker in the SQL is LIST_FIELDS_OMITTED"
        );
        let marker: Value = serde_json::from_str(LIST_FIELDS_OMITTED).unwrap();
        assert!(marker.is_object());
        // The cut columns never shadow the stored ones the filters read.
        assert!(!squeezed.contains(" AS message") && !squeezed.contains(" AS fields"));
    }

    #[test]
    fn query_slots_cap_each_user_and_release_on_drop() {
        let logs = Arc::new(LogsState::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = QueryPermit::acquire_with(&logs, a, 2).unwrap();
        let _second = QueryPermit::acquire_with(&logs, a, 2).unwrap();
        assert!(matches!(
            QueryPermit::acquire_with(&logs, a, 2),
            Err(AppError::RateLimited)
        ));
        let _other = QueryPermit::acquire_with(&logs, b, 2).expect("another user is not affected");
        drop(first);
        assert_eq!(logs.query_slots.open(a), 1);
        let _again = QueryPermit::acquire_with(&logs, a, 2).unwrap();
    }

    #[test]
    fn regex_and_timeout_errors_become_400s_and_others_stay_500s() {
        let server = |body: &str| ClickHouseError::Server {
            status: 500,
            body: body.to_string(),
        };
        assert!(matches!(
            user_error(server("Code: 427 ... (CANNOT_COMPILE_REGEXP)")),
            AppError::Validation(_)
        ));
        assert!(matches!(
            user_error(server("Code: 159 ... (TIMEOUT_EXCEEDED)")),
            AppError::Validation(_)
        ));
        assert!(matches!(
            user_error(server("Code: 396 ... (TOO_MANY_ROWS_OR_BYTES)")),
            AppError::Validation(_)
        ));
        assert!(matches!(
            user_error(ClickHouseError::ResponseTooLarge { limit: 1 }),
            AppError::Validation(_)
        ));
        assert!(matches!(
            user_error(server("Code: 60 ... (UNKNOWN_TABLE)")),
            AppError::ClickHouse(_)
        ));
    }
}
