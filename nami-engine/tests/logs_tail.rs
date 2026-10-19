// Live tail (`GET /projects/{id}/logs/tail`, Server-Sent Events). Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use chrono::{SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json as post_json_raw, seed_source},
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use http_body_util::BodyExt;
use nami_engine::{
    core::state::AppState,
    logs::{
        LogsState,
        query::tail::{MAX_TAILS_PER_USER, TAIL_BATCH, TailTimings},
    },
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn fast(overrides: impl FnOnce(&mut TailTimings)) -> Arc<LogsState> {
    let mut tail = TailTimings {
        poll: Duration::from_millis(100),
        keep_alive: Duration::from_secs(15),
        recheck: Duration::from_secs(60),
        max_duration: Duration::from_secs(60),
        lookback: Duration::from_secs(60),
        send_timeout: Duration::from_secs(10),
        batch: TAIL_BATCH,
    };
    overrides(&mut tail);
    Arc::new(LogsState {
        tail,
        ..LogsState::default()
    })
}

#[derive(Debug, PartialEq)]
enum Sse {
    Comment,
    Event { event: String, data: Value },
}

/// Reads Server-Sent Events off a streaming response body.
struct SseStream {
    body: Body,
    buffer: String,
}

impl SseStream {
    async fn open(
        app: &Router,
        uri: &str,
        cookie: Option<&HeaderValue>,
    ) -> Result<SseStream, (StatusCode, Value)> {
        let mut request = Request::builder().uri(uri);
        if let Some(cookie) = cookie {
            request = request.header(header::COOKIE, cookie);
        }
        let response = app
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        if status != StatusCode::OK {
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            return Err((
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            ));
        }
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(response.headers()["x-accel-buffering"], "no");
        Ok(SseStream {
            body: response.into_body(),
            buffer: String::new(),
        })
    }

    /// The next event or comment; `None` when the stream ended or nothing
    /// arrived within `wait`.
    async fn next(&mut self, wait: Duration) -> Option<Sse> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                return Some(parse_block(block.trim_end()));
            }
            let frame = tokio::time::timeout_at(deadline, self.body.frame())
                .await
                .ok()??
                .ok()?;
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }

    /// The next `event: log` row, skipping comments.
    async fn next_log(&mut self, wait: Duration) -> Option<Value> {
        loop {
            match self.next(wait).await? {
                Sse::Comment => continue,
                Sse::Event { event, data } if event == "log" => return Some(data),
                other => panic!("expected a log event, got {other:?}"),
            }
        }
    }

    /// True when the server closed the stream (rather than just being quiet).
    async fn ended(&mut self) -> bool {
        matches!(
            tokio::time::timeout(Duration::from_secs(5), self.body.frame()).await,
            Ok(None)
        )
    }
}

fn parse_block(block: &str) -> Sse {
    let mut event = None;
    let mut data = String::new();
    for line in block.lines() {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
        }
    }
    match event {
        None if data.is_empty() => Sse::Comment,
        event => Sse::Event {
            event: event.unwrap_or_default(),
            data: serde_json::from_str(&data).unwrap(),
        },
    }
}

/// Ingests through the public router; ingestion must be accepted (202).
async fn post_json(public: &Router, token: &str, body: Value) {
    let (status, json) = post_json_raw(public, token, body).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{json}");
}

fn ago(seconds: i64) -> String {
    (Utc::now() - chrono::Duration::seconds(seconds)).to_rfc3339_opts(SecondsFormat::Millis, true)
}

#[sqlx::test]
async fn tail_streams_new_matching_rows_including_late_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        logs: fast(|_| {}),
        ..state
    };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, other_token) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(
        &public,
        &token,
        json!({ "message": "before the tail", "level": "error" }),
    )
    .await;

    let mut stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail?level=error"),
        Some(&viewer),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    post_json(&public, &token, json!([{ "message": "ignored info", "level": "info" }, { "message": "first error", "level": "error" }])).await;
    let row = stream
        .next_log(Duration::from_secs(5))
        .await
        .expect("a row");
    assert_eq!(
        (row["message"].clone(), row["level"].clone()),
        (json!("first error"), json!("error"))
    );
    assert!(
        row["fields"].is_object() && row["pattern_hash"].is_string(),
        "{row}"
    );

    // A late row (dt 30 s ago, older than the row just sent) still arrives;
    // another project's row never does.
    post_json(
        &public,
        &other_token,
        json!({ "message": "not yours", "level": "error" }),
    )
    .await;
    post_json(
        &public,
        &token,
        json!({ "message": "late error", "level": "error", "dt": ago(30) }),
    )
    .await;
    let late = stream
        .next_log(Duration::from_secs(5))
        .await
        .expect("the late row");
    assert_eq!(late["message"], "late error");
    assert_eq!(
        stream.next(Duration::from_millis(700)).await,
        None,
        "nothing twice, nothing foreign, nothing from before"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn idle_streams_get_keep_alive_comments(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState {
        logs: fast(|t| t.keep_alive = Duration::from_millis(100)),
        ..state
    });
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail"),
        Some(&viewer),
    )
    .await
    .unwrap();
    assert_eq!(
        stream.next(Duration::from_secs(2)).await,
        Some(Sse::Comment)
    );
    ch.drop_database().await.unwrap();
}

