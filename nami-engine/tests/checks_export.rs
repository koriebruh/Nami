// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::time::{Duration as StdDuration, Instant};

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, Response, StatusCode, header},
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use common::{
    app, call, seed_http_monitor, seed_project, state_with_clickhouse, test_app, user_with_role,
};
use nami_engine::{
    clickhouse::ClickHouse,
    uptime::{
        checker::CheckStatus,
        export::{HEADER, MAX_EXPORTS_IN_FLIGHT},
        store::{self, UptimeCheckRow},
    },
};
use serde_json::Value;
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const FORMULA: &str = "=cmd|' /C calc'!A0";

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn export_url(project: Uuid, monitor: Uuid, from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    format!(
        "/projects/{project}/monitors/{monitor}/checks/export?from={}&to={}",
        ts(from),
        ts(to)
    )
}

fn check(project: Uuid, monitor: Uuid, at: DateTime<Utc>, error: Option<&str>) -> UptimeCheckRow {
    UptimeCheckRow {
        monitor_id: monitor,
        project_id: project,
        checked_at: at,
        status: if error.is_some() {
            CheckStatus::Down
        } else {
            CheckStatus::Up
        },
        status_code: Some(if error.is_some() { 503 } else { 200 }),
        response_time_ms: Some(42),
        error: error.map(String::from),
    }
}

/// A day boundary ten days ago (well inside the 180-day TTL).
fn range_start() -> DateTime<Utc> {
    let ten_days_ago = Utc::now() - Duration::days(10);
    let secs = ten_days_ago.timestamp();
    DateTime::from_timestamp(secs - secs.rem_euclid(86_400), 0).unwrap()
}

/// 3 days x 2000 checks (one every 43.2 s) for `monitor` from `from`, row
/// 1234 carrying `FORMULA` as its error, every 7th another down error with a
/// comma and quotes.
async fn seed_three_days(ch: &ClickHouse, project: Uuid, monitor: Uuid, from: DateTime<Utc>) {
    let rows: Vec<UptimeCheckRow> = (0..6000)
        .map(|i| {
            let at = from + Duration::milliseconds(i * 43_200);
            let error = match i {
                1234 => Some(FORMULA),
                _ if i % 7 == 0 => Some("expected status 200-399, got \"503\""),
                _ => None,
            };
            check(project, monitor, at, error)
        })
        .collect();
    store::insert_checks(ch, &rows).await.unwrap();
}

async fn get(app: &Router, uri: &str, cookie: &HeaderValue) -> Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

