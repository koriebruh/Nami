// HTTP log ingestion (`POST /api/v1/logs`), served by the public listener.
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::http::StatusCode;
use common::{
    LogCapture,
    logs::{
        closed_by_server, eventually, first_response, ingest_head, ingest_request, messages,
        post_bytes_to, post_json, post_logs, post_to, seed_source, seed_source_with, serve,
        source_config, stored_rows, unreachable_pool,
    },
    public_app, seed_project, state_with_clickhouse, test_cipher, test_public_app,
};
use nami_engine::{
    clickhouse::ClickHouse,
    logs::{
        LogsState,
        ingest::BodyPace,
        normalize::{FIELDS_TRUNCATED_KEY, MAX_MESSAGE_BYTES, TRUNCATION_MARKER},
        rate_limit::{DecodeShares, InFlight, TokenBuckets},
    },
    models::source::{self, SourceConfig},
    router::{build_public_router, build_public_router_with_ingest_limits},
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

const WAIT: Duration = Duration::from_secs(10);

/// A request holds its place while its body arrives, so a client that stalls
/// (a dead connection that never closes, a sender that goes quiet) must not
/// keep it for long: a small body gets the grace period, not the cap, and is
/// answered 408, logged with its source (never its token), and its place is
/// free again. Over a real socket, which is where a client can stall.
#[sqlx::test]
async fn a_stalled_body_gives_up_its_place_after_the_grace_period_and_is_logged(
    pool: sqlx::PgPool,
) {
    use tokio::io::AsyncWriteExt;

    let capture = LogCapture::default();
    let _guard = capture.install();
    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        places: Arc::new(Semaphore::new(1)),
        body_pace: BodyPace {
            grace: Duration::from_millis(500),
            min_bytes_per_second: 1_000_000,
            cap: Duration::from_secs(30),
        },
        ..IngestLimits::default()
    };
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;
    let message = json!({ "message": "m" });

    // A client announces 1000 bytes, sends one and goes quiet.
    let mut stalled = tokio::net::TcpStream::connect(served.addr).await.unwrap();
    let started = Instant::now();
    stalled
        .write_all(format!("{}{{", ingest_head(&token, 1000)).as_bytes())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the only place is held by the stalled request"
    );

    let reply = first_response(&mut stalled, WAIT)
        .await
        .expect("the stalled request is answered");
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(reply.contains(r#""code":"request_timeout""#), "{reply}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "after the grace period of 0.5 s, not the cap of 30 s: {:?}",
        started.elapsed()
    );

    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "the place was given back");
    assert_eq!(stored_rows(&ch, project).await.len(), 1);

    let output = capture.lines().join("\n");
    assert!(
        output.contains("ingest request body did not arrive in time"),
        "{output}"
    );
    assert!(output.contains(&source_id.to_string()), "{output}");
    assert!(!output.contains(&token), "a token leaked into the logs");
    ch.drop_database().await.unwrap();
}

/// What a body must keep up with grows with what it has delivered, so a client
/// that sends a trickle (a flat deadline would give it the cap) is cut off as
/// soon as it falls behind: here 200 bytes every 150 ms is 1.3 KB/s against a
/// minimum of 2 KB/s, for a body that would take 75 s to arrive.
#[sqlx::test]
async fn a_body_that_crawls_in_slower_than_the_minimum_pace_is_cut_off_early(pool: sqlx::PgPool) {
    use tokio::io::AsyncWriteExt;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        body_pace: BodyPace {
            grace: Duration::from_millis(300),
            min_bytes_per_second: 2_000,
            cap: Duration::from_secs(30),
        },
        ..IngestLimits::default()
    };
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let (mut read, mut write) = tokio::net::TcpStream::connect(served.addr)
        .await
        .unwrap()
        .into_split();
    write
        .write_all(ingest_head(&token, 100_000).as_bytes())
        .await
        .unwrap();
    let started = Instant::now();
    let trickle = tokio::spawn(async move {
        while write.write_all(&[b' '; 200]).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    });
    let reply = first_response(&mut read, WAIT)
        .await
        .expect("the crawling request is answered");
    trickle.abort();
    assert!(reply.starts_with("HTTP/1.1 408"), "{reply}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}: it fell behind after about a second",
        started.elapsed()
    );
    ch.drop_database().await.unwrap();
}

