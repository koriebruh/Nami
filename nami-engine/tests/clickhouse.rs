// Requires `docker compose up -d clickhouse` (user/password nami/nami).
use chrono::{DateTime, Duration, Utc};
use nami_engine::{
    clickhouse::{ClickHouse, ClickHouseError},
    state::AppState,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

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

/// Like `CheckRow`, but with a misspelled field: the table column is
/// `response_time_ms`.
#[derive(Debug, Serialize)]
struct MisspelledRow {
    monitor_id: Uuid,
    project_id: Uuid,
    checked_at: DateTime<Utc>,
    status: String,
    respnse_time_ms: u32,
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
    // placeholder-looking fragment; then the characters that are special one
    // layer up, in the URL's form encoding (`+` is a space, `&`/`=`/`#`
    // delimit, `%20` is an escape), and multibyte UTF-8.
    let hostile = concat!(
        "it's \"quoted\" C:\\new\\folder\ttab\nline'; DROP TABLE uptime_checks; -- {error:String}",
        " a+b c&d=e%20f%2Fg#frag é日本",
    );
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
    // `+` stays a plus (not a space) and `%20` stays three literal characters
    // (not decoded a second time): each near-miss must differ from the stored value.
    assert_eq!(count(hostile.replace("a+b", "a b")).await, 0, "`+` must not become a space");
    assert_eq!(count(hostile.replace("e%20f", "e f")).await, 0, "`%20` must not be decoded");
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
            "SELECT name FROM system.tables WHERE database = {db:String} ORDER BY name",
            &[("db", ch.database().to_string())],
        )
        .await
        .unwrap();
    assert_eq!(
        tables,
        vec![
            serde_json::json!({ "name": "logs" }),
            serde_json::json!({ "name": "uptime_checks" })
        ]
    );
    ch.drop_database().await.unwrap();
}

#[tokio::test]
#[should_panic(expected = "drop_database only drops")]
async fn drop_database_refuses_anything_but_test_databases() {
    // A name that neither exists nor carries the test prefix, on a URL nothing
    // listens on: if the guard ever broke, the DROP could neither find nor
    // reach any real data (and the missing panic would fail this test).
    let ch = ClickHouse::new("http://127.0.0.1:1", "nami", "nami", "nami_guard_probe");
    let _ = ch.drop_database().await;
}

#[tokio::test]
async fn a_row_with_an_unknown_field_is_rejected_not_stored_with_defaults() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let typo = MisspelledRow {
        monitor_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        checked_at: now_millis(),
        status: "up".to_string(),
        respnse_time_ms: 42,
    };
    let error = ch.insert_rows("uptime_checks", &[typo]).await.unwrap_err();
    match error {
        ClickHouseError::Server { status, body } => {
            assert!((400..600).contains(&status), "status was {status}");
            assert!(body.contains("respnse_time_ms"), "the error must name the field: {body:?}");
        }
        other => panic!("expected ClickHouseError::Server, got {other:?}"),
    }
    let stored: Vec<Count> = ch
        .query_rows("SELECT count() AS n FROM uptime_checks", &[])
        .await
        .unwrap();
    assert_eq!(stored[0].n, 0, "the rejected row must not be stored with a defaulted column");
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn query_rows_is_read_only_so_ddl_and_writes_fail_on_the_read_path() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    ch.insert_rows("uptime_checks", &[row(Uuid::new_v4(), now_millis(), None)])
        .await
        .unwrap();

    for sql in [
        "CREATE TABLE sneaky (x UInt8) ENGINE = Memory",
        "INSERT INTO uptime_checks (monitor_id, project_id, checked_at, status) \
         VALUES (generateUUIDv4(), generateUUIDv4(), now64(3), 'up')",
        "ALTER TABLE uptime_checks DELETE WHERE 1",
        "TRUNCATE TABLE uptime_checks",
        "DROP TABLE uptime_checks",
        // `readonly=2` lets a request change other settings, never `readonly` itself.
        "SELECT 1 SETTINGS readonly = 0",
    ] {
        match ch.query_rows::<serde_json::Value>(sql, &[]).await.unwrap_err() {
            ClickHouseError::Server { body, .. } => {
                assert!(body.contains("READONLY"), "{sql}: body was {body:?}");
            }
            other => panic!("{sql}: expected ClickHouseError::Server, got {other:?}"),
        }
    }

    // Nothing above took effect, and ordinary reads still work.
    let tables: Vec<serde_json::Value> = ch
        .query_rows(
            "SELECT name FROM system.tables WHERE database = {db:String} ORDER BY name",
            &[("db", ch.database().to_string())],
        )
        .await
        .unwrap();
    assert_eq!(
        tables,
        vec![
            serde_json::json!({ "name": "logs" }),
            serde_json::json!({ "name": "uptime_checks" })
        ]
    );
    let rows: Vec<Count> = ch
        .query_rows("SELECT count() AS n FROM uptime_checks", &[])
        .await
        .unwrap();
    assert_eq!(rows[0].n, 1);
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn transport_errors_never_contain_parameter_values_or_credentials() {
    // Nothing listens on port 1, so the request fails before reaching a server.
    let ch = ClickHouse::new("http://127.0.0.1:1", "nami", "hunter2-password", "nami_guard_probe");
    let error = ch
        .query_rows::<serde_json::Value>(
            "SELECT {x:String}",
            &[("x", "secret-param-value".to_string())],
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ClickHouseError::Transport(_)),
        "expected a transport error, got {error:?}"
    );
    // The reqwest error carries the request URL, which holds every `param_*`
    // value; it must be stripped from both renderings (logs use either).
    for rendering in [error.to_string(), format!("{error:?}")] {
        for leaked in ["secret-param-value", "param_x", "hunter2-password"] {
            assert!(!rendering.contains(leaked), "{leaked:?} leaked into {rendering:?}");
        }
    }
}

