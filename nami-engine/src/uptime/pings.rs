//! Heartbeat ping log in ClickHouse (`heartbeat_pings`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::heartbeat::PingKind;
use crate::core::clickhouse::{ClickHouse, ClickHouseError};

/// One row of `heartbeat_pings`; also the JSON shape of
/// `GET /projects/{id}/heartbeats/{heartbeat_id}/pings`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatPingRow {
    pub heartbeat_id: Uuid,
    pub project_id: Uuid,
    pub received_at: DateTime<Utc>,
    pub kind: PingKind,
    pub exit_code: Option<i32>,
    pub output: String,
}

/// Stores one ping, waiting until ClickHouse has flushed it.
pub async fn insert_ping(ch: &ClickHouse, row: &HeartbeatPingRow) -> Result<(), ClickHouseError> {
    ch.insert_rows_with(
        "heartbeat_pings",
        std::slice::from_ref(row),
        &[("async_insert", "1"), ("wait_for_async_insert", "1")],
    )
    .await
}

/// The heartbeat's most recent pings, newest first.
pub async fn recent_pings(
    ch: &ClickHouse,
    project_id: Uuid,
    heartbeat_id: Uuid,
    limit: u32,
) -> Result<Vec<HeartbeatPingRow>, ClickHouseError> {
    ch.query_rows(
        "SELECT heartbeat_id, project_id, received_at, kind, exit_code, output
         FROM heartbeat_pings
         WHERE project_id = {project_id:UUID} AND heartbeat_id = {heartbeat_id:UUID}
         ORDER BY received_at DESC
         LIMIT {limit:UInt32}",
        &[
            ("project_id", project_id.to_string()),
            ("heartbeat_id", heartbeat_id.to_string()),
            ("limit", limit.to_string()),
        ],
    )
    .await
}
