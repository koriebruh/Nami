// The ClickHouse `logs` table and row building. Requires
// `docker compose up -d clickhouse`.
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    core::clickhouse::{ClickHouse, ClickHouseError},
    logs::{
        normalize,
        store::{self, LogRow},
    },
    models::source::IngestSource,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

fn source() -> IngestSource {
    IngestSource {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        retention_days: 14,
        daily_byte_quota: None,
        updated_at: chrono::Utc::now(),
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
    assert_ne!(built.id, row(&source, json!({}), received).id);
}

/// ClickHouse orders UUIDs by their second 8 bytes first. A plain UUIDv7
/// keeps its timestamp in the first 8 bytes and, in the second 8, the low
/// bits of a counter that is reseeded at random every millisecond, then
/// random bits, so it would break `dt` ties in random order across
/// milliseconds. Row ids are UUIDv7s with the two halves swapped
/// (`store::new_row_id`), so rows built in sequence sort in that sequence.
#[tokio::test]
async fn row_ids_sort_in_build_order_when_dt_ties() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = source();
    let received = now_millis();
    let rows: Vec<LogRow> = (0..300)
        .map(|n| {
            row(
                &source,
                json!({ "dt": received.to_rfc3339(), "message": format!("m{n:03}") }),
                received,
            )
        })
        .collect();
    // Inserted in reverse, so the storage order cannot explain the result.
    let reversed: Vec<LogRow> = rows.iter().rev().cloned().collect();
    store::insert_logs(&ch, &reversed).await.unwrap();

    let built: Vec<String> = rows.iter().map(|r| r.message.clone()).collect();
    let by_id: Vec<Value> = ch
        .query_rows(
            "SELECT message FROM logs WHERE project_id = {p:UUID} ORDER BY id",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    let by_sorting_key: Vec<Value> = ch
        .query_rows(
            "SELECT message FROM logs WHERE project_id = {p:UUID} ORDER BY dt, id",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    for (order, stored) in [("id", by_id), ("dt, id", by_sorting_key)] {
        let messages: Vec<&str> = stored
            .iter()
            .map(|row| row["message"].as_str().unwrap())
            .collect();
        assert_eq!(messages, built, "ORDER BY {order} must be the build order");
    }
    ch.drop_database().await.unwrap();
}

/// `dt` lies inside `[insert time - 365 days, insert time + 1 day]` whatever
/// path built the event. The table's TTL goes through `toDateTime`, which
/// wraps around outside 1970 to 2106 (1900 becomes 2036, 2200 becomes 2063),
/// so a wild timestamp would keep a row decades beyond its retention.
#[test]
fn wild_timestamps_are_clamped_by_build_row() {
    let source = source();
    let event = |dt: &str| {
        normalize::LogEvent::new(
            dt.parse().unwrap(),
            "info",
            "m".to_string(),
            Map::new(),
            String::new(),
            String::new(),
        )
    };
    let before = Utc::now();
    let oldest = store::build_row(event("1900-01-01T00:00:00Z"), &source);
    let newest = store::build_row(event("2200-01-01T00:00:00Z"), &source);
    let after = Utc::now();

    // Both edges sit on the column's millisecond grid, rounded inward: the
    // oldest one up, the newest one down. Each may therefore lie up to 1 ms
    // inside the exact instant, so the slack goes on the rounding side only
    // (for the oldest edge that is the upper bound: warm, `after` is often in
    // the same millisecond as the insert time and the rounding is not).
    let millisecond = Duration::milliseconds(1);
    assert!(
        oldest.dt >= before - normalize::MAX_PAST
            && oldest.dt <= after - normalize::MAX_PAST + millisecond,
        "{} is not insert time - 365 days",
        oldest.dt
    );
    assert!(
        newest.dt >= before + normalize::MAX_FUTURE - millisecond
            && newest.dt <= after + normalize::MAX_FUTURE,
        "{} is not insert time + 1 day",
        newest.dt
    );
}

/// `LogEvent::new` takes the level and the ids as given, so an event built by
/// hand could carry anything. Whatever it carries, the table only ever holds
/// one of the known levels and ids of 32 / 16 hex characters (or empty
/// ones); the raw values are kept in `fields`.
#[tokio::test]
async fn hand_built_events_cannot_store_an_arbitrary_level_or_id() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = source();
    let event = normalize::LogEvent::new(
        Utc::now(),
        &"x".repeat(10_000),
        "m".to_string(),
        Map::new(),
        "y".repeat(10_000),
        "z".repeat(10_000),
    );
    store::insert_logs(&ch, &[store::build_row(event, &source)])
        .await
        .unwrap();

    let stored: Vec<Value> = ch
        .query_rows(
            "SELECT level, trace_id, span_id, length(fields) AS fields_length
             FROM logs WHERE project_id = {p:UUID}",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0]["level"], "info");
    assert_eq!(stored[0]["trace_id"], "");
    assert_eq!(stored[0]["span_id"], "");
    assert!(
        stored[0]["fields_length"].as_u64().unwrap() > 20_000,
        "the raw ids are kept in fields: {}",
        stored[0]
    );
    ch.drop_database().await.unwrap();
}

/// The window of `dt` spans at most 367 calendar days (365 back, today,
/// tomorrow), so the partition guard sits at 400: a wide backfill is
/// accepted, a runaway fan-out still fails instead of piling up parts.
#[tokio::test]
async fn the_partition_guard_fits_the_dt_window_and_stops_a_runaway_fan_out() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let source = IngestSource {
        retention_days: 365,
        ..source()
    };
    let received = now_millis();
    // One row per day straight into the table: nothing clamps `dt` here.
    let days = |count: i64| -> Vec<LogRow> {
        (0..count)
            .map(|day| LogRow {
                dt: received - Duration::days(day),
                ..row(&source, json!({ "message": "m" }), received)
            })
            .collect()
    };
    store::insert_logs(&ch, &days(367)).await.unwrap();
    match store::insert_logs(&ch, &days(401)).await.unwrap_err() {
        ClickHouseError::Server { body, .. } => {
            assert!(body.contains("TOO_MANY_PARTS"), "{body}");
        }
        other => panic!("expected a ClickHouse server error, got {other:?}"),
    }
    ch.drop_database().await.unwrap();
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
/// still means stored) but gives up after 25 s, before the client's own 30 s
/// timeout, and tolerates wide backfills without switching the partition
/// guard off.
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
        ("wait_for_async_insert_timeout", "25"),
        ("max_partitions_per_insert_block", "400"),
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
        !params
            .iter()
            .any(|(k, _)| k == "throw_on_max_partitions_per_insert_block"),
        "the partition guard must stay on: {params:?}"
    );
    assert!(
        params
            .iter()
            .any(|(k, v)| k == "param_table" && v == "logs"),
        "{params:?}"
    );
}

