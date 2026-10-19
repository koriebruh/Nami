//! Check history in ClickHouse (`uptime_checks`).

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::checker::{CheckOutcome, CheckStatus};
use crate::{
    clickhouse::{ClickHouse, ClickHouseError},
    models::monitor::Monitor,
};

/// One row of `uptime_checks`; also the JSON shape of
/// `GET /projects/{id}/monitors/{monitor_id}/checks`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UptimeCheckRow {
    pub monitor_id: Uuid,
    pub project_id: Uuid,
    pub checked_at: DateTime<Utc>,
    pub status: CheckStatus,
    pub status_code: Option<u16>,
    pub response_time_ms: Option<u32>,
    pub error: Option<String>,
}

impl UptimeCheckRow {
    pub fn from_outcome(monitor: &Monitor, outcome: &CheckOutcome) -> Self {
        UptimeCheckRow {
            monitor_id: monitor.id,
            project_id: monitor.project_id,
            checked_at: outcome.checked_at,
            status: outcome.status,
            status_code: outcome.status_code,
            response_time_ms: outcome.response_time_ms,
            error: outcome.error.clone(),
        }
    }
}

pub async fn insert_checks(
    ch: &ClickHouse,
    rows: &[UptimeCheckRow],
) -> Result<(), ClickHouseError> {
    ch.insert_rows("uptime_checks", rows).await
}

