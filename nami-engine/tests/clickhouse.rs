// Requires `docker compose up -d clickhouse` (user/password nami/nami).
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    clickhouse::{ClickHouse, ClickHouseError},
    state::AppState,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct CheckRow {
    monitor_id: Uuid,
    project_id: Uuid,
    checked_at: DateTime<Utc>,
    status: String,
    status_code: Option<u16>,
    response_time_ms: Option<u32>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Count {
    n: u64,
}

/// `checked_at` is `DateTime64(3)`: sub-millisecond precision is truncated
/// on insert, so build values that are already whole milliseconds.
fn now_millis() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

fn row(monitor_id: Uuid, checked_at: DateTime<Utc>, error: Option<&str>) -> CheckRow {
    CheckRow {
        monitor_id,
        project_id: Uuid::new_v4(),
        checked_at,
        status: if error.is_some() { "down" } else { "up" }.to_string(),
        status_code: if error.is_some() { None } else { Some(200) },
        response_time_ms: if error.is_some() { None } else { Some(42) },
        error: error.map(str::to_string),
    }
}

#[tokio::test]
async fn insert_then_query_round_trips_uuids_datetimes_and_nulls() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let monitor_id = Uuid::new_v4();
    let t0 = now_millis();
    let rows = vec![
        row(monitor_id, t0, None),
        row(monitor_id, t0 + Duration::seconds(30), Some("timeout after 10s")),
        row(Uuid::new_v4(), t0, None), // another monitor: must not come back
    ];
    ch.insert_rows("uptime_checks", &rows).await.unwrap();

    let fetched: Vec<CheckRow> = ch
        .query_rows(
            "SELECT monitor_id, project_id, checked_at, status, status_code, response_time_ms, error
             FROM uptime_checks
             WHERE monitor_id = {monitor_id:UUID}
             ORDER BY checked_at",
            &[("monitor_id", monitor_id.to_string())],
        )
        .await
        .unwrap();

    assert_eq!(fetched, rows[..2].to_vec());
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn parameter_values_are_matched_literally_never_executed() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    // Quotes, a backslash sequence that TSV-unescaping would mangle
    // (`\n`, `\f`), a raw tab and newline, an embedded statement, and a
    // placeholder-looking fragment.
    let hostile = "it's \"quoted\" C:\\new\\folder\ttab\nline'; DROP TABLE uptime_checks; -- {error:String}";
    let stored = row(Uuid::new_v4(), now_millis(), Some(hostile));
    ch.insert_rows("uptime_checks", &[stored]).await.unwrap();

    let count = |value: String| {
        let ch = ch.clone();
        async move {
            let rows: Vec<Count> = ch
                .query_rows(
                    "SELECT count() AS n FROM uptime_checks WHERE error = {error:String}",
                    &[("error", value)],
                )
                .await
                .unwrap();
            rows[0].n
        }
    };

    assert_eq!(count(hostile.to_string()).await, 1, "value must round-trip byte-for-byte");
    assert_eq!(
        count(hostile.replace("C:\\new", "C:new")).await,
        0,
        "a near-miss must not match: the value is compared, not reinterpreted"
    );
    let all: Vec<Count> = ch
        .query_rows("SELECT count() AS n FROM uptime_checks", &[])
        .await
        .unwrap();
    assert_eq!(all[0].n, 1, "the embedded DROP TABLE never ran");
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn non_2xx_responses_carry_the_clickhouse_error_text() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let error = ch
        .query_rows::<serde_json::Value>("SELEC 1", &[])
        .await
        .unwrap_err();
    match error {
        ClickHouseError::Server { status, body } => {
            assert!((400..600).contains(&status), "status was {status}");
            assert!(body.contains("SYNTAX_ERROR"), "body was {body:?}");
        }
        other => panic!("expected ClickHouseError::Server, got {other:?}"),
    }
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn migrate_is_idempotent() {
    let ch = ClickHouse::create_test_database().await.unwrap(); // first run
    ch.migrate().await.unwrap(); // second run must be a no-op, not an error
    let tables: Vec<serde_json::Value> = ch
        .query_rows(
            "SELECT name FROM system.tables WHERE database = {db:String}",
            &[("db", ch.database().to_string())],
        )
        .await
        .unwrap();
    assert_eq!(tables, vec![serde_json::json!({ "name": "uptime_checks" })]);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
#[should_panic(expected = "drop_database only drops")]
async fn drop_database_refuses_anything_but_test_databases() {
    let ch = ClickHouse::new("http://localhost:8123", "nami", "nami", "nami");
    let _ = ch.drop_database().await;
}

#[sqlx::test]
async fn app_state_for_test_never_points_at_the_real_database(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool);
    assert_ne!(state.clickhouse.database(), state.config.clickhouse_database);
    assert!(!state.clickhouse.database().starts_with("nami_test_"));
}
