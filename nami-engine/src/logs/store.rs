//! Log rows in ClickHouse (`logs`, see `clickhouse::MIGRATIONS`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    normalize::{self, LogEvent},
    pattern,
};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    models::source::{IngestSource, MAX_RETENTION_DAYS},
};

pub const LOGS_TABLE: &str = "logs";

/// The smallest retention a source can have; [`MAX_RETENTION_DAYS`] is the
/// largest.
const MIN_RETENTION_DAYS: i32 = 1;

/// Sent with every insert into `logs`:
/// - `async_insert` lets ClickHouse batch the many small ingest requests
///   server-side;
/// - waiting for the flush keeps a 202 meaning "stored", but only for 25 s,
///   below the client's own 30 s timeout: a stalled flush then comes back as
///   a ClickHouse error (`TIMEOUT_EXCEEDED`) instead of a bare client-side
///   timeout on a request whose outcome the client cannot tell;
/// - up to 400 daily partitions per insert (ClickHouse allows 100) fit the
///   367 calendar days `dt` can span (see [`build_row_at`]), so a wide
///   backfill is accepted, while a runaway fan-out, which would create a
///   part per partition, still fails.
pub const INSERT_SETTINGS: [(&str, &str); 4] = [
    ("async_insert", "1"),
    ("wait_for_async_insert", "1"),
    ("wait_for_async_insert_timeout", "25"),
    ("max_partitions_per_insert_block", "400"),
];

/// One row of `logs` as inserted (`pattern_hash` is derived by ClickHouse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    /// Unique, and ordered by creation time under ClickHouse: the `(dt, id)`
    /// tiebreaker. Opaque; see [`new_row_id`].
    pub id: Uuid,
    pub project_id: Uuid,
    pub source_id: Uuid,
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    /// A JSON object.
    pub fields: String,
    pub trace_id: String,
    pub span_id: String,
    pub pattern: String,
    pub retention_days: u16,
}

impl LogRow {
    /// What a row counts against usage and quotas: the UTF-8 length of the
    /// stored message plus fields.
    pub fn stored_bytes(&self) -> u64 {
        (self.message.len() + self.fields.len()) as u64
    }
}

/// A fresh row id that sorts in creation order under ClickHouse.
///
/// ClickHouse compares `UUID` values by their second 8 bytes first. In a
/// UUIDv7 those are random bits (the timestamp and the counter that orders
/// ids within a millisecond sit in the first 8), so ids would break ties in
/// `ORDER BY (project_id, dt, id)` at random. Swapping the two 64-bit halves
/// puts the timestamp and counter where ClickHouse compares first. Row ids
/// are opaque: nothing reads one back as a UUIDv7, they only have to be
/// unique and ordered.
pub fn new_row_id() -> Uuid {
    let (high, low) = Uuid::now_v7().as_u64_pair();
    Uuid::from_u64_pair(low, high)
}

/// The row for `event` from `source`, as of now: see [`build_row_at`].
pub fn build_row(event: LogEvent, source: &IngestSource) -> LogRow {
    build_row_at(event, source, Utc::now())
}

/// The row for `event` from `source` at insert time `now`: a fresh id, the
/// message pattern, the source's current retention (kept within the 1 to 365
/// days a source may have) and a `dt` inside the window around `now`
/// ([`normalize::clamp_to_accepted_range`]).
///
/// Events are plain data and later pipeline stages may rewrite them, so the
/// size caps of [`LogEvent::new`] and the `dt` window are enforced again
/// here: whatever path built the event, a stored row respects them.
pub fn build_row_at(event: LogEvent, source: &IngestSource, now: DateTime<Utc>) -> LogRow {
    let message = normalize::truncate_message(event.message);
    let fields = normalize::cap_fields(event.fields);
    let pattern = pattern::normalize_message(&message);
    LogRow {
        id: new_row_id(),
        project_id: source.project_id,
        source_id: source.id,
        dt: normalize::clamp_to_accepted_range(event.dt, now),
        level: event.level,
        fields: serde_json::Value::Object(fields).to_string(),
        message,
        trace_id: event.trace_id,
        span_id: event.span_id,
        pattern,
        // Within 1..=365, so the cast cannot truncate.
        retention_days: source
            .retention_days
            .clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS) as u16,
    }
}

