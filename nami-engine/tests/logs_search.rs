// Log search and histogram (`GET /projects/{id}/logs`, `/logs/histogram`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::collections::HashSet;

use axum::{Router, http::StatusCode};
use chrono::{DateTime, Duration, DurationRound, SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json, seed_source},
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use serde_json::{Value, json};
use uuid::Uuid;

fn ts(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn parse_ts(value: &Value) -> DateTime<Utc> {
    value.as_str().unwrap().parse().unwrap()
}

fn filter(value: Value) -> String {
    format!("filter={}", encode(&value.to_string()))
}

struct Fixture {
    app: Router,
    public: Router,
    ch: nami_engine::core::clickhouse::ClickHouse,
    project: Uuid,
    other_project: Uuid,
    api: Uuid,
    worker: Uuid,
    foreign_source: Uuid,
    viewer: axum::http::HeaderValue,
    base: DateTime<Utc>,
}

const EVIL: &str = "'; DROP TABLE logs; -- {p0:String} sleep(3) back\\slash\nnext line";

/// Five rows in `project` (two sources) and one in `other_project`, one
/// second apart from `base` (30 minutes ago).
async fn fixture(pool: &sqlx::PgPool) -> Fixture {
    let (state, ch) = state_with_clickhouse(pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(pool).await;
    let other_project = seed_project(pool).await;
    let (api, api_token) = seed_source(pool, project, "api").await;
    let (worker, worker_token) = seed_source(pool, project, "worker").await;
    let (foreign_source, foreign_token) = seed_source(pool, other_project, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, pool, project, Some("viewer")).await;
    // A multiple of 10 s, so histogram buckets of 2 s start exactly at base.
    let base = (Utc::now() - Duration::minutes(30))
        .duration_trunc(Duration::seconds(10))
        .unwrap();
    let at = |s: i64| ts(base + Duration::seconds(s));
    let accepted = (StatusCode::ACCEPTED, json!({ "accepted": 1 }));
    for (token, event) in [
        (
            &api_token,
            json!({ "dt": at(1), "level": "info", "message": "user 42 logged in from 10.0.0.7",
            "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7",
            "status": 200, "latency_ms": 12.5, "user": { "name": "O'Brien", "tier": "gold" } }),
        ),
        (
            &api_token,
            json!({ "dt": at(2), "level": "error", "message": "Disk FULL on /var",
            "status": "500", "path": "/api/orders" }),
        ),
        (
            &worker_token,
            json!({ "dt": at(3), "level": "warn", "message": "retrying job 7",
            "status": 404, "duration": "250" }),
        ),
        (
            &worker_token,
            json!({ "dt": at(4), "level": "debug", "message": "100% done_x" }),
        ),
        (
            &api_token,
            json!({ "dt": at(5), "level": "info", "message": EVIL, "note": "'; DROP TABLE logs; --" }),
        ),
        (
            &foreign_token,
            json!({ "dt": at(3), "level": "error", "message": "other project secret", "status": 500 }),
        ),
    ] {
        assert_eq!(post_json(&public, token, event).await, accepted);
    }
    Fixture {
        app,
        public,
        ch,
        project,
        other_project,
        api,
        worker,
        foreign_source,
        viewer,
        base,
    }
}

impl Fixture {
    fn window(&self) -> String {
        format!(
            "from={}&to={}",
            encode(&ts(self.base - Duration::minutes(1))),
            encode(&ts(Utc::now()))
        )
    }

    async fn get(&self, path: &str, query: &str) -> (StatusCode, Value) {
        let uri = format!(
            "/projects/{}/logs{path}?{}&{query}",
            self.project,
            self.window()
        );
        call(&self.app, "GET", &uri, Some(&self.viewer), None).await
    }

    /// The messages a search returns, in order.
    async fn messages(&self, query: &str) -> Vec<String> {
        let (status, page) = self.get("", query).await;
        assert_eq!(status, StatusCode::OK, "{query}: {page}");
        page["logs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["message"].as_str().unwrap().to_string())
            .collect()
    }
}

const M1: &str = "user 42 logged in from 10.0.0.7";
const M2: &str = "Disk FULL on /var";
const M3: &str = "retrying job 7";
const M4: &str = "100% done_x";

#[sqlx::test]
async fn search_is_newest_first_and_confined_to_the_project(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (status, page) = f.get("", "").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let logs = page["logs"].as_array().unwrap();
    let messages: Vec<&str> = logs
        .iter()
        .map(|row| row["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages, [EVIL, M4, M3, M2, M1]);
    assert_eq!(page["next_cursor"], Value::Null);
    let first = logs.last().unwrap();
    assert_eq!(
        first["fields"],
        json!({ "status": 200, "latency_ms": 12.5, "user": { "name": "O'Brien", "tier": "gold" } })
    );
    assert_eq!(first["source_id"], f.api.to_string());
    assert_eq!(first["trace_id"], "4bf92f3577b34da6a3ce929d0e0e4736");
    assert!(
        first["pattern_hash"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .is_ok(),
        "pattern_hash is a decimal string"
    );
    assert_eq!(parse_ts(&first["dt"]), f.base + Duration::seconds(1));

    // Security: another project's source id matches nothing here.
    assert!(
        f.messages(&format!("source_id={}", f.foreign_source))
            .await
            .is_empty()
    );
    let other = format!("/projects/{}/logs?{}", f.other_project, f.window());
    assert_eq!(
        call(&f.app, "GET", &other, Some(&f.viewer), None).await.0,
        StatusCode::FORBIDDEN
    );
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn pagination_returns_every_row_exactly_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let base = (Utc::now() - Duration::minutes(10))
        .duration_trunc(Duration::seconds(1))
        .unwrap();
    // Seven rows; three share one timestamp, so the id breaks the tie.
    let events: Vec<Value> = [0, 1, 1, 1, 2, 3, 4]
        .iter()
        .enumerate()
        .map(|(i, s)| json!({ "dt": ts(base + Duration::seconds(*s)), "message": format!("m{i}") }))
        .collect();
    assert_eq!(
        post_json(&public, &token, Value::Array(events)).await.0,
        StatusCode::ACCEPTED
    );

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut sizes = Vec::new();
    loop {
        let mut uri = format!(
            "/projects/{project}/logs?limit=2&from={}",
            encode(&ts(base - Duration::minutes(1)))
        );
        if let Some(c) = &cursor {
            uri.push_str(&format!("&cursor={c}"));
        }
        let (status, page) = call(&app, "GET", &uri, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        let logs = page["logs"].as_array().unwrap();
        sizes.push(logs.len());
        seen.extend(logs.iter().map(|row| {
            (
                row["dt"].as_str().unwrap().to_string(),
                row["id"].as_str().unwrap().to_string(),
            )
        }));
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_string()),
            None => break,
        }
    }
    assert_eq!(sizes, [2, 2, 2, 1]);
    let ids: HashSet<&String> = seen.iter().map(|(_, id)| id).collect();
    assert_eq!(ids.len(), 7, "no row twice, none skipped");
    assert!(
        seen.windows(2).all(|pair| pair[0].0 >= pair[1].0),
        "newest first: {seen:?}"
    );
    for query in ["limit=0", "limit=501", "cursor=bogus"] {
        let (status, _) = call(
            &app,
            "GET",
            &format!("/projects/{project}/logs?{query}"),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn levels_sources_text_and_ids_narrow_the_results(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    assert_eq!(f.messages("level=error&level=WARN").await, [M3, M2]);
    assert_eq!(
        f.messages(&format!("source_id={}", f.worker)).await,
        [M4, M3]
    );
    assert_eq!(
        f.messages(&format!("source_id={}&source_id={}", f.worker, f.api))
            .await
            .len(),
        5
    );
    assert_eq!(
        f.messages("q=disk%20full").await,
        [M2],
        "q is case-insensitive"
    );
    assert_eq!(
        f.messages("q=100%25").await,
        [M4],
        "% is literal, not a wildcard"
    );
    assert_eq!(f.messages("q=_").await, [M4], "_ is literal too");
    assert!(
        f.messages("q=orders").await.is_empty(),
        "fields are searched only with in_fields"
    );
    assert_eq!(f.messages("q=orders&in_fields=true").await, [M2]);
    assert_eq!(
        f.messages("q=ORDERS&in_fields=true").await,
        [M2],
        "in_fields is case-insensitive too"
    );
    assert_eq!(
        f.messages("trace_id=4BF92F3577B34DA6A3CE929D0E0E4736")
            .await,
        [M1]
    );
    assert_eq!(f.messages("span_id=00f067aa0ba902b7").await, [M1]);
    let (_, page) = f.get("", "").await;
    let hash = page["logs"].as_array().unwrap().last().unwrap()["pattern_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(f.messages(&format!("pattern_hash={hash}")).await, [M1]);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn field_filters_support_every_operator(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    for (filters, expected) in [
        (
            vec![json!({ "path": "status", "op": "eq", "value": 200 })],
            vec![M1],
        ),
        (
            vec![json!({ "path": "status", "op": "eq", "value": "500" })],
            vec![M2],
        ),
        (
            vec![json!({ "path": "status", "op": "neq", "value": "200" })],
            vec![EVIL, M4, M3, M2],
        ),
        (
            vec![json!({ "path": "user.name", "op": "contains", "value": "BRIEN" })],
            vec![M1],
        ),
        (vec![json!({ "path": "path", "op": "exists" })], vec![M2]),
        (
            vec![json!({ "path": "path", "op": "regex", "value": "^/api/" })],
            vec![M2],
        ),
        (
            vec![json!({ "path": "status", "op": "not_regex", "value": "^5" })],
            vec![EVIL, M4, M3, M1],
        ),
        (
            vec![json!({ "path": "status", "op": "gt", "value": 300 })],
            vec![M3, M2],
        ),
        (
            vec![json!({ "path": "latency_ms", "op": "gte", "value": "12.5" })],
            vec![M1],
        ),
        (
            vec![json!({ "path": "duration", "op": "lt", "value": 300 })],
            vec![M3],
        ),
        (
            vec![json!({ "path": "status", "op": "lte", "value": 200 })],
            vec![M1],
        ),
        (
            vec![json!({ "path": "user.tier", "op": "eq", "value": "gold" })],
            vec![M1],
        ),
        (
            vec![json!({ "path": "user.name", "op": "gt", "value": 0 })],
            vec![],
        ),
        (
            vec![
                json!({ "path": "user", "op": "eq", "value": "{\"name\":\"O'Brien\",\"tier\":\"gold\"}" }),
            ],
            vec![M1],
        ),
        (
            vec![
                json!({ "path": "status", "op": "gt", "value": 100 }),
                json!({ "path": "path", "op": "exists" }),
            ],
            vec![M2],
        ),
    ] {
        let query = filters
            .iter()
            .map(|value| filter(value.clone()))
            .collect::<Vec<_>>()
            .join("&");
        assert_eq!(f.messages(&query).await, expected, "{filters:?}");
    }
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn invalid_parameters_are_400(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let project = f.project;
    let now = Utc::now();
    for query in [
        format!(
            "from={}&to={}",
            encode(&ts(now - Duration::days(32))),
            encode(&ts(now))
        ),
        format!(
            "from={}&to={}",
            encode(&ts(now)),
            encode(&ts(now - Duration::hours(1)))
        ),
        "from=yesterday".to_string(),
        "source_id=not-a-uuid".to_string(),
        "level=verbose".to_string(),
        "in_fields=maybe".to_string(),
        "trace_id=xyz".to_string(),
        "pattern_hash=-1".to_string(),
        format!("q={}", "x".repeat(1025)),
        "filter=not-json".to_string(),
        filter(json!({ "path": "a", "op": "like", "value": "x" })),
        filter(json!({ "path": "a.b.c.d.e.f", "op": "exists" })),
        filter(json!({ "path": "a;b", "op": "exists" })),
        filter(json!({ "path": "status", "op": "gt", "value": "abc" })),
        filter(json!({ "path": "status", "op": "regex", "value": "(a" })),
        filter(json!({ "path": "status", "op": "regex", "value": "a".repeat(257) })),
        // Valid for Rust's regex crate (verbose mode), rejected by ClickHouse's RE2.
        filter(json!({ "path": "status", "op": "regex", "value": "(?x)5 0 0" })),
        (0..11)
            .map(|_| filter(json!({ "path": "a", "op": "exists" })))
            .collect::<Vec<_>>()
            .join("&"),
    ] {
        for path in ["", "/histogram"] {
            let (status, json) = call(
                &f.app,
                "GET",
                &format!("/projects/{project}/logs{path}?{query}"),
                Some(&f.viewer),
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path}?{query}: {json}");
            assert!(json["error"].is_string(), "{json}");
        }
    }
    f.ch.drop_database().await.unwrap();
}

/// Security: hostile text in q, filter values and paths is matched
/// literally or rejected, never executed.
#[sqlx::test]
async fn injection_attempts_are_matched_literally_or_rejected(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    for q in [
        "'; DROP TABLE logs; --",
        "{p0:String}",
        "sleep(3)",
        "back\\slash",
        "slash\nnext",
        "logs; --",
    ] {
        assert_eq!(
            f.messages(&format!("q={}", encode(q))).await,
            [EVIL],
            "{q:?}"
        );
    }
    let started = std::time::Instant::now();
    assert!(
        f.messages(&format!("q={}", encode("sleep(3) or 1=1")))
            .await
            .is_empty()
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "sleep(3) was not executed"
    );
    for value in [
        json!({ "path": "note", "op": "eq", "value": "'; DROP TABLE logs; --" }),
        json!({ "path": "note", "op": "contains", "value": "drop table" }),
        json!({ "path": "note", "op": "regex", "value": "^'; DROP" }),
    ] {
        assert_eq!(f.messages(&filter(value.clone())).await, [EVIL], "{value}");
    }
    for path in [
        "note'",
        "note\"",
        "a) OR (1=1",
        "note\nx",
        "note\\",
        "fields",
        "a.b.c.d.e.f",
    ] {
        let (status, _) = f
            .get("", &filter(json!({ "path": path, "op": "exists" })))
            .await;
        if path == "fields" {
            assert_eq!(
                status,
                StatusCode::OK,
                "a plain key named fields is just a key"
            );
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{path:?}");
        }
    }
    for query in [
        "level=error%27%20OR%20%271%27%3D%271",
        "source_id=x%27%20OR%201%3D1%20--",
    ] {
        assert_eq!(f.get("", query).await.0, StatusCode::BAD_REQUEST, "{query}");
    }
    // Nothing was dropped or changed.
    assert_eq!(f.messages("").await.len(), 5);
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_histogram_counts_rows_per_bucket_with_a_level_breakdown(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let window = format!(
        "from={}&to={}",
        encode(&ts(f.base - Duration::seconds(60))),
        encode(&ts(f.base + Duration::seconds(60)))
    );
    let uri = format!("/projects/{}/logs/histogram?{window}", f.project);
    let (status, histogram) = call(&f.app, "GET", &uri, Some(&f.viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{histogram}");
    assert_eq!(histogram["step_seconds"], 2);
    let buckets = histogram["buckets"].as_array().unwrap();
    assert_eq!(buckets.len(), 61);
    assert_eq!(
        histogram["total"], 5,
        "the other project's row is not counted"
    );
    assert_eq!(
        buckets
            .iter()
            .map(|b| b["count"].as_u64().unwrap())
            .sum::<u64>(),
        5
    );
    assert_eq!(
        buckets
            .iter()
            .map(|b| b["errors"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
    assert_eq!(
        buckets
            .iter()
            .map(|b| b["warnings"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
    let at = |seconds: i64| {
        let start = f.base + Duration::seconds(seconds);
        buckets
            .iter()
            .find(|b| parse_ts(&b["start"]) == start)
            .unwrap()["count"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(at(0), 1, "base+1s falls in the bucket starting at base");
    assert_eq!(at(2), 2);
    assert_eq!(at(4), 2);
    assert!(
        buckets
            .windows(2)
            .all(|pair| parse_ts(&pair[0]["start"]) + Duration::seconds(2)
                == parse_ts(&pair[1]["start"]))
    );

    let (_, errors_only) = call(
        &f.app,
        "GET",
        &format!("{uri}&level=error"),
        Some(&f.viewer),
        None,
    )
    .await;
    assert_eq!(errors_only["total"], 1);
    let (_, hour) = call(
        &f.app,
        "GET",
        &format!("/projects/{}/logs/histogram", f.project),
        Some(&f.viewer),
        None,
    )
    .await;
    assert_eq!(hour["step_seconds"], 60);
    assert_eq!(hour["total"], 5);
    assert!((60..=61).contains(&hour["buckets"].as_array().unwrap().len()));
    f.ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn search_needs_a_session_and_project_access(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (_, _, outsider) = user_with_role(&f.app, &pool, f.project, None).await;
    for path in ["/logs", "/logs/histogram"] {
        let uri = format!("/projects/{}{path}", f.project);
        assert_eq!(
            call(&f.app, "GET", &uri, None, None).await.0,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(&f.app, "GET", &uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&f.app, "GET", &uri, Some(&f.viewer), None).await.0,
            StatusCode::OK
        );
    }
    f.ch.drop_database().await.unwrap();
}

/// A pattern that backtracks catastrophically in a backtracking engine runs
/// in linear time under RE2.
#[sqlx::test]
async fn catastrophic_regexes_finish_quickly(pool: sqlx::PgPool) {
    let f = fixture(&pool).await;
    let (_, token) = seed_source(&pool, f.project, "big").await;
    let event = json!({ "dt": ts(f.base + Duration::seconds(10)), "message": "big", "s": format!("{}!", "a".repeat(60_000)) });
    assert_eq!(
        post_json(&f.public, &token, event).await.0,
        StatusCode::ACCEPTED
    );
    let started = std::time::Instant::now();
    let found = f
        .messages(&filter(
            json!({ "path": "s", "op": "regex", "value": "(a+)+$" }),
        ))
        .await;
    assert!(found.is_empty(), "{found:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    f.ch.drop_database().await.unwrap();
}

/// The raw body of a GET through the API router.
async fn get_raw(
    app: &Router,
    uri: &str,
    cookie: &axum::http::HeaderValue,
) -> (StatusCode, Vec<u8>) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .uri(uri)
        .header(axum::http::header::COOKIE, cookie)
        .body(axum::body::Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

/// Security (memory amplification): rows stored at the caps and filled with
/// control characters (one byte stored, six once JSON-escaped) are listed
/// cut, so what a page weighs does not depend on what senders stored; the
/// entry endpoint returns one whole row, confined to the project.
#[sqlx::test]
async fn list_rows_are_cut_and_the_entry_endpoint_returns_the_whole_row(pool: sqlx::PgPool) {
    use nami_engine::logs::{
        normalize::LogEvent,
        search::{LIST_FIELDS_OMITTED, LIST_MESSAGE_CHARS},
        store,
    };
    use nami_engine::models::source::IngestSource;

    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "api").await;
    let (other_source, _) = seed_source(&pool, other_project, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let source = |id, project_id| IngestSource {
        id,
        project_id,
        retention_days: 30,
        daily_byte_quota: None,
        updated_at: chrono::Utc::now(),
    };
    let pad = "\u{1}".repeat(40_000);
    let base = (Utc::now() - Duration::minutes(5))
        .duration_trunc(Duration::milliseconds(1))
        .unwrap();
    const BIG_ROWS: i64 = 40;
    let mut rows: Vec<store::LogRow> = (0..BIG_ROWS)
        .map(|i| {
            let mut fields = serde_json::Map::new();
            fields.insert("pad".to_string(), json!(pad));
            store::build_row(
                LogEvent::new(
                    base + Duration::milliseconds(i),
                    "info",
                    "\u{1}".repeat(70_000),
                    fields,
                    String::new(),
                    String::new(),
                ),
                &source(source_id, project),
            )
        })
        .collect();
    let mut small_fields = serde_json::Map::new();
    small_fields.insert("k".to_string(), json!("v"));
    rows.push(store::build_row(
        LogEvent::new(
            base + Duration::seconds(1),
            "info",
            "small".to_string(),
            small_fields,
            String::new(),
            String::new(),
        ),
        &source(source_id, project),
    ));
    let foreign = store::build_row(
        LogEvent::new(
            base,
            "info",
            "theirs".to_string(),
            serde_json::Map::new(),
            String::new(),
            String::new(),
        ),
        &source(other_source, other_project),
    );
    rows.push(foreign.clone());
    store::insert_logs(&ch, &rows).await.unwrap();
    let stored_message = rows[0].message.clone();
    assert!(stored_message.len() > 60_000);

    let window = format!(
        "from={}&to={}",
        encode(&ts(base - Duration::minutes(1))),
        encode(&ts(Utc::now()))
    );
    let (status, body) = get_raw(
        &app,
        &format!("/projects/{project}/logs?{window}&limit=500"),
        &viewer,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Each cut row: 4096 characters of six escaped bytes, the marker, and the
    // rest of the row. Uncut, these rows weigh about 600 KB each.
    let per_row = LIST_MESSAGE_CHARS * 6 + LIST_FIELDS_OMITTED.len() + 1024;
    assert!(
        body.len() < (BIG_ROWS as usize + 1) * per_row,
        "{} bytes for {} rows",
        body.len(),
        BIG_ROWS + 1
    );
    let page: Value = serde_json::from_slice(&body).unwrap();
    let logs = page["logs"].as_array().unwrap();
    assert_eq!(logs.len(), BIG_ROWS as usize + 1);
    let small = &logs[0];
    assert_eq!(small["message"], "small");
    assert_eq!(small["fields"], json!({ "k": "v" }));
    assert_eq!(small["truncated"], false);
    let big = &logs[1];
    assert_eq!(big["truncated"], true);
    assert_eq!(
        big["message"].as_str().unwrap().chars().count(),
        LIST_MESSAGE_CHARS
    );
    assert_eq!(
        big["fields"],
        serde_json::from_str::<Value>(LIST_FIELDS_OMITTED).unwrap()
    );

    // The whole row, by id and dt.
    let id = big["id"].as_str().unwrap();
    let dt = big["dt"].as_str().unwrap();
    let entry_uri = |id: &str, dt: &str| format!("/projects/{project}/logs/{id}?dt={}", encode(dt));
    let (status, entry) = call(&app, "GET", &entry_uri(id, dt), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{entry}");
    assert_eq!(entry["truncated"], false);
    assert_eq!(entry["message"].as_str().unwrap(), stored_message);
    assert_eq!(entry["fields"]["pad"].as_str().unwrap(), pad);
    assert_eq!(entry["id"], big["id"]);

    // Confined to the project; the right id at the wrong dt is not found.
    let foreign_dt = ts(foreign.dt);
    let (status, _) = call(
        &app,
        "GET",
        &entry_uri(&foreign.id.to_string(), &foreign_dt),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let other_dt = ts(parse_ts(&big["dt"]) + Duration::milliseconds(500));
    assert_eq!(
        call(&app, "GET", &entry_uri(id, &other_dt), Some(&viewer), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "GET", &entry_uri(id, dt), Some(&outsider), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", &entry_uri(id, dt), None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    for bad in [
        format!("/projects/{project}/logs/{id}"),
        format!("/projects/{project}/logs/{id}?dt=yesterday"),
    ] {
        assert_eq!(
            call(&app, "GET", &bad, Some(&viewer), None).await.0,
            StatusCode::BAD_REQUEST,
            "{bad}"
        );
    }
    // Another project's path is 403 for this viewer, whatever the row.
    assert_eq!(
        call(
            &app,
            "GET",
            &format!(
                "/projects/{other_project}/logs/{}?dt={}",
                foreign.id,
                encode(&foreign_dt)
            ),
            Some(&viewer),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    ch.drop_database().await.unwrap();
}

/// Security: one user can have at most `MAX_QUERIES_PER_USER` searches,
/// histograms and entry reads running; the next is 429, and a finished one
/// frees its slot.
#[sqlx::test]
async fn log_reads_are_capped_per_user(pool: sqlx::PgPool) {
    use nami_engine::logs::search::{MAX_QUERIES_PER_USER, QueryPermit};

    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (viewer_id, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, other) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let held: Vec<QueryPermit> = (0..MAX_QUERIES_PER_USER)
        .map(|_| QueryPermit::acquire(&logs, viewer_id).unwrap())
        .collect();
    let paths = [
        format!("/projects/{project}/logs"),
        format!("/projects/{project}/logs/histogram"),
        format!(
            "/projects/{project}/logs/{}?dt={}",
            Uuid::new_v4(),
            encode(&ts(Utc::now()))
        ),
    ];
    // Entry reads (`paths[2]`) have their own cap, see
    // `entry_reads_do_not_compete_with_searches`.
    for path in &paths[..2] {
        assert_eq!(
            call(&app, "GET", path, Some(&viewer), None).await.0,
            StatusCode::TOO_MANY_REQUESTS,
            "{path}"
        );
    }
    // Another user is not affected, and nothing leaked a slot.
    assert_eq!(
        call(&app, "GET", &paths[0], Some(&other), None).await.0,
        StatusCode::OK
    );
    drop(held);
    assert_eq!(
        call(&app, "GET", &paths[0], Some(&viewer), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &paths[2], Some(&viewer), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(logs.query_slots.open(viewer_id), 0);
    assert_eq!(logs.entry_slots.open(viewer_id), 0);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn entry_reads_do_not_compete_with_searches(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!({ "message": "one" })).await;
    let (_, page) = call(
        &app,
        "GET",
        &format!("/projects/{project}/logs"),
        Some(&viewer),
        None,
    )
    .await;
    let row = &page["logs"][0];
    let entry = format!(
        "/projects/{project}/logs/{}?dt={}",
        row["id"].as_str().unwrap(),
        encode(row["dt"].as_str().unwrap())
    );

    // Every search slot taken (a page, a histogram, a load-more, ...): entry reads still work.
    let searches: Vec<_> = (0..nami_engine::logs::search::MAX_QUERIES_PER_USER)
        .map(|_| nami_engine::logs::search::QueryPermit::acquire(&logs, user).unwrap())
        .collect();
    assert_eq!(
        call(&app, "GET", &entry, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    drop(searches);

    // Entry reads have their own cap.
    let entries: Vec<_> = (0..nami_engine::logs::search::MAX_ENTRY_READS_PER_USER)
        .map(|_| {
            logs.entry_slots
                .acquire(user, nami_engine::logs::search::MAX_ENTRY_READS_PER_USER)
                .unwrap()
        })
        .collect();
    assert_eq!(
        call(&app, "GET", &entry, Some(&viewer), None).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(entries);
    assert_eq!(
        call(&app, "GET", &entry, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    ch.drop_database().await.unwrap();
}
