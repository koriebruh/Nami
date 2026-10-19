//! The most frequent message patterns of a window (`pattern_hash` groups
//! rows whose messages have the same shape, see [`super::pattern`]).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::logs::{
    query::search,
    query::{self, LogFilters, Sql, Window},
};
use crate::{core::clickhouse::ClickHouse, core::error::AppError};

pub const TOP_PATTERNS: u32 = 100;

/// `READ_SETTINGS` plus a bound on grouping: past a million distinct
/// patterns in the window, ClickHouse keeps counting the patterns it has and
/// ignores new ones (approximate, not an error).
pub const PATTERN_SETTINGS: [(&str, &str); 4] = [
    ("max_result_bytes", "33554432"),
    ("result_overflow_mode", "throw"),
    ("max_rows_to_group_by", "1000000"),
    ("group_by_overflow_mode", "any"),
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatternCount {
    /// Decimal string: a UInt64 does not fit a JavaScript number.
    pub pattern_hash: String,
    pub pattern: String,
    pub count: u64,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

/// The [`TOP_PATTERNS`] most frequent patterns of the project's rows in the
/// window that match `filters`, most frequent first.
pub async fn top_patterns(
    ch: &ClickHouse,
    project_id: Uuid,
    window: &Window,
    filters: &LogFilters,
) -> Result<Vec<PatternCount>, AppError> {
    let mut sql = Sql::new();
    sql.push(
        "SELECT toString(pattern_hash) AS pattern_hash, any(pattern) AS pattern, count() AS count, \
         min(dt) AS first_seen, max(dt) AS last_seen FROM logs",
    );
    query::push_where(&mut sql, project_id, filters);
    query::push_window(&mut sql, window);
    sql.push(" GROUP BY pattern_hash ORDER BY count DESC, pattern_hash LIMIT ")
        .value("UInt32", TOP_PATTERNS.to_string());
    ch.query_rows_with(sql.text(), &sql.params(), &PATTERN_SETTINGS)
        .await
        .map_err(search::user_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_settings_extend_the_read_settings() {
        assert_eq!(PATTERN_SETTINGS[..2], search::READ_SETTINGS);
        // Grouping stays bounded and approximate past a million patterns (R-P2-10).
        assert_eq!(
            PATTERN_SETTINGS[2..],
            [
                ("max_rows_to_group_by", "1000000"),
                ("group_by_overflow_mode", "any")
            ]
        );
    }
}