pub async fn insert_logs(ch: &ClickHouse, rows: &[LogRow]) -> Result<(), ClickHouseError> {
    ch.insert_rows_with(LOGS_TABLE, rows, &INSERT_SETTINGS)
        .await
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn source(retention_days: i32) -> IngestSource {
        IngestSource {
            id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            retention_days,
            daily_byte_quota: None,
        }
    }

    fn now() -> DateTime<Utc> {
        "2026-10-13T12:00:00Z".parse().unwrap()
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn event_at(dt: DateTime<Utc>) -> LogEvent {
        LogEvent::new(
            dt,
            "info",
            "m".to_string(),
            serde_json::Map::new(),
            String::new(),
            String::new(),
        )
    }

    #[test]
    fn row_ids_are_uuid_v7s_with_their_halves_swapped() {
        let ids: Vec<Uuid> = (0..1000).map(|_| new_row_id()).collect();
        for id in &ids {
            let (high, low) = id.as_u64_pair();
            assert_eq!(
                Uuid::from_u64_pair(low, high).get_version_num(),
                7,
                "{id} is not a UUIDv7 with swapped halves"
            );
        }
        // ClickHouse compares the second 8 bytes first, then the first 8.
        let clickhouse_order = |id: &Uuid| {
            let (high, low) = id.as_u64_pair();
            (low, high)
        };
        assert!(
            ids.windows(2)
                .all(|pair| clickhouse_order(&pair[0]) < clickhouse_order(&pair[1])),
            "ids built in sequence sort in that sequence"
        );
    }

    #[test]
    fn dt_is_clamped_into_the_window_whatever_built_the_event() {
        let source = source(14);
        let build = |dt| build_row_at(event_at(dt), &source, now()).dt;
        assert_eq!(
            build(at("1900-01-01T00:00:00Z")),
            now() - normalize::MAX_PAST
        );
        assert_eq!(
            build(at("2200-01-01T00:00:00Z")),
            now() + normalize::MAX_FUTURE
        );
        let inside = at("2026-10-13T11:59:58.250Z");
        assert_eq!(build(inside), inside);
        // An event assembled by hand (the fields are public) with digits below
        // the column's millisecond precision lands on the millisecond grid.
        let by_hand = LogEvent {
            dt: at("2026-10-13T11:59:58.123456789Z"),
            ..event_at(now())
        };
        assert_eq!(
            build_row_at(by_hand, &source, now()).dt,
            at("2026-10-13T11:59:58.123Z")
        );
    }

    #[test]
    fn the_caps_apply_again_to_an_event_edited_after_it_was_built() {
        let mut event = event_at(now());
        event.message = "x".repeat(100 * 1024);
        event
            .fields
            .insert("big".to_string(), json!("y".repeat(300 * 1024)));
        event.fields.insert("keep".to_string(), json!(1));
        let row = build_row_at(event, &source(14), now());

        assert!(
            row.message.len() <= normalize::MAX_MESSAGE_BYTES,
            "{}",
            row.message.len()
        );
        assert!(row.message.ends_with(normalize::TRUNCATION_MARKER));
        assert!(
            row.fields.len() <= normalize::MAX_FIELDS_BYTES,
            "{}",
            row.fields.len()
        );
        let fields: Value = serde_json::from_str(&row.fields).unwrap();
        assert_eq!(fields["keep"], 1);
        assert!(fields.get("big").is_none());
        assert!(fields.get(normalize::FIELDS_TRUNCATED_KEY).is_some());
    }

    #[test]
    fn retention_is_clamped_to_what_a_source_may_have() {
        for (stored, expected) in [
            (1, 1),
            (14, 14),
            (365, 365),
            (0, 1),
            (-5, 1),
            (366, 365),
            (70_000, 365),
            (i32::MAX, 365),
            (i32::MIN, 1),
        ] {
            let row = build_row_at(event_at(now()), &source(stored), now());
            assert_eq!(row.retention_days, expected, "retention {stored}");
        }
    }
}