fn header_of(response: &Response<Body>, name: header::HeaderName) -> String {
    response
        .headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

#[sqlx::test]
async fn the_export_streams_every_check_in_the_range_as_csv(pool: PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let other = seed_http_monitor(&pool, project, "other", "https://example.org", 1).await;
    let from = range_start();
    let to = from + Duration::days(3);
    seed_three_days(&ch, project, monitor, from).await;
    store::insert_checks(
        &ch,
        &[
            // Outside the range: just before `from`, and at `to` (exclusive).
            check(
                project,
                monitor,
                from - Duration::milliseconds(1),
                Some("before"),
            ),
            check(project, monitor, to, Some("at the end")),
            // Another monitor, inside the range.
            check(
                project,
                other,
                from + Duration::hours(1),
                Some("other monitor"),
            ),
        ],
    )
    .await
    .unwrap();

    let response = get(&app, &export_url(project, monitor, from, to), &viewer).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_of(&response, header::CONTENT_TYPE),
        "text/csv; charset=utf-8"
    );
    assert_eq!(
        header_of(&response, header::CONTENT_DISPOSITION),
        format!("attachment; filename=\"checks-{monitor}.csv\"")
    );
    assert_eq!(header_of(&response, header::CACHE_CONTROL), "no-store");
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = std::str::from_utf8(&body).unwrap();

    assert!(body.starts_with(HEADER));
    let lines: Vec<&str> = body.split_terminator("\r\n").collect();
    assert_eq!(lines.len(), 1 + 6000);
    assert!(!body.contains("before") && !body.contains("at the end"));
    assert!(!body.contains("other monitor"));
    let formula_lines: Vec<&&str> = lines.iter().filter(|l| l.contains("cmd|")).collect();
    assert_eq!(formula_lines.len(), 1);
    assert!(
        formula_lines[0].ends_with(",'=cmd|' /C calc'!A0"),
        "{}",
        formula_lines[0]
    );

    let mut reader = csv::Reader::from_reader(body.as_bytes());
    assert_eq!(
        reader.headers().unwrap(),
        vec![
            "checked_at",
            "status",
            "status_code",
            "response_time_ms",
            "error"
        ]
    );
    let mut previous: Option<DateTime<Utc>> = None;
    let mut count = 0;
    for record in reader.records() {
        let record = record.unwrap();
        assert_eq!(record.len(), 5);
        let at: DateTime<Utc> = record[0].parse().unwrap();
        assert!(at >= from && at < to);
        assert!(previous.is_none_or(|p| p < at), "ascending");
        previous = Some(at);
        if &record[1] == "down" {
            assert!(record[4].starts_with('\'') || record[4].starts_with("expected"));
        } else {
            assert_eq!(&record[4], "");
        }
        count += 1;
    }
    assert_eq!(count, 6000);
    assert_eq!(
        reader.position().line() as usize,
        6002,
        "no embedded newline split a record"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn export_checks_access_project_and_range(pool: PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let elsewhere = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, elsewhere, Some("admin")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let foreign = seed_http_monitor(&pool, elsewhere, "theirs", "https://example.org", 1).await;
    let from = range_start();
    let to = from + Duration::days(1);
    store::insert_checks(&ch, &[check(elsewhere, foreign, from, Some("theirs"))])
        .await
        .unwrap();

    // Viewer: allowed (an empty range is just the header).
    let response = get(&app, &export_url(project, monitor, from, to), &viewer).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(body, HEADER.as_bytes());

    // Not a member of the project: 403, whether or not the monitor exists.
    for id in [monitor, Uuid::new_v4()] {
        let (status, _) = call(
            &app,
            "GET",
            &export_url(project, id, from, to),
            Some(&outsider),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // A monitor of another project, through this project: 404.
    let (status, _) = call(
        &app,
        "GET",
        &export_url(project, foreign, from, to),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Ranges.
    let (status, body) = call(
        &app,
        "GET",
        &export_url(project, monitor, from, from + Duration::days(181)),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "the range must be at most 180 days");
    let (status, _) = call(
        &app,
        "GET",
        &export_url(project, monitor, from, from + Duration::days(180)),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    for (a, b) in [(from, from), (to, from)] {
        let (status, body) = call(
            &app,
            "GET",
            &export_url(project, monitor, a, b),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "from must be before to");
    }
    let (status, _) = call(
        &app,
        "GET",
        &format!(
            "/projects/{project}/monitors/{monitor}/checks/export?from={}",
            ts(from)
        ),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // No session: 401.
    let (status, _) = call(
        &app,
        "GET",
        &export_url(project, monitor, from, to),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_clickhouse_outage_is_a_json_500_before_any_csv(pool: PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let from = range_start();
    let response = get(
        &app,
        &export_url(project, monitor, from, from + Duration::days(2)),
        &viewer,
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(header_of(&response, header::CONTENT_TYPE).starts_with("application/json"));
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body, serde_json::json!({ "error": "internal error" }));
}

/// Exports whose bodies nobody reads hold their slots (the producer waits on
/// the full channel, so the file is not buffered whole); a fifth export is a
/// 503 with `Retry-After`. Dropping one body (the client went away) frees its
/// slot.
#[sqlx::test]
async fn slots_are_bounded_and_freed_when_the_client_goes_away(pool: PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let from = range_start();
    let to = from + Duration::days(3);
    seed_three_days(&ch, project, monitor, from).await;
    let uri = export_url(project, monitor, from, to);

    let mut held = Vec::new();
    for _ in 0..MAX_EXPORTS_IN_FLIGHT {
        let response = get(&app, &uri, &viewer).await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    // Let every producer run until its channel is full.
    tokio::time::sleep(StdDuration::from_millis(500)).await;
    let busy = get(&app, &uri, &viewer).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(header_of(&busy, header::RETRY_AFTER), "5");
    assert_eq!(header_of(&busy, header::CACHE_CONTROL), "no-store");
    let body = axum::body::to_bytes(busy.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body, serde_json::json!({ "error": "busy, retry shortly" }));

    drop(held.pop());
    let deadline = Instant::now() + StdDuration::from_secs(10);
    let freed = loop {
        let response = get(&app, &uri, &viewer).await;
        if response.status() == StatusCode::OK {
            break response;
        }
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(Instant::now() < deadline, "the slot was never freed");
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    };
    // The new export completes in full.
    let body = axum::body::to_bytes(freed.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(
        body.split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count(),
        6001
    );
    drop(held);
    ch.drop_database().await.unwrap();
}
