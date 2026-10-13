// The ClickHouse `logs` table and row building. Requires
// `docker compose up -d clickhouse`.
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    clickhouse::ClickHouse,
    logs::{
        normalize,
        store::{self, LogRow},
    },
    models::source::IngestSource,
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

fn source() -> IngestSource {
    IngestSource {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        retention_days: 14,
        daily_byte_quota: None,
    }
}

fn row(source: &IngestSource, value: Value, received_at: DateTime<Utc>) -> LogRow {
    store::build_row(
        normalize::from_json(value.as_object().unwrap().clone(), received_at),
        source,
    )
}

fn now_millis() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap()
}

#[test]
fn build_row_carries_the_source_and_the_pattern() {
    let source = source();
    let received = now_millis();
    let built = row(
        &source,
        json!({ "message": "user 42 logged in from 10.0.0.7", "level": "error", "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "k": [1, 2] }),
        received,
    );
    assert_eq!(
        (built.project_id, built.source_id, built.retention_days),
        (source.project_id, source.id, 14)
    );
    assert_eq!(built.dt, received);
    assert_eq!(built.level, "error");
    assert_eq!(built.pattern, "user <num> logged in from <ip>");
    assert_eq!(built.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(built.fields, r#"{"k":[1,2]}"#);
    assert_eq!(
        built.stored_bytes(),
        (built.message.len() + built.fields.len()) as u64
    );
    assert_eq!(built.id.get_version_num(), 7, "time-ordered UUIDv7 ids");
    assert_ne!(built.id, row(&source, json!({}), received).id);
}

#[derive(Debug, Deserialize)]
struct StoredRow {
    #[serde(flatten)]
    row: LogRow,
    pattern_hash: u64,
    expected_hash: u64,
}

#[tokio::test]
async fn rows_round_trip_and_equal_patterns_share_a_hash() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = source();
    let received = now_millis();
    let rows = vec![
        row(
            &source,
            json!({ "dt": (received - Duration::seconds(2)).to_rfc3339(), "message": "user 42 logged in", "level": "info", "user": { "name": "O'Brien \\ \"x\"" } }),
            received,
        ),
        row(
            &source,
            json!({ "dt": (received - Duration::seconds(1)).to_rfc3339(), "message": "user 7 logged in", "level": "warn" }),
            received,
        ),
        row(
            &source,
            json!({ "message": "disk full on /var\nretrying", "level": "error", "span_id": "00f067aa0ba902b7" }),
            received,
        ),
    ];
    store::insert_logs(&ch, &rows).await.unwrap();

    let stored: Vec<StoredRow> = ch
        .query_rows(
            "SELECT id, project_id, source_id, dt, level, message, fields, trace_id, span_id, pattern,
                    retention_days, pattern_hash, cityHash64(pattern) AS expected_hash
             FROM logs WHERE project_id = {project_id:UUID} ORDER BY dt, id",
            &[("project_id", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(
        stored.iter().map(|s| s.row.clone()).collect::<Vec<_>>(),
        rows
    );
    assert!(
        stored.iter().all(|s| s.pattern_hash == s.expected_hash),
        "pattern_hash = cityHash64(pattern)"
    );
    assert_eq!(
        stored[0].pattern_hash, stored[1].pattern_hash,
        "same pattern, same hash"
    );
    assert_ne!(stored[0].pattern_hash, stored[2].pattern_hash);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn a_batch_spanning_more_than_100_daily_partitions_is_accepted() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    // Rows older than their retention are dropped by the TTL on insert.
    let source = IngestSource {
        retention_days: 365,
        ..source()
    };
    let received = now_millis();
    let rows: Vec<LogRow> = (0..150)
        .map(|day| {
            row(
                &source,
                json!({ "dt": (received - Duration::days(day)).to_rfc3339(), "message": "m" }),
                received,
            )
        })
        .collect();
    store::insert_logs(&ch, &rows).await.unwrap();
    let count: Vec<Value> = ch
        .query_rows(
            "SELECT count() AS n, uniqExact(toYYYYMMDD(dt)) AS days FROM logs WHERE project_id = {p:UUID}",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(count, vec![json!({ "n": 150, "days": 150 })]);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn the_logs_table_has_the_spec_schema() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let db = ("db", ch.database().to_string());
    let columns: Vec<Value> = ch
        .query_rows(
            "SELECT name, type, default_kind FROM system.columns
             WHERE database = {db:String} AND table = 'logs' ORDER BY position",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    let expected = [
        ("id", "UUID", ""),
        ("project_id", "UUID", ""),
        ("source_id", "UUID", ""),
        ("dt", "DateTime64(3, 'UTC')", ""),
        ("level", "LowCardinality(String)", ""),
        ("message", "String", ""),
        ("fields", "String", ""),
        ("trace_id", "String", ""),
        ("span_id", "String", ""),
        ("pattern", "String", ""),
        ("pattern_hash", "UInt64", "MATERIALIZED"),
        ("retention_days", "UInt16", ""),
    ];
    assert_eq!(
        columns,
        expected
            .iter()
            .map(|(name, ty, kind)| json!({ "name": name, "type": ty, "default_kind": kind }))
            .collect::<Vec<_>>()
    );
    let indexes: Vec<Value> = ch
        .query_rows(
            "SELECT name, type_full FROM system.data_skipping_indices
             WHERE database = {db:String} AND table = 'logs' ORDER BY name",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    assert_eq!(
        indexes,
        vec![
            json!({ "name": "idx_level", "type_full": "set(16)" }),
            json!({ "name": "idx_message", "type_full": "tokenbf_v1(32768, 3, 0)" }),
            json!({ "name": "idx_pattern", "type_full": "bloom_filter(0.01)" }),
            json!({ "name": "idx_trace", "type_full": "bloom_filter(0.01)" }),
        ]
    );
    let table: Vec<Value> = ch
        .query_rows(
            "SELECT partition_key, sorting_key, create_table_query FROM system.tables
             WHERE database = {db:String} AND name = 'logs'",
            std::slice::from_ref(&db),
        )
        .await
        .unwrap();
    assert_eq!(table[0]["partition_key"], "toYYYYMMDD(dt)");
    assert_eq!(table[0]["sorting_key"], "project_id, dt, id");
    let ddl = table[0]["create_table_query"].as_str().unwrap();
    assert!(
        ddl.contains("TTL toDateTime(dt) + toIntervalDay(retention_days)"),
        "{ddl}"
    );
    ch.drop_database().await.unwrap();
}

/// The insert asks for an async insert that waits for the flush (so 202
/// still means stored) and tolerates wide backfills.
#[tokio::test]
async fn inserts_use_waited_async_inserts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_logs");
    store::insert_logs(
        &ch,
        &[row(&source(), json!({ "message": "m" }), now_millis())],
    )
    .await
    .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let params: Vec<(String, String)> = requests[0]
        .url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    for expected in [
        ("async_insert", "1"),
        ("wait_for_async_insert", "1"),
        ("throw_on_max_partitions_per_insert_block", "0"),
        ("input_format_skip_unknown_fields", "0"),
    ] {
        assert!(
            params
                .iter()
                .any(|(k, v)| k == expected.0 && v == expected.1),
            "missing {expected:?} in {params:?}"
        );
    }
    assert!(
        params
            .iter()
            .any(|(k, v)| k == "param_table" && v == "logs"),
        "{params:?}"
    );
}