/// The ingest path builds the insert body on a blocking thread
/// (`store::encode_logs`) and sends it from the async side
/// (`store::insert_encoded_logs`). That is the same insert as
/// `store::insert_logs`: the same settings on the request, the same body, and
/// the same rows in the table.
#[tokio::test]
async fn an_encoded_batch_is_inserted_like_insert_logs_inserts_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let probe = ClickHouse::new(&server.uri(), "nami", "nami", "nami_logs");
    let source = source();
    let rows: Vec<LogRow> = (0..3)
        .map(|n| {
            row(
                &source,
                json!({ "message": format!("m {n}"), "n": n }),
                now_millis(),
            )
        })
        .collect();
    store::insert_logs(&probe, &rows).await.unwrap();
    let body = store::encode_logs(&rows, 1 << 20).unwrap();
    store::insert_encoded_logs(&probe, body.clone())
        .await
        .unwrap();
    assert_eq!(body.lines().count(), 3);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let params = |index: usize| {
        let mut pairs: Vec<(String, String)> = requests[index]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        pairs.sort();
        pairs
    };
    assert_eq!(params(0), params(1), "the same settings and query");
    assert_eq!(requests[0].body, requests[1].body, "the same body");

    // Nothing to send, nothing sent.
    assert_eq!(store::encode_logs(&[], 0).unwrap(), "");
    store::insert_encoded_logs(&probe, String::new())
        .await
        .unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    // And in a real ClickHouse the encoded body is stored as rows.
    let ch = ClickHouse::create_test_database().await.unwrap();
    store::insert_encoded_logs(&ch, body).await.unwrap();
    let stored: Vec<Value> = ch
        .query_rows(
            "SELECT message, fields FROM logs WHERE project_id = {p:UUID} ORDER BY message",
            &[("p", source.project_id.to_string())],
        )
        .await
        .unwrap();
    assert_eq!(
        stored,
        vec![
            json!({ "message": "m 0", "fields": r#"{"n":0}"# }),
            json!({ "message": "m 1", "fields": r#"{"n":1}"# }),
            json!({ "message": "m 2", "fields": r#"{"n":2}"# }),
        ]
    );
    ch.drop_database().await.unwrap();
}

/// A batch over the cap is refused before anything is built; one at exactly
/// the cap is not.
#[test]
fn an_encoded_batch_over_the_cap_is_refused() {
    let source = source();
    let rows: Vec<LogRow> = (0..10)
        .map(|n| {
            row(
                &source,
                json!({ "message": format!("m {n}") }),
                now_millis(),
            )
        })
        .collect();
    let exact = store::encode_logs(&rows, usize::MAX).unwrap().len();
    assert_eq!(store::encode_logs(&rows, exact).unwrap().len(), exact);
    assert!(matches!(
        store::encode_logs(&rows, exact - 1),
        Err(ClickHouseError::TooLarge { limit }) if limit == exact - 1
    ));
}