/// Security: a stream ends once its session is no longer valid — logged
/// out, or the user lost access to the project.
#[sqlx::test]
async fn the_stream_ends_when_the_session_or_access_is_gone(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState {
        logs: fast(|t| t.recheck = Duration::from_millis(200)),
        ..state
    });
    let project = seed_project(&pool).await;
    let (_, _, leaving) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (removed_id, _, removed) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (changed_id, _, changed) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, staying) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");
    let mut logged_out = SseStream::open(&app, &uri, Some(&leaving)).await.unwrap();
    let mut kicked = SseStream::open(&app, &uri, Some(&removed)).await.unwrap();
    let mut password_changed = SseStream::open(&app, &uri, Some(&changed)).await.unwrap();
    let mut kept = SseStream::open(&app, &uri, Some(&staying)).await.unwrap();

    assert_eq!(
        call(
            &app,
            "POST",
            "/auth/logout",
            Some(&leaving),
            Some(json!({}))
        )
        .await
        .0,
        StatusCode::OK
    );
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project)
        .bind(removed_id)
        .execute(&pool)
        .await
        .unwrap();
    // A session older than the user's last password change is no longer valid.
    sqlx::query("update users set password_changed_at = now() + interval '1 second' where id = $1")
        .bind(changed_id)
        .execute(&pool)
        .await
        .unwrap();

    for stream in [&mut logged_out, &mut kicked, &mut password_changed] {
        let end = stream.next(Duration::from_secs(3)).await;
        assert_eq!(
            end,
            Some(Sse::Event {
                event: "end".to_string(),
                data: json!({ "reason": "session" })
            })
        );
        assert!(
            stream.ended().await,
            "the stream is closed after the end event"
        );
    }
    assert_eq!(
        kept.next(Duration::from_millis(500)).await,
        None,
        "a valid session keeps streaming"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_stream_closes_after_its_maximum_duration(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState {
        logs: fast(|t| t.max_duration = Duration::from_millis(400)),
        ..state
    });
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail"),
        Some(&viewer),
    )
    .await
    .unwrap();
    let end = stream.next(Duration::from_secs(3)).await;
    assert_eq!(
        end,
        Some(Sse::Event {
            event: "end".to_string(),
            data: json!({ "reason": "timeout" })
        })
    );
    assert!(stream.ended().await);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_tail_needs_a_session_project_access_and_valid_filters(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(AppState {
        logs: fast(|_| {}),
        ..state
    });
    let project = seed_project(&pool).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");
    assert_eq!(
        SseStream::open(&app, &uri, None).await.err().unwrap().0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        SseStream::open(&app, &uri, Some(&outsider))
            .await
            .err()
            .unwrap()
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, json) = SseStream::open(&app, &format!("{uri}?level=verbose"), Some(&viewer))
        .await
        .err()
        .unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    let random = Uuid::new_v4();
    assert_eq!(
        SseStream::open(
            &app,
            &format!("/projects/{random}/logs/tail"),
            Some(&viewer)
        )
        .await
        .err()
        .unwrap()
        .0,
        StatusCode::FORBIDDEN
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn streams_are_capped_per_user_and_freed_when_the_client_disconnects(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        logs: fast(|_| {}),
        ..state
    };
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (user_id, email, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, other) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/tail");

    let mut streams = Vec::new();
    for _ in 0..MAX_TAILS_PER_USER {
        streams.push(SseStream::open(&app, &uri, Some(&viewer)).await.unwrap());
    }
    assert_eq!(
        logs.tail_slots.open(user_id).1,
        MAX_TAILS_PER_USER,
        "{email}"
    );
    let (status, _) = SseStream::open(&app, &uri, Some(&viewer))
        .await
        .err()
        .unwrap();
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        SseStream::open(&app, &uri, Some(&other)).await.is_ok(),
        "the cap is per user"
    );

    // Disconnecting frees the slot: the streaming task ends on its own.
    drop(streams.pop());
    let mut reopened = None;
    for _ in 0..50 {
        match SseStream::open(&app, &uri, Some(&viewer)).await {
            Ok(stream) => {
                reopened = Some(stream);
                break;
            }
            Err((status, _)) => assert_eq!(status, StatusCode::TOO_MANY_REQUESTS),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        reopened.is_some(),
        "the slot of a disconnected stream is released"
    );

    drop(streams);
    drop(reopened);
    for _ in 0..50 {
        if logs.tail_slots.open(user_id).1 == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(logs.tail_slots.open(user_id).1, 0, "no leaked task or slot");
    ch.drop_database().await.unwrap();
}

/// A client that stops reading must not pin its slot: the bounded send ends
/// the stream (and so releases the slot) even though the body is never read.
#[sqlx::test]
async fn a_client_that_stops_reading_loses_its_stream_and_slot(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        logs: fast(|t| {
            t.send_timeout = Duration::from_millis(300);
            t.max_duration = Duration::from_secs(30);
        }),
        ..state
    };
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (user_id, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail"),
        Some(&viewer),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    // More rows than the channel holds, and nobody reads.
    let rows: Vec<Value> = (0..150)
        .map(|i| json!({ "message": format!("row {i}") }))
        .collect();
    post_json(&public, &token, Value::Array(rows)).await;
    assert_eq!(logs.tail_slots.open(user_id).1, 1);
    let mut freed = false;
    for _ in 0..60 {
        if logs.tail_slots.open(user_id).1 == 0 {
            freed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(freed, "the slot of a stalled client is released");
    drop(stream);
    ch.drop_database().await.unwrap();
}

/// Rows dated in the future must not fill the newest-first batch and hide
/// current rows.
#[sqlx::test]
async fn future_dated_rows_do_not_starve_current_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        logs: fast(|t| t.batch = 5),
        ..state
    };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail"),
        Some(&viewer),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let future =
        (Utc::now() + chrono::Duration::hours(1)).to_rfc3339_opts(SecondsFormat::Millis, true);
    let rows: Vec<Value> = (0..8)
        .map(|i| json!({ "message": format!("future {i}"), "dt": future }))
        .collect();
    post_json(&public, &token, Value::Array(rows)).await;
    post_json(&public, &token, json!({ "message": "current" })).await;
    let row = stream
        .next_log(Duration::from_secs(5))
        .await
        .expect("the current row");
    assert_eq!(row["message"], "current");
    assert_eq!(
        stream.next(Duration::from_millis(500)).await,
        None,
        "future rows wait for their time"
    );
    ch.drop_database().await.unwrap();
}

/// Security (memory amplification): the tail sends list rows, long messages
/// cut and large fields left out, flagged `truncated`, and a poll reads only
/// the rows it has not sent.
#[sqlx::test]
async fn tailed_rows_are_cut_like_list_rows(pool: sqlx::PgPool) {
    use nami_engine::logs::query::search::{LIST_FIELDS_OMITTED, LIST_MESSAGE_CHARS};

    let (state, ch) = state_with_clickhouse(&pool).await;
    let state = AppState {
        logs: fast(|_| {}),
        ..state
    };
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let mut stream = SseStream::open(
        &app,
        &format!("/projects/{project}/logs/tail"),
        Some(&viewer),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let pad = "\u{1}".repeat(40_000);
    post_json(
        &public,
        &token,
        json!([{ "message": "\u{1}".repeat(60_000), "pad": pad }, { "message": "small", "k": 1 }]),
    )
    .await;
    let big = stream
        .next_log(Duration::from_secs(5))
        .await
        .expect("a row");
    let small = stream
        .next_log(Duration::from_secs(5))
        .await
        .expect("a row");
    assert_eq!(big["truncated"], true);
    assert_eq!(
        big["message"].as_str().unwrap().chars().count(),
        LIST_MESSAGE_CHARS
    );
    assert_eq!(
        big["fields"],
        serde_json::from_str::<Value>(LIST_FIELDS_OMITTED).unwrap()
    );
    assert_eq!(small["truncated"], false);
    assert_eq!(small["fields"], json!({ "k": 1 }));
    assert_eq!(
        stream.next(Duration::from_millis(700)).await,
        None,
        "nothing twice"
    );
    ch.drop_database().await.unwrap();
}
