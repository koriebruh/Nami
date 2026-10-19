//! Log search (newest first, keyset-paginated) and the volume histogram.
//! Both are always scoped to one project (bound from the path) and run with
//! the ClickHouse client's server-side time limit (`query_rows`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::query::{self, Cursor, ERROR_LEVELS_SQL, LogFilters, SearchParams, Sql, Window};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    error::AppError,
};

/// The columns every log query returns, in [`LogEntry`] order.
pub const ENTRY_COLUMNS: &str = "SELECT id, source_id, dt, level, message, fields, trace_id, span_id, \
     toString(pattern_hash) AS pattern_hash FROM logs";

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
}

#[derive(Debug, Deserialize)]
pub struct RawEntry {
    id: Uuid,
    source_id: Uuid,
    dt: DateTime<Utc>,
    level: String,
    message: String,
    fields: String,
    trace_id: String,
    span_id: String,
    pattern_hash: String,
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
        ClickHouseError::Server { body, .. } if body.contains("TIMEOUT_EXCEEDED") => {
            AppError::Validation(
                "the query took too long: narrow the time window or add filters".to_string(),
            )
        }
        _ => AppError::ClickHouse(error),
    }
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
        .query_rows(sql.text(), &sql.params())
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
        .query_rows(sql.text(), &sql.params())
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
            user_error(server("Code: 60 ... (UNKNOWN_TABLE)")),
            AppError::ClickHouse(_)
        ));
    }
}
