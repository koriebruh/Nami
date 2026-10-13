// HTTP log ingestion (`POST /api/v1/logs`), served by the public listener.
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{io::Write, sync::Arc, time::Duration};

use axum::http::StatusCode;
use common::{
    LogCapture,
    logs::{
        messages, post_json, post_logs, seed_source, seed_source_with, source_config, stored_rows,
        test_cipher, unreachable_pool,
    },
    public_app, seed_project, state_with_clickhouse, test_public_app,
};
use nami_engine::{
    logs::{
        LogsState,
        normalize::{FIELDS_TRUNCATED_KEY, MAX_MESSAGE_BYTES, TRUNCATION_MARKER},
        rate_limit::TokenBuckets,
    },
    models::source::{self, SourceConfig},
    router::build_public_router_with_ingest_limits,
    routes::logs_ingest::IngestLimits,
    state::AppState,
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;

const JSON: (&str, &str) = ("content-type", "application/json");
const NDJSON: (&str, &str) = ("content-type", "application/x-ndjson");
const MSGPACK: (&str, &str) = ("content-type", "application/msgpack");
const GZIP: (&str, &str) = ("content-encoding", "gzip");

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn msgpack(value: &Value) -> Vec<u8> {
    rmp_serde::to_vec(value).unwrap()
}

#[sqlx::test]
async fn json_objects_arrays_and_ndjson_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;

    let object = json!({
        "dt": chrono::Utc::now().to_rfc3339(),
        "message": "one",
        "level": "WARNING",
        "trace_id": "4BF92F3577B34DA6A3CE929D0E0E4736",
        "user": { "id": 1 }
    });
    assert_eq!(
        post_json(&public, &token, object).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    let array = json!([{ "msg": "two" }, { "log": "three", "severity": 3 }]);
    assert_eq!(
        post_json(&public, &token, array).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    let ndjson = "{\"message\":\"four\"}\n\n{\"message\":\"five\"}\r\n";
    let (status, _, body) = post_logs(&public, Some(&token), &[NDJSON], ndjson).await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    // Shippers that send NDJSON as application/json are accepted too.
    let (status, _, body) = post_logs(
        &public,
        Some(&token),
        &[JSON],
        "{\"message\":\"six\"}\n{\"message\":\"seven\"}",
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    // `curl -d` labels its body application/x-www-form-urlencoded.
    let (status, _, body) = post_logs(
        &public,
        Some(&token),
        &[("content-type", "application/x-www-form-urlencoded")],
        "{\"message\":\"eight\"}",
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    assert_eq!(
        post_json(&public, &token, json!([])).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 0 }))
    );

    let rows = stored_rows(&ch, project).await;
    assert_eq!(
        messages(&rows),
        [
            "eight", "five", "four", "one", "seven", "six", "three", "two"
        ]
    );
    let one = rows.iter().find(|r| r["message"] == "one").unwrap();
    assert_eq!(one["level"], "warn");
    assert_eq!(one["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert_eq!(one["fields"], r#"{"user":{"id":1}}"#);
    assert_eq!(one["source_id"], source_id.to_string());
    assert_eq!(one["retention_days"], 30);
    assert_eq!(
        rows.iter().find(|r| r["message"] == "three").unwrap()["level"],
        "error"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn messagepack_bodies_are_stored(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let (status, _, body) = post_logs(
        &public,
        Some(&token),
        &[MSGPACK],
        msgpack(&json!({ "message": "mp one", "level": 50 })),
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    let array = msgpack(&json!([{ "message": "mp two" }, { "message": "mp three" }]));
    let (status, _, body) = post_logs(
        &public,
        Some(&token),
        &[("content-type", "application/x-msgpack")],
        array,
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    // { "message": "mp bin", "b": bin8 [1, 2, 3], "t": timestamp32(1_700_000_000) }
    let mut raw = vec![0x83, 0xa7];
    raw.extend(b"message");
    raw.extend([0xa6]);
    raw.extend(b"mp bin");
    raw.extend([0xa1, b'b', 0xc4, 3, 1, 2, 3, 0xa1, b't', 0xd6, 0xff]);
    raw.extend(1_700_000_000u32.to_be_bytes());
    let (status, _, body) = post_logs(&public, Some(&token), &[MSGPACK], raw).await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );

    let rows = stored_rows(&ch, project).await;
    assert_eq!(messages(&rows), ["mp bin", "mp one", "mp three", "mp two"]);
    let bin = rows.iter().find(|r| r["message"] == "mp bin").unwrap();
    assert_eq!(bin["fields"], r#"{"b":"AQID","t":"2023-11-14T22:13:20Z"}"#);
    assert_eq!(
        rows.iter().find(|r| r["message"] == "mp one").unwrap()["level"],
        "error"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn gzip_bodies_are_decompressed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let body = gzip(
        json!([{ "message": "gz one" }, { "message": "gz two" }])
            .to_string()
            .as_bytes(),
    );
    let (status, _, json) = post_logs(&public, Some(&token), &[JSON, GZIP], body).await;
    assert_eq!(
        (status, json),
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    let body = gzip(&msgpack(&json!({ "message": "gz three" })));
    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[MSGPACK, ("content-encoding", "x-gzip")],
        body,
    )
    .await;
    assert_eq!(
        (status, json),
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[JSON, GZIP],
        "{\"message\":\"not gzip\"}",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert!(
        json["error"]
            .as_str()
            .unwrap()
            .starts_with("invalid gzip body"),
        "{json}"
    );
    assert_eq!(
        messages(&stored_rows(&ch, project).await),
        ["gz one", "gz three", "gz two"]
    );
    ch.drop_database().await.unwrap();
}

/// Security: every token that is not a live source's is a generic 401 and
/// nothing is stored.
#[sqlx::test]
async fn missing_invalid_rotated_and_deleted_source_tokens_are_401(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (rotated_id, rotated_old) = seed_source(&pool, project, "rotated").await;
    let rotated_new = source::rotate_token(&pool, &test_cipher(), project, rotated_id)
        .await
        .unwrap()
        .unwrap()
        .token;
    let (deleted_id, deleted_token) = seed_source(&pool, project, "deleted").await;
    assert!(
        source::delete_in_project(&pool, project, deleted_id)
            .await
            .unwrap()
    );
    let body = json!({ "message": "should not be stored" }).to_string();
    let unauthorized = (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));

    let (status, _, json) = post_logs(&public, None, &[JSON], body.clone()).await;
    assert_eq!((status, json), unauthorized.clone());
    for authorization in [
        format!("Basic {rotated_new}"),
        "Bearer".to_string(),
        format!("Bearer {rotated_new}x"),
    ] {
        let (status, _, json) = post_logs(
            &public,
            None,
            &[JSON, ("authorization", authorization.as_str())],
            body.clone(),
        )
        .await;
        assert_eq!((status, json), unauthorized.clone(), "{authorization}");
    }
    for token in [rotated_old.as_str(), deleted_token.as_str(), "not-a-token"] {
        let (status, _, json) = post_logs(&public, Some(token), &[JSON], body.clone()).await;
        assert_eq!((status, json), unauthorized.clone(), "{token}");
    }
    assert!(stored_rows(&ch, project).await.is_empty());
    assert_eq!(
        post_json(&public, &rotated_new, json!({ "message": "ok" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn oversized_bodies_gzip_bombs_and_too_many_events_are_413(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let rejected = |json: &Value, fragment: &str| {
        assert!(
            json["error"].as_str().unwrap().contains(fragment),
            "{json} should mention {fragment:?}"
        );
    };

    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[JSON],
        vec![b' '; 10 * 1024 * 1024 + 1],
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "request body exceeds 10 MiB");
    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[JSON, ("content-length", "20971520")],
        "{\"message\":\"x\"}",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "a declared length is refused before reading"
    );
    rejected(&json, "request body exceeds 10 MiB");

    // 51 gzip members of 1 MiB of zeros: ~50 KiB on the wire, 51 MiB inflated.
    let bomb = gzip(&vec![0u8; 1024 * 1024]).repeat(51);
    let (status, _, json) = post_logs(&public, Some(&token), &[JSON, GZIP], bomb).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "decompressed request body exceeds 50 MiB");

    let too_many = Value::Array(vec![json!({ "message": "m" }); 10_001]);
    let (status, json) = post_json(&public, &token, too_many).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    rejected(&json, "too many events: at most 10000 per request");
    let (status, _, json) =
        post_logs(&public, Some(&token), &[NDJSON], "{}\n".repeat(10_001)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");
    let declared_too_many = [0xdd, 0x00, 0x00, 0x27, 0x11];
    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[MSGPACK],
        declared_too_many.to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");
    assert!(stored_rows(&ch, project).await.is_empty());

    let exactly = Value::Array(vec![json!({ "message": "m" }); 10_000]);
    assert_eq!(
        post_json(&public, &token, exactly).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 10_000 }))
    );
    assert_eq!(stored_rows(&ch, project).await.len(), 10_000);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn malformed_bodies_are_400_and_unsupported_formats_415(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let nested_json = |depth: usize| {
        format!(
            "{{\"a\":{}{}}}",
            "[".repeat(depth - 1),
            "]".repeat(depth - 1)
        )
    };
    let nested_msgpack = |depth: usize| {
        let mut raw = vec![0x81, 0xa1, b'a'];
        raw.extend(vec![0x91; depth - 2]);
        raw.push(0x90);
        raw
    };

    for (headers, body, fragment) in [
        (vec![JSON], b"{\"message\":".to_vec(), "invalid JSON body"),
        (
            vec![JSON],
            b"\"just a string\"".to_vec(),
            "expected a JSON object or an array of objects",
        ),
        (vec![JSON], b"[{\"a\":1}, 2]".to_vec(), "invalid JSON body"),
        (vec![NDJSON], b"{\"a\":1}\nnot json\n".to_vec(), "line 2"),
        (vec![JSON], b"   ".to_vec(), "request body is empty"),
        (
            vec![JSON],
            nested_json(128).into_bytes(),
            "recursion limit exceeded",
        ),
        (
            vec![MSGPACK],
            nested_msgpack(128),
            "nesting deeper than 127",
        ),
        (
            vec![MSGPACK],
            vec![0x81, 0x01, 0x02],
            "map keys must be strings",
        ),
        (
            vec![MSGPACK],
            vec![0x82, 0xa1, b'a'],
            "unexpected end of input",
        ),
    ] {
        let (status, _, json) = post_logs(&public, Some(&token), &headers, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{}: {json}",
            String::from_utf8_lossy(&body)
        );
        assert!(
            json["error"].as_str().unwrap().contains(fragment),
            "{json} should mention {fragment:?}"
        );
    }
    let (status, _, json) = post_logs(&public, Some(&token), &[JSON], nested_json(127)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "127 levels are fine: {json}");
    let (status, _, json) = post_logs(&public, Some(&token), &[MSGPACK], nested_msgpack(127)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");

    for headers in [
        vec![("content-type", "application/x-protobuf")],
        vec![JSON, ("content-encoding", "br")],
    ] {
        let (status, _, json) = post_logs(&public, Some(&token), &headers, "{}").await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{headers:?}: {json}"
        );
    }
    ch.drop_database().await.unwrap();
}

/// A type error in a JSON body quotes what was sent; a body of one huge
/// string must not come back as one huge error message.
#[sqlx::test]
async fn error_messages_do_not_echo_the_body(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let body = format!("\"{}\"", "a".repeat(2 * 1024 * 1024));
    let (status, _, json) = post_logs(&public, Some(&token), &[JSON], body).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let message = json["error"].as_str().unwrap();
    assert!(message.starts_with("invalid JSON body"), "{message}");
    assert!(message.len() < 512, "{} bytes", message.len());
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_rate_limit_is_per_source_and_answers_429_json(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    // A bucket of 3 that practically never refills, so the test is not timing-sensitive.
    let logs = Arc::new(LogsState {
        rate_limiter: TokenBuckets::new(0.0001, 3.0),
        ..LogsState::default()
    });
    let state = AppState {
        logs: logs.clone(),
        ..state
    };
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (limited, token) = seed_source(&pool, project, "busy").await;
    let (_, other_token) = seed_source(&pool, project, "quiet").await;

    for _ in 0..3 {
        assert_eq!(
            post_json(&public, &token, json!({ "message": "m" }))
                .await
                .0,
            StatusCode::ACCEPTED
        );
    }
    let (status, headers, json) =
        post_logs(&public, Some(&token), &[JSON], "{\"message\":\"m\"}").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["code"], "rate_limited");
    assert!(
        json["error"].as_str().unwrap().contains("rate limit"),
        "{json}"
    );
    assert_eq!(headers["retry-after"], "1");
    assert_eq!(
        post_json(&public, &other_token, json!({ "message": "m" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );

    logs.usage.flush(&pool).await.unwrap();
    let rate_limited: i64 = sqlx::query_scalar(
        "select requests_rate_limited from source_usage_daily where source_id = $1",
    )
    .bind(limited)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rate_limited, 1);
    assert_eq!(stored_rows(&ch, project).await.len(), 4);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn long_messages_and_large_fields_are_capped_on_ingest(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let event =
        json!({ "message": "m".repeat(70 * 1024), "stack": "s".repeat(300 * 1024), "user_id": 7 });
    assert_eq!(
        post_json(&public, &token, event).await.0,
        StatusCode::ACCEPTED
    );

    let row = &stored_rows(&ch, project).await[0];
    let message = row["message"].as_str().unwrap();
    assert!(
        message.len() <= MAX_MESSAGE_BYTES && message.ends_with(TRUNCATION_MARKER),
        "{}",
        message.len()
    );
    let fields: Value = serde_json::from_str(row["fields"].as_str().unwrap()).unwrap();
    assert_eq!(fields["user_id"], 7);
    assert!(fields.get("stack").is_none());
    assert!(fields.get(FIELDS_TRUNCATED_KEY).is_some());
    ch.drop_database().await.unwrap();
}

/// Security: ingest is served by the public listener and by nothing else, so
/// exposing `PUBLIC_PORT` is all shippers need and the private API (the
/// login, whose rate limit trusts a forwardable header) stays unexposed.
#[sqlx::test]
async fn ingest_is_served_on_the_public_listener_only(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let private = common::app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let body = json!({ "message": "where" });

    // The private API refuses a body that is not JSON with 415 before it
    // routes anything, which would hide whether the route exists: the
    // request carries JSON, and the route is not there.
    let (status, _, _) = post_logs(&private, Some(&token), &[JSON], body.to_string()).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "POST /api/v1/logs on the API"
    );
    assert!(stored_rows(&ch, project).await.is_empty());

    assert_eq!(
        post_json(&public, &token, body).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    // Only POST: a GET (a browser, a prefetcher) neither reads nor stores.
    let response = tower::ServiceExt::oneshot(
        public.clone(),
        axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/logs")
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(stored_rows(&ch, project).await.len(), 1);
    ch.drop_database().await.unwrap();
}

/// Security: a token that cannot be a source's (not 43 base64url characters)
/// is refused before the database is asked, so garbage costs a flood nothing
/// the API and the uptime worker need from the shared pool.
#[sqlx::test]
async fn tokens_that_cannot_be_a_source_token_are_refused_before_any_database_access(
    pool: sqlx::PgPool,
) {
    let public = public_app(AppState {
        db: unreachable_pool(),
        ..AppState::for_test(pool.clone())
    });
    let body = json!({ "message": "m" }).to_string();
    let unauthorized = (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }));

    let well_formed = source::new_token();
    for token in [
        "x".repeat(42),
        "x".repeat(44),
        "x".repeat(100_000),
        format!("{}+", "x".repeat(42)),
        format!("{}/", "x".repeat(42)),
        format!("{}=", "x".repeat(42)),
        format!("{} ", "x".repeat(42)),
        format!("{}é", "x".repeat(41)),
        "not-a-token".to_string(),
    ] {
        let (status, _, json) = post_logs(&public, Some(&token), &[JSON], body.clone()).await;
        assert_eq!(
            (status, json),
            unauthorized.clone(),
            "{} characters",
            token.chars().count()
        );
    }
    // The control: a token of the right shape does reach the database, which
    // here cannot answer.
    let (status, _, json) = post_logs(&public, Some(&well_formed), &[JSON], body).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "internal error" })
        )
    );
}

/// With every ingest place taken, a request is turned away at once with 503
/// and `Retry-After: 1`, before any database work, and nothing is stored or
/// counted. A token that cannot be one is still a plain 401: it needs no
/// place. Once places are free, ingest is served again.
#[sqlx::test]
async fn a_full_ingest_endpoint_turns_requests_away_with_retry_after(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let slots = Arc::new(Semaphore::new(2));
    let limits = IngestLimits {
        places: slots.clone(),
        ..IngestLimits::default()
    };
    let public = build_public_router_with_ingest_limits(state.clone(), limits.clone());
    // The same endpoint over a pool that cannot answer: whatever it replies
    // while full, it replied without a query.
    let without_database = build_public_router_with_ingest_limits(
        AppState {
            db: unreachable_pool(),
            ..state
        },
        limits,
    );
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;
    let held = slots.clone().acquire_many_owned(2).await.unwrap();

    for router in [&public, &without_database] {
        let started = std::time::Instant::now();
        let (status, headers, json) =
            post_logs(router, Some(&token), &[JSON], "{\"message\":\"m\"}").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{json}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "answered at once"
        );
        assert_eq!(headers["retry-after"], "1");
        assert_eq!(json["code"], "busy", "{json}");
    }
    let (status, _, json) = post_logs(
        &without_database,
        Some("not-a-token"),
        &[JSON],
        "{\"message\":\"m\"}",
    )
    .await;
    assert_eq!(
        (status, json),
        (StatusCode::UNAUTHORIZED, json!({ "error": "unauthorized" }))
    );
    assert!(stored_rows(&ch, project).await.is_empty());
    let today = chrono::Utc::now().date_naive();
    assert_eq!(
        logs.usage.pending(source_id, today),
        Default::default(),
        "a request that was turned away counts for nothing"
    );

    drop(held);
    assert_eq!(
        post_json(&public, &token, json!({ "message": "m" })).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    ch.drop_database().await.unwrap();
}

/// A request holds its place for as long as it runs, database work included:
/// with one place, a request waiting on the database keeps it, and a second
/// request meanwhile is turned away instead of queueing for a connection.
/// Once the database answers, the first completes and the place is given
/// back.
#[sqlx::test]
async fn an_ingest_request_holds_its_place_while_it_waits_for_the_database(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        places: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    let public = build_public_router_with_ingest_limits(state, limits);
    let project = seed_project(&pool).await;
    // The quota check reads source_usage_daily; an exclusive lock on the
    // table holds that read (and so the request) where the test wants it.
    let quota = SourceConfig {
        daily_byte_quota: Some(10 * 1024 * 1024),
        ..source_config("capped")
    };
    let (_, token) = seed_source_with(&pool, project, quota).await;
    let (_, other_token) = seed_source(&pool, project, "free").await;

    let mut lock = pool.begin().await.unwrap();
    sqlx::query("lock table source_usage_daily in access exclusive mode")
        .execute(&mut *lock)
        .await
        .unwrap();
    let first = tokio::spawn({
        let public = public.clone();
        let token = token.clone();
        async move {
            post_json(&public, &token, json!({ "message": "first" }))
                .await
                .0
        }
    });
    wait_for_a_lock_wait(&pool, "from source_usage_daily").await;

    let (status, _, _) = post_logs(
        &public,
        Some(&other_token),
        &[JSON],
        "{\"message\":\"second\"}",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the only place is taken"
    );

    lock.rollback().await.unwrap();
    assert_eq!(first.await.unwrap(), StatusCode::ACCEPTED);
    assert_eq!(
        post_json(&public, &other_token, json!({ "message": "third" }))
            .await
            .0,
        StatusCode::ACCEPTED,
        "the place was given back"
    );
    assert_eq!(
        messages(&stored_rows(&ch, project).await),
        ["first", "third"]
    );
    ch.drop_database().await.unwrap();
}

/// A request holds its place while its body arrives, so a client that stalls
/// (a dead connection that never closes, a sender that goes quiet) must not
/// keep it for ever: at the deadline the request ends with 408 and the place
/// is free again. Over a real socket, which is where a client can stall.
#[sqlx::test]
async fn a_stalled_body_gives_up_its_place_at_the_deadline(pool: sqlx::PgPool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        places: Arc::new(Semaphore::new(1)),
        body_read_timeout: Duration::from_millis(800),
    };
    let public = build_public_router_with_ingest_limits(state, limits);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, public).await.unwrap() });
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let post = || {
        http.post(format!("http://{addr}/api/v1/logs"))
            .bearer_auth(&token)
            .json(&json!({ "message": "m" }))
            .send()
    };

    // A client announces 1000 bytes, sends one and goes quiet.
    let mut stalled = tokio::net::TcpStream::connect(addr).await.unwrap();
    stalled
        .write_all(
            format!(
                "POST /api/v1/logs HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\
                 Content-Type: application/json\r\nContent-Length: 1000\r\n\r\n{{"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        post().await.unwrap().status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the only place is held by the stalled request"
    );

    let mut reply = [0u8; 512];
    let read = tokio::time::timeout(Duration::from_secs(10), stalled.read(&mut reply))
        .await
        .expect("the stalled request is answered at the deadline")
        .unwrap();
    let reply = String::from_utf8_lossy(&reply[..read]);
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(reply.contains(r#""code":"request_timeout""#), "{reply}");

    assert_eq!(
        post().await.unwrap().status(),
        StatusCode::ACCEPTED,
        "the place was given back"
    );
    assert_eq!(stored_rows(&ch, project).await.len(), 1);
    server.abort();
    ch.drop_database().await.unwrap();
}

async fn wait_for_a_lock_wait(pool: &sqlx::PgPool, statement: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let waiting: i64 = sqlx::query_scalar(
            "select count(*) from pg_stat_activity
             where datname = current_database()
               and wait_event_type = 'Lock'
               and query ilike '%' || $1 || '%'",
        )
        .bind(statement)
        .fetch_one(pool)
        .await
        .unwrap();
        if waiting > 0 {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no statement containing {statement:?} ever waited for a lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Usage is counted for what was stored: a request that failed in any way
/// (bad body, storage down) adds no events or bytes, so quotas are not eaten
/// by what was never kept.
#[sqlx::test]
async fn only_stored_events_count_towards_usage(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let logs = state.logs.clone();
    // ClickHouse here is bound to a database that does not exist: every
    // insert fails.
    let broken = public_app(state);
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;

    let (status, json) = post_json(&broken, &token, json!({ "message": "lost" })).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "internal error" })
        )
    );
    let (status, _, _) = post_logs(&broken, Some(&token), &[JSON], "{not json").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let today = chrono::Utc::now().date_naive();
    assert_eq!(logs.usage.pending(source_id, today), Default::default());
}

/// Security: tokens never reach a log line — accepted, rejected, or when
/// storing fails and the error is logged.
#[sqlx::test]
async fn tokens_never_appear_in_log_output(pool: sqlx::PgPool) {
    let capture = LogCapture::default();
    let _guard = capture.install_trace();

    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let working = public_app(state);
    assert_eq!(
        post_json(&working, &token, json!({ "message": "fine" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    let wrong = format!("{token}-wrong");
    assert_eq!(
        post_json(&working, &wrong, json!({ "message": "no" }))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // A token of the right shape that is nobody's reaches the database lookup.
    let unknown = source::new_token();
    assert_eq!(
        post_json(&working, &unknown, json!({ "message": "no" }))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    // `test_public_app` points ClickHouse at a database that does not exist:
    // the insert fails and the error is logged.
    let broken = test_public_app(pool.clone());
    let (status, json) = post_json(&broken, &token, json!({ "message": "lost" })).await;
    assert_eq!(
        (status, json),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "internal error" })
        )
    );

    let output = capture.lines().join("\n");
    assert!(
        output.contains("clickhouse error"),
        "the capture works: {output}"
    );
    for secret in [&token, &wrong, &unknown] {
        assert!(
            !output.contains(secret.as_str()),
            "a token leaked into the logs:\n{output}"
        );
    }
    // Nor does the hash a token is looked up by.
    assert!(
        !output.contains(&nami_engine::auth::token::hash_token(&token)),
        "a token hash leaked into the logs:\n{output}"
    );
    ch.drop_database().await.unwrap();
}