#[tokio::test]
async fn credentials_travel_in_a_basic_auth_header_never_in_the_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let ch = ClickHouse::new(&server.uri(), "nami", "s3cret", "nami_guard_probe");
    ch.execute("SELECT 1").await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request.headers.get("authorization").and_then(|v| v.to_str().ok()),
        Some("Basic bmFtaTpzM2NyZXQ="), // base64("nami:s3cret")
    );
    assert!(request.headers.get("x-clickhouse-key").is_none());
    assert!(request.headers.get("x-clickhouse-user").is_none());
    assert!(!request.url.as_str().contains("s3cret"), "password in the URL: {}", request.url);
}

/// The `name=value` pairs of the one request a mock server received.
async fn received_settings(server: &MockServer) -> Vec<(String, String)> {
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    requests[0]
        .url
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

/// Where `name` appears in `pairs`: every settings name must appear once.
fn only_position(pairs: &[(String, String)], name: &str) -> usize {
    let at: Vec<usize> = pairs
        .iter()
        .enumerate()
        .filter(|(_, (key, _))| key == name)
        .map(|(index, _)| index)
        .collect();
    assert_eq!(at.len(), 1, "{name} must be sent exactly once: {pairs:?}");
    at[0]
}

#[tokio::test]
async fn query_rows_with_adds_its_settings_and_still_sends_the_ones_the_client_owns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{\"n\":1}\n"))
        .mount(&server)
        .await;
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_guard_probe");
    let rows: Vec<serde_json::Value> = ch
        .query_rows_with(
            "SELECT 1 AS n",
            &[],
            &[("max_execution_time", "7"), ("max_result_rows", "1000")],
        )
        .await
        .unwrap();
    assert_eq!(rows, vec![serde_json::json!({ "n": 1 })]);

    let pairs = received_settings(&server).await;
    for (name, value) in [("max_execution_time", "7"), ("max_result_rows", "1000")] {
        assert_eq!(pairs[only_position(&pairs, name)].1, value, "{pairs:?}");
    }
    // The settings the client owns still arrive, with their values, and after
    // the extra ones, so nothing a caller passes can take their place.
    let last_extra = only_position(&pairs, "max_result_rows");
    for (name, value) in [
        ("default_format", "JSONEachRow"),
        ("wait_end_of_query", "1"),
        ("readonly", "2"),
    ] {
        let at = only_position(&pairs, name);
        assert!(
            at > last_extra,
            "{name} must come after the extra settings: {pairs:?}"
        );
        assert_eq!(pairs[at].1, value, "{pairs:?}");
    }
}