/// Buckets in the stats series: 48 x 30 minutes, which covers 23.5 to 24
/// hours (see [`monitor_stats`]).
pub const SERIES_BUCKETS: i64 = 48;
/// Length of a series bucket. The series query takes it as a bound parameter,
/// so the SQL and the Rust bucket arithmetic cannot drift apart.
pub const BUCKET_SECONDS: i64 = 30 * 60;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorStats {
    pub uptime_24h: Option<f64>,
    pub uptime_7d: Option<f64>,
    pub uptime_30d: Option<f64>,
    pub avg_response_ms_24h: Option<f64>,
    pub series: Vec<SeriesBucket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SeriesBucket {
    pub bucket_start: DateTime<Utc>,
    pub avg_response_ms: Option<f64>,
    pub up_ratio: Option<f64>,
}

fn ratio(up: u64, total: u64) -> Option<f64> {
    (total > 0).then(|| up as f64 / total as f64)
}

/// The text ClickHouse parses for an `Array(UUID)` query parameter:
/// `['<uuid>','<uuid>']`, UUIDs single-quoted. This builds a parameter
/// *value*, never SQL text, and a `Uuid` renders as hex digits and dashes
/// only, so nothing in it can break out of its quotes.
fn uuid_array_param(ids: &[Uuid]) -> String {
    let quoted: Vec<String> = ids.iter().map(|id| format!("'{id}'")).collect();
    format!("[{}]", quoted.join(","))
}

/// 24-hour uptime of the given monitors of one project, from one grouped
/// query. `monitor_ids` are the project's own monitor ids, which the caller
/// has from Postgres: filtering on them lets ClickHouse use the
/// `(monitor_id, checked_at)` sort key instead of reading every check of the
/// project. `project_id` still filters too, so an id that belongs to another
/// project returns nothing. Monitors with no checks in the window are absent
/// from the map; with no ids no query is sent.
pub async fn uptime_24h_by_monitor(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_ids: &[Uuid],
    now: DateTime<Utc>,
) -> Result<HashMap<Uuid, f64>, ClickHouseError> {
    #[derive(Deserialize)]
    struct Row {
        monitor_id: Uuid,
        up: u64,
        total: u64,
    }
    if monitor_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows: Vec<Row> = ch
        .query_rows(
            "SELECT monitor_id, countIf(status = 'up') AS up, count() AS total
             FROM uptime_checks
             WHERE project_id = {project_id:UUID}
               AND monitor_id IN {ids:Array(UUID)}
               AND checked_at >= fromUnixTimestamp64Milli({since_ms:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')
             GROUP BY monitor_id",
            &[
                ("project_id", project_id.to_string()),
                ("ids", uuid_array_param(monitor_ids)),
                (
                    "since_ms",
                    (now - chrono::Duration::hours(24))
                        .timestamp_millis()
                        .to_string(),
                ),
                ("now_ms", now.timestamp_millis().to_string()),
            ],
        )
        .await?;
    Ok(rows
        .into_iter()
        .filter_map(|row| ratio(row.up, row.total).map(|uptime| (row.monitor_id, uptime)))
        .collect())
}

/// The monitor's most recent checks, newest first.
pub async fn recent_checks(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_id: Uuid,
    limit: u32,
) -> Result<Vec<UptimeCheckRow>, ClickHouseError> {
    ch.query_rows(
        "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms, error
         FROM uptime_checks
         WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
         ORDER BY checked_at DESC
         LIMIT {limit:UInt32}",
        &[
            ("project_id", project_id.to_string()),
            ("monitor_id", monitor_id.to_string()),
            ("limit", limit.to_string()),
        ],
    )
    .await
}

/// Most rows [`checks_between`] returns in one query. At the 30 s minimum
/// interval one day holds 2880 checks; a caller that gets exactly this many
/// rows continues after the last one (see `uptime::export`).
pub const CHECKS_BETWEEN_LIMIT: u32 = 20_000;

/// The monitor's checks with `from <= checked_at < to`, oldest first, at most
/// [`CHECKS_BETWEEN_LIMIT`] of them. `error` is cut to the checker's
/// `MAX_ERROR_CHARS` here too, so one result stays far below the client's
/// response cap (`clickhouse::MAX_RESPONSE_BYTES`) whatever a row holds.
pub async fn checks_between(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_id: Uuid,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<Vec<UptimeCheckRow>, ClickHouseError> {
    ch.query_rows(
        "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms,
                leftUTF8(error, {max_error:UInt32}) AS error
         FROM uptime_checks
         WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
           AND checked_at >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
           AND checked_at < fromUnixTimestamp64Milli({to_ms:Int64}, 'UTC')
         ORDER BY checked_at ASC
         LIMIT {limit:UInt32}",
        &[
            ("project_id", project_id.to_string()),
            ("monitor_id", monitor_id.to_string()),
            ("max_error", super::checker::MAX_ERROR_CHARS.to_string()),
            ("from_ms", from.timestamp_millis().to_string()),
            ("to_ms", to.timestamp_millis().to_string()),
            ("limit", CHECKS_BETWEEN_LIMIT.to_string()),
        ],
    )
    .await
}

/// Uptime over 24 h / 7 d / 30 d, 24 h average response time, and a 24 h
/// series of [`SERIES_BUCKETS`] half-hour buckets ending with the bucket
/// that contains `now`. Buckets without checks are present with nulls, so a
/// chart always gets a stable x-axis. `now` is a parameter so tests can pin
/// the bucket boundaries.
///
/// The series starts on a bucket boundary, so it covers between 23.5 and 24
/// hours (the last bucket is partial): its counts need not add up to the
/// rolling 24 h window behind `uptime_24h`.
pub async fn monitor_stats(
    ch: &ClickHouse,
    project_id: Uuid,
    monitor_id: Uuid,
    now: DateTime<Utc>,
) -> Result<MonitorStats, ClickHouseError> {
    #[derive(Deserialize, Default)]
    struct Totals {
        up_24h: u64,
        total_24h: u64,
        up_7d: u64,
        total_7d: u64,
        up_30d: u64,
        total_30d: u64,
        avg_response_ms_24h: Option<f64>,
    }
    #[derive(Deserialize)]
    struct BucketRow {
        bucket: i64,
        avg_response_ms: Option<f64>,
        up: u64,
        total: u64,
    }

    let millis = |t: DateTime<Utc>| t.timestamp_millis().to_string();
    let totals: Vec<Totals> = ch
        .query_rows(
            "SELECT
                 countIf(status = 'up' AND checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS up_24h,
                 countIf(checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS total_24h,
                 countIf(status = 'up' AND checked_at >= fromUnixTimestamp64Milli({since_7d:Int64}, 'UTC')) AS up_7d,
                 countIf(checked_at >= fromUnixTimestamp64Milli({since_7d:Int64}, 'UTC')) AS total_7d,
                 countIf(status = 'up') AS up_30d,
                 count() AS total_30d,
                 avgIf(response_time_ms, checked_at >= fromUnixTimestamp64Milli({since_24h:Int64}, 'UTC')) AS avg_response_ms_24h
             FROM uptime_checks
             WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
               AND checked_at >= fromUnixTimestamp64Milli({since_30d:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')",
            &[
                ("project_id", project_id.to_string()),
                ("monitor_id", monitor_id.to_string()),
                ("since_24h", millis(now - chrono::Duration::hours(24))),
                ("since_7d", millis(now - chrono::Duration::days(7))),
                ("since_30d", millis(now - chrono::Duration::days(30))),
                ("now_ms", millis(now)),
            ],
        )
        .await?;
    let totals = totals.into_iter().next().unwrap_or_default();

    let current_bucket = now.timestamp() - now.timestamp().rem_euclid(BUCKET_SECONDS);
    let first_bucket = current_bucket - (SERIES_BUCKETS - 1) * BUCKET_SECONDS;
    let rows: Vec<BucketRow> = ch
        .query_rows(
            "SELECT toUnixTimestamp(toStartOfInterval(checked_at, toIntervalSecond({bucket_seconds:UInt32}))) AS bucket,
                    avg(response_time_ms) AS avg_response_ms,
                    countIf(status = 'up') AS up,
                    count() AS total
             FROM uptime_checks
             WHERE project_id = {project_id:UUID} AND monitor_id = {monitor_id:UUID}
               AND checked_at >= fromUnixTimestamp64Milli({from_ms:Int64}, 'UTC')
               AND checked_at <= fromUnixTimestamp64Milli({now_ms:Int64}, 'UTC')
             GROUP BY bucket",
            &[
                ("project_id", project_id.to_string()),
                ("monitor_id", monitor_id.to_string()),
                ("bucket_seconds", BUCKET_SECONDS.to_string()),
                ("from_ms", (first_bucket * 1000).to_string()),
                ("now_ms", millis(now)),
            ],
        )
        .await?;
    let by_bucket: HashMap<i64, BucketRow> = rows.into_iter().map(|r| (r.bucket, r)).collect();
    let series = (0..SERIES_BUCKETS)
        .map(|i| {
            let start = first_bucket + i * BUCKET_SECONDS;
            let row = by_bucket.get(&start);
            SeriesBucket {
                bucket_start: DateTime::from_timestamp(start, 0)
                    .expect("bucket start is a valid timestamp"),
                avg_response_ms: row.and_then(|r| r.avg_response_ms),
                up_ratio: row.and_then(|r| ratio(r.up, r.total)),
            }
        })
        .collect();

    Ok(MonitorStats {
        uptime_24h: ratio(totals.up_24h, totals.total_24h),
        uptime_7d: ratio(totals.up_7d, totals.total_7d),
        uptime_30d: ratio(totals.up_30d, totals.total_30d),
        avg_response_ms_24h: totals.avg_response_ms_24h,
        series,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn monitor() -> Monitor {
        Monitor {
            id: Uuid::from_u128(1),
            project_id: Uuid::from_u128(2),
            name: "Checkout API".to_string(),
            check_type: "http".to_string(),
            url: Some("http://example.test/".to_string()),
            host: None,
            port: None,
            http_method: "GET".to_string(),
            keyword: None,
            keyword_mode: "contains".to_string(),
            expected_status_min: 200,
            expected_status_max: 399,
            interval_seconds: 60,
            timeout_seconds: 10,
            confirmation_threshold: 2,
            paused: false,
            current_status: "pending".to_string(),
            consecutive_failures: 0,
            last_checked_at: None,
            next_check_at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
            created_at: DateTime::from_timestamp(1_792_000_000, 0).unwrap(),
            follow_redirects: true,
            verify_ssl: true,
            ..Monitor::default()
        }
    }

    fn outcome(status: CheckStatus) -> CheckOutcome {
        CheckOutcome {
            status,
            status_code: Some(503),
            response_time_ms: Some(42),
            error: Some("expected status 200-399, got 503".to_string()),
            checked_at: DateTime::from_timestamp(1_792_000_123, 0).unwrap(),
        }
    }

    #[test]
    fn a_row_takes_its_ids_from_the_monitor_and_the_rest_from_the_outcome() {
        let row = UptimeCheckRow::from_outcome(&monitor(), &outcome(CheckStatus::Down));
        assert_eq!(
            row,
            UptimeCheckRow {
                monitor_id: Uuid::from_u128(1),
                project_id: Uuid::from_u128(2),
                checked_at: DateTime::from_timestamp(1_792_000_123, 0).unwrap(),
                status: CheckStatus::Down,
                status_code: Some(503),
                response_time_ms: Some(42),
                error: Some("expected status 200-399, got 503".to_string()),
            }
        );
    }

    /// ClickHouse parses an `Array(UUID)` query parameter from text and
    /// rejects UUIDs that are not single-quoted.
    #[test]
    fn a_uuid_list_is_sent_as_a_quoted_clickhouse_array() {
        assert_eq!(
            uuid_array_param(&[Uuid::from_u128(1), Uuid::from_u128(0xabc)]),
            "['00000000-0000-0000-0000-000000000001','00000000-0000-0000-0000-000000000abc']"
        );
        assert_eq!(uuid_array_param(&[]), "[]");
    }

    /// The JSON keys are the `uptime_checks` column names and `status` is an
    /// `Enum8` label: ClickHouse rejects any other key or label.
    #[test]
    fn a_row_serializes_to_the_columns_of_the_uptime_checks_table() {
        let up = CheckOutcome {
            status: CheckStatus::Up,
            status_code: None,
            response_time_ms: None,
            error: None,
            ..outcome(CheckStatus::Up)
        };
        let json = serde_json::to_value(UptimeCheckRow::from_outcome(&monitor(), &up)).unwrap();
        assert_eq!(
            json,
            json!({
                "monitor_id": "00000000-0000-0000-0000-000000000001",
                "project_id": "00000000-0000-0000-0000-000000000002",
                "checked_at": "2026-10-14T17:48:43Z",
                "status": "up",
                "status_code": null,
                "response_time_ms": null,
                "error": null,
            })
        );
    }
}
