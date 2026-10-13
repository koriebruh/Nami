//! Check history in ClickHouse (`uptime_checks`).

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