/// The list of settings a caller may not pass is checked against what the
/// client really puts on its requests, so a setting added to a request
/// without being reserved, or dropped from the list, fails here.
#[tokio::test]
async fn every_setting_the_client_sends_on_its_own_is_refused_as_an_extra() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_guard_probe");
    ch.execute("SELECT 1").await.unwrap();
    ch.query_rows::<serde_json::Value>("SELECT 1", &[("x", "1".to_string())])
        .await
        .unwrap();
    ch.insert_rows("uptime_checks", &[row(Uuid::new_v4(), now_millis(), None)])
        .await
        .unwrap();

    let sent: std::collections::BTreeSet<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .flat_map(|request| request.url.query_pairs().map(|(name, _)| name.into_owned()))
        .filter(|name| !name.starts_with("param_"))
        .collect();
    assert!(
        sent.len() >= 9,
        "expected the client's own settings, got {sent:?}"
    );
    for name in sent {
        // Settings are `&'static str` by design; leaking a few test names is fine.
        let name: &'static str = Box::leak(name.into_boxed_str());
        let ch = ch.clone();
        let outcome = tokio::spawn(async move {
            ch.query_rows_with::<serde_json::Value>("SELECT 1", &[], &[(name, "1")])
                .await
        })
        .await;
        assert!(
            outcome.unwrap_err().is_panic(),
            "{name} is set by the client itself, so it must be refused as an extra setting"
        );
    }
}

#[tokio::test]
async fn insert_rows_with_adds_its_settings_and_still_sends_the_ones_the_client_owns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_guard_probe");
    ch.insert_rows_with(
        "uptime_checks",
        &[row(Uuid::new_v4(), now_millis(), None)],
        &[("async_insert", "1"), ("wait_for_async_insert", "1")],
    )
    .await
    .unwrap();

    let pairs = received_settings(&server).await;
    for (name, value) in [("async_insert", "1"), ("wait_for_async_insert", "1")] {
        assert_eq!(pairs[only_position(&pairs, name)].1, value, "{pairs:?}");
    }
    let last_extra = only_position(&pairs, "wait_for_async_insert");
    let strict = only_position(&pairs, "input_format_skip_unknown_fields");
    assert!(strict > last_extra, "{pairs:?}");
    assert_eq!(pairs[strict].1, "0", "{pairs:?}");
    assert_eq!(
        pairs[only_position(&pairs, "query")].1,
        "INSERT INTO {db:Identifier}.{table:Identifier} FORMAT JSONEachRow"
    );
}

fn assert_invalid_identifier<T: std::fmt::Debug>(result: Result<T, ClickHouseError>, expected: &str) {
    match result {
        Err(ClickHouseError::InvalidIdentifier(name)) => assert_eq!(name, expected),
        other => panic!("expected InvalidIdentifier({expected:?}), got {other:?}"),
    }
}

#[tokio::test]
async fn invalid_identifiers_are_rejected_before_any_request_is_sent() {
    // A live server that records every request it receives. It has no mocks
    // mounted, so a request that slipped through would come back as a 404 and
    // show up in `received_requests` instead of as an `InvalidIdentifier`.
    let server = MockServer::start().await;
    let any_row = || row(Uuid::new_v4(), now_millis(), None);

    let bad_names = [
        "a\\b",  // a backslash
        "a.b",    // a dot (would address another database's table)
        "a b",    // a space
        "a;b",    // a statement separator
        "",       // empty
        "1abc",   // leading digit
        "a-b",
        "a`b",
        "a'b",
        "a\"b",
        "a\nb",
        "a\n",    // trailing newline: `$` in a regex would let this through
        "\u{e9}", // non-ASCII letter
        "uptime_checks; DROP TABLE uptime_checks",
    ];
    for bad in bad_names {
        // As a table name, with rows and with none: the empty-slice shortcut
        // must not hide a bad name.
        let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_guard_probe");
        assert_invalid_identifier(ch.insert_rows(bad, &[any_row()]).await, bad);
        assert_invalid_identifier(ch.insert_rows::<CheckRow>(bad, &[]).await, bad);

        // As the database name, through every entry point that names it.
        let ch = ClickHouse::new(&server.uri(), "nami", "nami", bad);
        assert_invalid_identifier(ch.insert_rows("uptime_checks", &[any_row()]).await, bad);
        assert_invalid_identifier(ch.migrate().await, bad);
        assert_invalid_identifier(ch.execute("SELECT 1").await, bad);
        assert_invalid_identifier(ch.query_rows::<serde_json::Value>("SELECT 1", &[]).await, bad);
    }

    // Passes `drop_database`'s test-prefix guard, so the identifier check is
    // what stops it.
    let ch = ClickHouse::new(&server.uri(), "nami", "nami", "nami_test_a;b");
    assert_invalid_identifier(ch.drop_database().await, "nami_test_a;b");

    let received: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(received.is_empty(), "requests reached the server: {received:?}");
}

#[sqlx::test]
async fn app_state_for_test_never_points_at_the_real_database(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool);
    assert_ne!(state.clickhouse.database(), state.config.clickhouse_database);
    assert!(!state.clickhouse.database().starts_with("nami_test_"));
}
