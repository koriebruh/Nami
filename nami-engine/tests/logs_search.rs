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
    ch: nami_engine::clickhouse::ClickHouse,
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
