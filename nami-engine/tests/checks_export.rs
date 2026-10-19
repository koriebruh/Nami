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
        checker::MAX_ERROR_CHARS,
        export::{HEADER, MAX_EXPORTS_IN_FLIGHT, MAX_EXPORTS_PER_PROJECT, MAX_EXPORTS_PER_USER},
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

/// An error the checker cut: `MAX_ERROR_CHARS` chars plus its `…` marker.
fn long_error() -> &'static str {
    static LONG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LONG.get_or_init(|| format!("{}…", "e".repeat(MAX_ERROR_CHARS)))
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
                4321 => Some(long_error()),
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
        formula_lines[0].ends_with(",\"'=cmd|' /C calc'!A0\""),
        "{}",
        formula_lines[0]
    );

    // The truncation marker of a cut error survives the export query.
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.ends_with(&format!(",\"{}\"", long_error())))
            .count(),
        1
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
            assert!(
                record[4].starts_with('\'')
                    || record[4].starts_with("expected")
                    || record[4] == *long_error()
            );
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
    let from = range_start();
    let to = from + Duration::days(3);
    // Two exports per project (and per user): filling the instance takes
    // MAX_EXPORTS_IN_FLIGHT / 2 projects, plus one more to be refused.
    let mut tenants = Vec::new();
    for _ in 0..=MAX_EXPORTS_IN_FLIGHT / MAX_EXPORTS_PER_PROJECT {
        tenants.push(export_tenant(&app, &pool, &ch, from).await);
    }
    let uri_of = |(project, monitor, _): &(Uuid, Uuid, HeaderValue)| {
        export_url(*project, *monitor, from, to)
    };

    let mut held = Vec::new();
    for i in 0..MAX_EXPORTS_IN_FLIGHT {
        let tenant = &tenants[i / MAX_EXPORTS_PER_PROJECT];
        let response = get(&app, &uri_of(tenant), &tenant.2).await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    let last = tenants.last().unwrap();
    let (uri, viewer) = (uri_of(last), last.2.clone());
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

/// A project of its own, a viewer of it and a monitor with three days of
/// checks from `from`.
async fn export_tenant(
    app: &Router,
    pool: &PgPool,
    ch: &ClickHouse,
    from: DateTime<Utc>,
) -> (Uuid, Uuid, HeaderValue) {
    let project = seed_project(pool).await;
    let (_, _, viewer) = user_with_role(app, pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(pool, project, "api", "https://example.com", 1).await;
    seed_three_days(ch, project, monitor, from).await;
    (project, monitor, viewer)
}

/// One project runs at most two exports at once, whoever starts them: a
/// third, from another member, is a 429 naming the project, while another
/// project exports in full.
#[sqlx::test]
async fn one_project_runs_at_most_two_exports(pool: PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let from = range_start();
    let to = from + Duration::days(3);
    let (project, monitor, first) = export_tenant(&app, &pool, &ch, from).await;
    let (_, _, second) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, third) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = export_url(project, monitor, from, to);

    let mut held = Vec::new();
    for cookie in [&first, &second].into_iter().take(MAX_EXPORTS_PER_PROJECT) {
        let response = get(&app, &uri, cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    let refused = get(&app, &uri, &third).await;
    assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header_of(&refused, header::RETRY_AFTER), "5");
    let body = axum::body::to_bytes(refused.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "error": "this project already has 2 exports running; retry when one finishes" })
    );

    let (other_project, other_monitor, outsider) = export_tenant(&app, &pool, &ch, from).await;
    let response = get(
        &app,
        &export_url(other_project, other_monitor, from, to),
        &outsider,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.starts_with(HEADER.as_bytes()));
    drop(held);
    ch.drop_database().await.unwrap();
}

/// One user runs at most two exports at once, whatever slots are free; the
/// third is a 429 with `Retry-After`, a user of another project is
/// unaffected, and a finished (dropped) export gives the user's slot back.
#[sqlx::test]
async fn one_user_runs_at_most_two_exports(pool: PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let from = range_start();
    let to = from + Duration::days(3);
    seed_three_days(&ch, project, monitor, from).await;
    let uri = export_url(project, monitor, from, to);
    let (other_project, other_monitor, other) = export_tenant(&app, &pool, &ch, from).await;

    let mut held = Vec::new();
    for _ in 0..MAX_EXPORTS_PER_USER {
        let response = get(&app, &uri, &viewer).await;
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    tokio::time::sleep(StdDuration::from_millis(300)).await;
    let third = get(&app, &uri, &viewer).await;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(header_of(&third, header::RETRY_AFTER), "5");
    assert_eq!(header_of(&third, header::CACHE_CONTROL), "no-store");
    let body = axum::body::to_bytes(third.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        serde_json::json!({ "error": "you already have 2 exports running; retry when one finishes" })
    );

    // Global slots are still free: another user exports in full.
    let response = get(
        &app,
        &export_url(other_project, other_monitor, from, to),
        &other,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.starts_with(HEADER.as_bytes()));

    // Rejected requests (403/404/400 before the slot, 429 itself) never
    // leak a slot; dropping one held export frees one for this user.
    drop(held.pop());
    let deadline = Instant::now() + StdDuration::from_secs(10);
    loop {
        let response = get(&app, &uri, &viewer).await;
        if response.status() == StatusCode::OK {
            break;
        }
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(Instant::now() < deadline, "the user's slot was never freed");
        tokio::time::sleep(StdDuration::from_millis(50)).await;
    }
    drop(held);
    ch.drop_database().await.unwrap();
}
