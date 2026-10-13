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
/// ClickHouse compares `UUID` values by their second 8 bytes first. A UUIDv7
/// keeps its 48-bit timestamp and the version in the first 8 bytes, and the
/// 42-bit counter that orders ids within a millisecond straddles both
/// halves: its high 12 bits end the first 8 bytes, its other 30 bits follow
/// the variant bits in the second 8, which end in random bits (the uuid
/// crate reseeds the counter at random every millisecond). Compared second
/// half first, ids would sort by those low counter bits and the random bits
/// before the timestamp, so across milliseconds in random order, and break
/// ties in `ORDER BY (project_id, dt, id)`. Swapping the two 64-bit halves
/// puts the timestamp, the version and the high counter bits where
/// ClickHouse compares first and the low counter bits where it compares
/// second, so its comparison equals the generation order. Row ids are
/// opaque: nothing reads one back as a UUIDv7, they only have to be unique
/// and ordered.
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
/// Events are plain data (public fields) and later pipeline stages may
/// rewrite them, so everything [`LogEvent::new`] and `from_json` guarantee is
/// enforced again here, and whatever path built the event, a stored row
/// respects it:
/// - the message and `fields` size caps, and the `dt` window;
/// - `level` is one of the known levels: it goes through
///   [`normalize::level_from_name`] (letter case and aliases are fine) and is
///   `info` when it names none;
/// - `trace_id` is 32 and `span_id` 16 hex characters, lowercase, or empty
///   ([`normalize::checked_id`]): anything else is not stored as an id but
///   put into `fields` under its own name (a field of that name already
///   there stays), where the fields cap applies to it.
pub fn build_row_at(event: LogEvent, source: &IngestSource, now: DateTime<Utc>) -> LogRow {
    let message = normalize::truncate_message(event.message);
    let mut fields = event.fields;
    let trace_id = normalize::checked_id(
        event.trace_id,
        normalize::TRACE_ID_LEN,
        "trace_id",
        &mut fields,
    );
    let span_id = normalize::checked_id(
        event.span_id,
        normalize::SPAN_ID_LEN,
        "span_id",
        &mut fields,
    );
    let fields = normalize::cap_fields(fields);
    let pattern = pattern::normalize_message(&message);
    LogRow {
        id: new_row_id(),
        project_id: source.project_id,
        source_id: source.id,
        dt: normalize::clamp_to_accepted_range(event.dt, now),
        level: normalize::level_from_name(&event.level).to_string(),
        fields: serde_json::Value::Object(fields).to_string(),
        message,
        trace_id,
        span_id,
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

    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const SPAN: &str = "00f067aa0ba902b7";

    fn row_with(level: &str, trace_id: &str, span_id: &str) -> LogRow {
        let event = LogEvent {
            level: level.to_string(),
            trace_id: trace_id.to_string(),
            span_id: span_id.to_string(),
            ..event_at(now())
        };
        build_row_at(event, &source(14), now())
    }

    fn fields_of(row: &LogRow) -> Value {
        serde_json::from_str(&row.fields).unwrap()
    }

    #[test]
    fn a_level_outside_the_known_set_is_mapped_or_becomes_info() {
        for (level, expected) in [
            ("info", "info"),
            ("error", "error"),
            ("fatal", "fatal"),
            ("WARNING", "warn"),
            (" Err ", "error"),
            ("verbose", "info"),
            ("", "info"),
        ] {
            assert_eq!(row_with(level, "", "").level, expected, "{level:?}");
        }
        for level in normalize::LEVELS {
            assert_eq!(row_with(level, "", "").level, level, "{level}");
        }
        // A hand-built event cannot store a level of any length.
        assert_eq!(row_with(&"x".repeat(100_000), "", "").level, "info");
    }

    #[test]
    fn valid_ids_are_kept_in_lowercase_and_empty_ids_stay_empty() {
        let row = row_with("info", TRACE, SPAN);
        assert_eq!((row.trace_id.as_str(), row.span_id.as_str()), (TRACE, SPAN));
        let row = row_with("info", &TRACE.to_uppercase(), &SPAN.to_uppercase());
        assert_eq!((row.trace_id.as_str(), row.span_id.as_str()), (TRACE, SPAN));
        let row = row_with("info", "", "");
        assert_eq!((row.trace_id.as_str(), row.span_id.as_str()), ("", ""));
        assert_eq!(row.fields, "{}");
    }

    #[test]
    fn ids_that_are_not_valid_are_not_stored_as_ids_but_kept_in_fields() {
        for (trace_id, span_id) in [
            ("4bf92f3577b34da6a3ce929d0e0e473", "00f067aa0ba902b"),
            ("4bf92f3577b34da6a3ce929d0e0e47361", "00f067aa0ba902b71"),
            ("00000000000000000000000000000000", "0000000000000000"),
            ("4bf92f3577b34da6a3ce929d0e0e473g", "00f067aa0ba902bz"),
            (" 4bf92f3577b34da6a3ce929d0e0e4736", " 00f067aa0ba902b7"),
            ("trace-1", "span-1"),
        ] {
            let row = row_with("info", trace_id, span_id);
            assert_eq!(
                (row.trace_id.as_str(), row.span_id.as_str()),
                ("", ""),
                "{trace_id} {span_id}"
            );
            let fields = fields_of(&row);
            assert_eq!(fields["trace_id"], trace_id);
            assert_eq!(fields["span_id"], span_id);
        }
    }

    #[test]
    fn an_id_of_any_length_neither_gets_stored_nor_pushes_fields_past_the_cap() {
        let row = row_with("info", &"a".repeat(1024 * 1024), &"b".repeat(100));
        assert_eq!((row.trace_id.as_str(), row.span_id.as_str()), ("", ""));
        assert!(
            row.fields.len() <= normalize::MAX_FIELDS_BYTES,
            "{}",
            row.fields.len()
        );
        let fields = fields_of(&row);
        assert!(
            fields.get("trace_id").is_none(),
            "too big for fields: dropped"
        );
        assert_eq!(fields["span_id"], "b".repeat(100));
        assert!(fields.get(normalize::FIELDS_TRUNCATED_KEY).is_some());
    }

    #[test]
    fn an_invalid_id_does_not_replace_a_field_of_the_same_name() {
        let mut event = event_at(now());
        event.trace_id = "not-an-id".to_string();
        event
            .fields
            .insert("trace_id".to_string(), json!("from the sender"));
        let row = build_row_at(event, &source(14), now());
        assert_eq!(row.trace_id, "");
        assert_eq!(fields_of(&row)["trace_id"], "from the sender");
    }
}