/// The same pace, kept: a body that arrives in pieces fast enough is stored.
#[sqlx::test]
async fn a_body_that_keeps_to_the_minimum_pace_is_stored(pool: sqlx::PgPool) {
    use tokio::io::AsyncWriteExt;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        body_pace: BodyPace {
            grace: Duration::from_millis(300),
            min_bytes_per_second: 4_000,
            cap: Duration::from_secs(30),
        },
        ..IngestLimits::default()
    };
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let body = serde_json::to_string(&vec![json!({ "message": "x".repeat(900) }); 5]).unwrap();
    let mut client = tokio::net::TcpStream::connect(served.addr).await.unwrap();
    client
        .write_all(ingest_head(&token, body.len()).as_bytes())
        .await
        .unwrap();
    for piece in body.as_bytes().chunks(body.len() / 5 + 1) {
        client.write_all(piece).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let reply = first_response(&mut client, WAIT).await.expect("answered");
    assert!(reply.starts_with("HTTP/1.1 202"), "{reply}");
    assert!(reply.contains(r#"{"accepted":5}"#), "{reply}");
    assert_eq!(stored_rows(&ch, project).await.len(), 5);
    ch.drop_database().await.unwrap();
}

/// A client that hangs up (hyper drops the handler the moment it sees the
/// connection end) must not free the place of work that is still running, and
/// must not let its events be stored without being counted. A ClickHouse that
/// takes 1.2 s to answer holds the request in the insert.
#[sqlx::test]
async fn a_client_that_hangs_up_during_the_insert_still_has_its_events_stored_and_counted(
    pool: sqlx::PgPool,
) {
    use tokio::io::AsyncWriteExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1200)))
        .mount(&slow)
        .await;
    let state = AppState {
        clickhouse: ClickHouse::new(&slow.uri(), "nami", "nami", "nami_unused"),
        ..AppState::for_test(pool.clone())
    };
    let logs = state.logs.clone();
    let limits = IngestLimits {
        places: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;
    let today = chrono::Utc::now().date_naive();

    // A complete request. Once its insert is under way, the connection ends.
    let mut leaving = tokio::net::TcpStream::connect(served.addr).await.unwrap();
    leaving
        .write_all(ingest_request(&token, r#"{"message":"left behind"}"#).as_bytes())
        .await
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while slow.received_requests().await.unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the insert never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    leaving.shutdown().await.unwrap();
    // The server drops the request it was serving and closes the connection:
    // from here on it is only the insert that holds the place.
    assert!(
        closed_by_server(&mut leaving, WAIT).await,
        "the server never closed the connection"
    );

    let message = json!({ "message": "next" });
    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the place is held by the insert that is under way, not by the connection"
    );
    assert_eq!(
        logs.in_flight.count(source_id),
        1,
        "and so is the source's count of requests in flight"
    );

    eventually("the events are counted", WAIT, || {
        logs.usage.pending(source_id, today).events == 1
    })
    .await;
    assert!(logs.usage.pending(source_id, today).bytes > 0);
    let inserts = slow.received_requests().await.unwrap();
    assert_eq!(inserts.len(), 1, "the 503 never reached ClickHouse");
    assert_eq!(
        String::from_utf8_lossy(&inserts[0].body).lines().count(),
        1,
        "one event was inserted"
    );

    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "the place was given back");
}

