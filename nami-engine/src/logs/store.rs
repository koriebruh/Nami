//! Log rows in ClickHouse (`logs`, see `clickhouse::MIGRATIONS`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{normalize::LogEvent, pattern};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    models::source::IngestSource,
};

pub const LOGS_TABLE: &str = "logs";

/// Sent with every insert into `logs`: `async_insert` lets ClickHouse batch
/// the many small ingest requests server-side, waiting for the flush keeps
/// a 202 meaning "stored", and a batch whose rows span more than 100 daily
/// partitions (a backfill) is accepted instead of failing.
pub const INSERT_SETTINGS: [(&str, &str); 3] = [
    ("async_insert", "1"),
    ("wait_for_async_insert", "1"),
    ("throw_on_max_partitions_per_insert_block", "0"),
];

/// One row of `logs` as inserted (`pattern_hash` is derived by ClickHouse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogRow {
    /// UUIDv7 generated at ingest: unique, the `(dt, id)` tiebreaker.
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

/// The row for `event` from `source`: a fresh id, the message pattern, and
/// the source's current retention.
pub fn build_row(event: LogEvent, source: &IngestSource) -> LogRow {
    let pattern = pattern::normalize_message(&event.message);
    LogRow {
        id: Uuid::now_v7(),
        project_id: source.project_id,
        source_id: source.id,
        dt: event.dt,
        level: event.level,
        fields: serde_json::Value::Object(event.fields).to_string(),
        message: event.message,
        trace_id: event.trace_id,
        span_id: event.span_id,
        pattern,
        retention_days: u16::try_from(source.retention_days).unwrap_or(u16::MAX),
    }
}

pub async fn insert_logs(ch: &ClickHouse, rows: &[LogRow]) -> Result<(), ClickHouseError> {
    ch.insert_rows_with(LOGS_TABLE, rows, &INSERT_SETTINGS)
        .await
}