/// The same hang-up while the batch is still waiting for a place to be
/// decoded: the place of the endpoint stays taken until the work ends, and the
/// work still happens.
#[sqlx::test]
async fn a_client_that_hangs_up_while_its_batch_waits_to_be_decoded_keeps_its_place(
    pool: sqlx::PgPool,
) {
    use tokio::io::AsyncWriteExt;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let limits = IngestLimits {
        places: Arc::new(Semaphore::new(1)),
        decodes: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    // Decoding cannot start while the test holds the only place for it.
    let gate = limits.decodes.clone().acquire_owned().await.unwrap();
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;
    let today = chrono::Utc::now().date_naive();

    let mut leaving = tokio::net::TcpStream::connect(served.addr).await.unwrap();
    leaving
        .write_all(ingest_request(&token, r#"{"message":"left behind"}"#).as_bytes())
        .await
        .unwrap();
    // The work counts as a user of its source's decode gate from its first
    // step: once it does, it is running, and waiting for the decode place.
    eventually("the work is waiting to decode", WAIT, || {
        logs.decode_shares.count(source_id) == 1
    })
    .await;
    leaving.shutdown().await.unwrap();
    assert!(
        closed_by_server(&mut leaving, WAIT).await,
        "the server never closed the connection"
    );

    let message = json!({ "message": "next" });
    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(logs.usage.pending(source_id, today).events, 0);
    assert_eq!(
        logs.in_flight.count(source_id),
        1,
        "the source's count stays too"
    );

    drop(gate);
    eventually("the events are counted", WAIT, || {
        logs.usage.pending(source_id, today).events == 1
    })
    .await;
    assert_eq!(stored_rows(&ch, project).await.len(), 1);
    eventually("the place is given back", WAIT, || {
        logs.in_flight.count(source_id) == 0
    })
    .await;
    let (status, _, _) = post_to(served.addr, &token, &message, WAIT).await.unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    ch.drop_database().await.unwrap();
}

/// One token cannot take every place: a source has a few requests in flight at
/// most and is told 429 beyond them, while the other sources are served. The
/// requests a source has in flight are counted until their work ends, so
/// hanging up does not free them early either.
#[sqlx::test]
async fn one_source_cannot_hold_more_than_its_share_of_the_places(pool: sqlx::PgPool) {
    use tokio::io::AsyncWriteExt;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = Arc::new(LogsState {
        in_flight: InFlight::new(2),
        ..LogsState::default()
    });
    let state = AppState {
        logs: logs.clone(),
        ..state
    };
    let served = serve(build_public_router(state)).await;
    let project = seed_project(&pool).await;
    let (a, token_a) = seed_source(&pool, project, "a").await;
    let (_, token_b) = seed_source(&pool, project, "b").await;
    let message = json!({ "message": "m" });

    // Two requests of `a` stall in the middle of their bodies.
    let mut stalled = Vec::new();
    for _ in 0..2 {
        let mut client = tokio::net::TcpStream::connect(served.addr).await.unwrap();
        client
            .write_all(format!("{}{{", ingest_head(&token_a, 1000)).as_bytes())
            .await
            .unwrap();
        stalled.push(client);
    }
    eventually("both are counted", WAIT, || logs.in_flight.count(a) == 2).await;

    let (status, headers, json) = post_to(served.addr, &token_a, &message, WAIT)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{json}");
    assert_eq!(json["code"], "too_many_in_flight");
    assert!(
        json["error"].as_str().unwrap().contains("at most 2"),
        "{json}"
    );
    assert_eq!(headers["retry-after"], "1");
    let (status, _, _) = post_to(served.addr, &token_b, &message, WAIT)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "another source is served");

    logs.usage.flush(&pool).await.unwrap();
    let rejected: i64 = sqlx::query_scalar(
        "select requests_rate_limited from source_usage_daily where source_id = $1",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rejected, 1, "the rejection counts as rate limited");

    // The clients hang up with their bodies unread: the handlers are dropped
    // and the counts go back to zero.
    drop(stalled);
    eventually("the source's count is back to zero", WAIT, || {
        logs.in_flight.count(a) == 0
    })
    .await;
    let (status, _, _) = post_to(served.addr, &token_a, &message, WAIT)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED);
    ch.drop_database().await.unwrap();
}

/// The token lookup and the quota check wait for one of a few places before
/// they ask Postgres, so that ingest never has more statements running than
/// the pool can spare. Waiting for one holds no connection.
#[sqlx::test]
async fn ingest_waits_for_one_of_a_few_places_before_it_asks_postgres(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = IngestLimits {
        queries: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    let public = build_public_router_with_ingest_limits(state, limits.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let held = limits.queries.clone().acquire_owned().await.unwrap();
    let waiting = tokio::time::timeout(
        Duration::from_millis(300),
        post_json(&public, &token, json!({ "message": "m" })),
    )
    .await;
    assert!(waiting.is_err(), "with no place free the request waits");
    drop(held);
    assert_eq!(
        post_json(&public, &token, json!({ "message": "m" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    ch.drop_database().await.unwrap();
}

/// 50 MiB of zeros inside one array is 50 KB gzipped and, decoded, gigabytes of
/// values: the decoders stop at their budget and the request is a 413, in a
/// fraction of the time (and of the memory) that decoding it all would take.
#[sqlx::test]
async fn a_gzip_body_that_decodes_to_gigabytes_is_refused_at_once(pool: sqlx::PgPool) {
    const MIB: usize = 1024 * 1024;
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    // Exactly the 50 MiB gzip allows: a head, then zeros (or `0,`s) to fill
    // it, as one member and 49 of 1 MiB.
    let zeros = gzip(&vec![0u8; MIB]);
    let mut msgpack_bomb = vec![0x81, 0xa1, b'a', 0xdd];
    msgpack_bomb.extend(52_428_792u32.to_be_bytes());
    msgpack_bomb.resize(MIB, 0);
    let mut msgpack_gz = gzip(&msgpack_bomb);
    msgpack_gz.extend(zeros.repeat(49));

    let mut json_bomb = b"{\"a\":[".to_vec();
    json_bomb.extend("0,".repeat((MIB - 6) / 2).bytes());
    let mut json_gz = gzip(&json_bomb);
    json_gz.extend(gzip("0,".repeat(MIB / 2).as_bytes()).repeat(49));

    for (what, headers, body) in [
        ("MessagePack", [MSGPACK, GZIP], msgpack_gz),
        ("JSON", [JSON, GZIP], json_gz),
    ] {
        assert!(
            body.len() < 100 * 1024,
            "{what}: {} bytes on the wire",
            body.len()
        );
        let started = Instant::now();
        let (status, _, json) = post_logs(&public, Some(&token), &headers, body).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{what}: {json}");
        assert!(
            json["error"]
                .as_str()
                .unwrap()
                .contains("expands to more than"),
            "{what}: {json}"
        );
        assert!(started.elapsed() < WAIT, "{what}: {:?}", started.elapsed());
    }
    assert!(stored_rows(&ch, project).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// A second `Content-Encoding` line is a stack of codings, or something a
/// proxy added: refused, not read as its first line alone.
#[sqlx::test]
async fn more_than_one_content_encoding_is_415(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let body = gzip(b"{\"message\":\"gz\"}");

    for extra in ["identity", "gzip", "br"] {
        let headers = [JSON, GZIP, ("content-encoding", extra)];
        let (status, _, json) = post_logs(&public, Some(&token), &headers, body.clone()).await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{extra}: {json}"
        );
    }
    let (status, _, json) = post_logs(&public, Some(&token), &[JSON, GZIP], body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
    assert_eq!(stored_rows(&ch, project).await.len(), 1);
    ch.drop_database().await.unwrap();
}

/// One token cannot fill the engine's few decode places either: a source has
/// a few of its own, and its other bodies wait at a gate of their own, so the
/// other sources are served at once. Here the source has one place and it is
/// taken.
#[sqlx::test]
async fn one_source_cannot_monopolize_the_decode_places(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = Arc::new(LogsState {
        decode_shares: DecodeShares::new(1),
        ..LogsState::default()
    });
    let state = AppState {
        logs: logs.clone(),
        ..state
    };
    let served = serve(build_public_router(state)).await;
    let project = seed_project(&pool).await;
    let (a, token_a) = seed_source(&pool, project, "a").await;
    let (_, token_b) = seed_source(&pool, project, "b").await;
    let today = chrono::Utc::now().date_naive();

    let share = logs.decode_shares.acquire(a).await;
    let waiting = post_to(
        served.addr,
        &token_a,
        &json!({ "message": "a" }),
        Duration::from_millis(500),
    )
    .await;
    assert!(waiting.is_err(), "a has no decode place free: it waits");
    let (status, _, _) = post_to(served.addr, &token_b, &json!({ "message": "b" }), WAIT)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::ACCEPTED, "b is not held back by a");

    // The request of a that gave up waiting is still being served.
    drop(share);
    eventually("a's request is stored after all", WAIT, || {
        logs.usage.pending(a, today).events == 1
    })
    .await;
    assert_eq!(stored_rows(&ch, project).await.len(), 2);
    ch.drop_database().await.unwrap();
}

/// An insert body is held in memory until ClickHouse has taken it, which can
/// be 25 s: the places of the endpoint bound the small ones, but a large one
/// (above 8 MiB) keeps its decode place meanwhile, so that no more than a few
/// exist at once. Here the only decode place is held by the large body's
/// insert, and the next request waits for it.
#[sqlx::test]
async fn a_large_batch_keeps_its_decode_place_until_clickhouse_has_taken_it(pool: sqlx::PgPool) {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1500)))
        .mount(&slow)
        .await;
    let state = AppState {
        clickhouse: ClickHouse::new(&slow.uri(), "nami", "nami", "nami_unused"),
        ..AppState::for_test(pool.clone())
    };
    let logs = state.logs.clone();
    let limits = IngestLimits {
        decodes: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    let decodes = limits.decodes.clone();
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (source_id, token) = seed_source(&pool, project, "api").await;
    let today = chrono::Utc::now().date_naive();

    // 180 events of 64 KB of text: a 10 MB insert body, 12 KB gzipped.
    let large: Vec<Value> = (0..180)
        .map(|n| json!({ "message": format!("{n} {}", "x".repeat(60_000)) }))
        .collect();
    let large = gzip(&serde_json::to_vec(&large).unwrap());
    let sending = tokio::spawn({
        let (addr, token) = (served.addr, token.clone());
        async move {
            post_bytes_to(addr, &token, &[JSON, GZIP], large, WAIT)
                .await
                .unwrap()
                .0
        }
    });
    let deadline = Instant::now() + WAIT;
    while slow.received_requests().await.unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the insert never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        decodes.available_permits(),
        0,
        "the large body holds its place while it is inserted"
    );

    // A small request needs a decode place and has to wait for it; it is
    // still served once the large one is done.
    let small = json!({ "message": "small" });
    let waiting = post_to(served.addr, &token, &small, Duration::from_millis(500)).await;
    assert!(
        waiting.is_err(),
        "the decode place is held by the large body"
    );
    assert_eq!(slow.received_requests().await.unwrap().len(), 1);

    assert_eq!(sending.await.unwrap(), StatusCode::ACCEPTED);
    eventually("the small request is stored too", WAIT, || {
        logs.usage.pending(source_id, today).events == 181
    })
    .await;
    assert_eq!(decodes.available_permits(), 1, "and every place is back");
}

/// A small insert body does not: its place goes back as soon as it is made, so
/// the requests behind it are decoded while it waits for ClickHouse.
#[sqlx::test]
async fn a_small_batch_gives_its_decode_place_back_before_its_insert(pool: sqlx::PgPool) {
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(1500)))
        .mount(&slow)
        .await;
    let state = AppState {
        clickhouse: ClickHouse::new(&slow.uri(), "nami", "nami", "nami_unused"),
        ..AppState::for_test(pool.clone())
    };
    let limits = IngestLimits {
        decodes: Arc::new(Semaphore::new(1)),
        ..IngestLimits::default()
    };
    let decodes = limits.decodes.clone();
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let project = seed_project(&pool).await;
    let (_, token_a) = seed_source(&pool, project, "a").await;
    let (_, token_b) = seed_source(&pool, project, "b").await;

    let first = tokio::spawn({
        let (addr, token) = (served.addr, token_a.clone());
        async move {
            post_to(addr, &token, &json!({ "message": "first" }), WAIT)
                .await
                .unwrap()
                .0
        }
    });
    let deadline = Instant::now() + WAIT;
    while slow.received_requests().await.unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the insert never started");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(decodes.available_permits(), 1, "its place is already back");

    let second = tokio::spawn({
        let (addr, token) = (served.addr, token_b.clone());
        async move {
            post_to(addr, &token, &json!({ "message": "second" }), WAIT)
                .await
                .unwrap()
                .0
        }
    });
    // The second insert reaches ClickHouse while the first is still waiting.
    let deadline = Instant::now() + Duration::from_millis(900);
    while slow.received_requests().await.unwrap().len() < 2 {
        assert!(
            Instant::now() < deadline,
            "the second request was held up behind the first one's insert"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(first.await.unwrap(), StatusCode::ACCEPTED);
    assert_eq!(second.await.unwrap(), StatusCode::ACCEPTED);
}

/// A body that decodes to little can store as a great deal: 479 events of two
/// strings of control characters are 52 MB inflated and 60 KB gzipped, and
/// would have been 150 MiB of rows and a 322 MiB insert body (a control
/// character is six bytes of JSON). It is refused, as one that stays under
/// the cap of rows but not the cap of the insert body is.
#[sqlx::test]
async fn a_body_that_stores_far_more_than_it_decodes_to_is_refused(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;

    let events = |count: usize, field: bool| {
        let mut out = vec![0xdc];
        out.extend((count as u16).to_be_bytes());
        for _ in 0..count {
            out.push(if field { 0x82 } else { 0x81 });
            out.extend([0xa7]);
            out.extend(b"message");
            out.push(0xdb);
            out.extend(65_536u32.to_be_bytes());
            out.extend(vec![1u8; 65_536]);
            if field {
                out.extend([0xa1, b'a', 0xda]);
                out.extend(43_689u16.to_be_bytes());
                out.extend(vec![1u8; 43_689]);
            }
        }
        out
    };
    for (what, body) in [
        ("rows over the cap", events(479, true)),
        ("an insert body over the cap", events(790, false)),
    ] {
        let wire = gzip(&body);
        assert!(
            wire.len() < 100 * 1024,
            "{what}: {} bytes on the wire",
            wire.len()
        );
        let started = Instant::now();
        let (status, _, json) = post_logs(&public, Some(&token), &[MSGPACK, GZIP], wire).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{what}: {json}");
        assert!(
            json["error"].as_str().unwrap().contains("once stored"),
            "{what}: {json}"
        );
        assert!(started.elapsed() < WAIT, "{what}: {:?}", started.elapsed());
    }
    assert!(stored_rows(&ch, project).await.is_empty());
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
    let (source_id, token) = seed_source(&pool, project, "api").await;
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
    // The failed insert is logged where it happened, with its source and no token.
    let failed = output
        .lines()
        .find(|line| line.contains("clickhouse error"))
        .unwrap();
    assert!(
        failed.contains(&source_id.to_string()),
        "the line names the source: {failed}"
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

/// Fairness between projects: however many sources one project has, it holds
/// at most `MAX_IN_FLIGHT_PER_PROJECT` of the endpoint's places, so eight
/// sources each stalling four uploads (enough to fill all 32 places) leave
/// another project served.
#[sqlx::test]
async fn one_project_cannot_hold_every_place_with_many_sources(pool: sqlx::PgPool) {
    use nami_engine::logs::rate_limit::{MAX_IN_FLIGHT_PER_PROJECT, MAX_IN_FLIGHT_PER_SOURCE};
    use nami_engine::routes::logs_ingest::MAX_INGEST_IN_FLIGHT;
    use tokio::io::AsyncWriteExt;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let limits = IngestLimits::default();
    let places = limits.places.clone();
    let served = serve(build_public_router_with_ingest_limits(state, limits)).await;
    let busy = seed_project(&pool).await;
    let mut tokens = Vec::new();
    for n in 0..MAX_INGEST_IN_FLIGHT / MAX_IN_FLIGHT_PER_SOURCE {
        tokens.push(seed_source(&pool, busy, &format!("s{n}")).await.1);
    }
    let mut stalled = Vec::new();
    for token in &tokens {
        for _ in 0..MAX_IN_FLIGHT_PER_SOURCE {
            let mut client = tokio::net::TcpStream::connect(served.addr).await.unwrap();
            client
                .write_all(format!("{}{{", ingest_head(token, 1000)).as_bytes())
                .await
                .unwrap();
            stalled.push(client);
        }
    }
    eventually("the project holds its share", WAIT, || {
        logs.project_limits.in_flight.count(busy) == MAX_IN_FLIGHT_PER_PROJECT
    })
    .await;
    eventually("the requests over its share were turned away", WAIT, || {
        places.available_permits() == MAX_INGEST_IN_FLIGHT - MAX_IN_FLIGHT_PER_PROJECT
    })
    .await;
    let message = json!({ "message": "m" });
    let (status, _, json) = post_to(served.addr, &tokens[0], &message, WAIT)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{json}");
    assert_eq!(json["code"], "too_many_in_flight");
    assert!(
        json["error"].as_str().unwrap().contains("project"),
        "{json}"
    );

    let other = seed_project(&pool).await;
    let (_, other_token) = seed_source(&pool, other, "theirs").await;
    let (status, _, json) = post_to(served.addr, &other_token, &message, WAIT)
        .await
        .unwrap();
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "another project is served: {json}"
    );
    assert_eq!(
        logs.project_limits.in_flight.count(busy),
        MAX_IN_FLIGHT_PER_PROJECT,
        "never more than its share"
    );
    drop(stalled);
    eventually("the project's count is back to zero", WAIT, || {
        logs.project_limits.in_flight.count(busy) == 0
    })
    .await;
    ch.drop_database().await.unwrap();
}
