// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::{
    app, call, seed_http_monitor, seed_project, state_with_clickhouse, test_app, user_with_role,
};
use nami_engine::{
    core::clickhouse::ClickHouse,
    core::state::AppState,
    uptime::{
        checker::CheckStatus,
        store::{self, SeriesBucket, UptimeCheckRow},
    },
};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

fn check(
    project: Uuid,
    monitor: Uuid,
    at: DateTime<Utc>,
    up: bool,
    response_ms: Option<u32>,
) -> UptimeCheckRow {
    UptimeCheckRow {
        monitor_id: monitor,
        project_id: project,
        checked_at: at,
        status: if up {
            CheckStatus::Up
        } else {
            CheckStatus::Down
        },
        status_code: up.then_some(200),
        response_time_ms: response_ms,
        error: (!up).then(|| "connection refused".to_string()),
    }
}

/// Start of the half-hour bucket containing the current time.
fn current_bucket_start() -> DateTime<Utc> {
    let secs = Utc::now().timestamp();
    DateTime::from_timestamp(secs - secs.rem_euclid(1800), 0).unwrap()
}

/// History for `monitor`, relative to `now` = bucket start + 10 minutes:
/// - current bucket: up 100 ms, up 200 ms, down
/// - 10 buckets back (5 h ago): up 300 ms
/// - 2 days ago: down; 20 days ago: up 50 ms; 40 days ago: down
/// - 5 minutes after `now`: down (must be ignored)
///
/// Plus one down check for `other` (another monitor, same project) and one
/// for another project under the same monitor id.
async fn seed_history(
    ch: &ClickHouse,
    project: Uuid,
    monitor: Uuid,
    other: Uuid,
    now: DateTime<Utc>,
) {
    let minutes = |m: i64| now - Duration::minutes(m);
    let rows = vec![
        check(project, monitor, minutes(1), true, Some(100)),
        check(project, monitor, minutes(2), true, Some(200)),
        check(project, monitor, minutes(3), false, None),
        check(project, monitor, minutes(300), true, Some(300)),
        check(project, monitor, now - Duration::days(2), false, None),
        check(project, monitor, now - Duration::days(20), true, Some(50)),
        check(project, monitor, now - Duration::days(40), false, None),
        check(project, monitor, now + Duration::minutes(5), false, None),
        check(project, other, minutes(1), false, None),
        check(Uuid::new_v4(), monitor, minutes(1), false, None), // another project's row
    ];
    store::insert_checks(ch, &rows).await.unwrap();
}

/// A ClickHouse client pointed at a TCP server that accepts connections and
/// never answers, plus the number of connections the server has accepted.
/// The accepted sockets are kept open, so the client waits for a reply
/// instead of seeing the connection drop.
async fn hung_clickhouse() -> (ClickHouse, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            held.push(socket);
        }
    });
    (
        ClickHouse::new(&url, "nami", "nami", "nami_hung_in_tests"),
        accepted,
    )
}

#[tokio::test]
async fn stats_compute_each_window_and_the_24h_average() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start() + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let stats = store::monitor_stats(&ch, project, monitor, now)
        .await
        .unwrap();

    assert_eq!(stats.uptime_24h, Some(0.75), "3 up + 1 down");
    assert_eq!(stats.uptime_7d, Some(0.6), "+ the down check 2 days ago");
    assert!(
        (stats.uptime_30d.unwrap() - 4.0 / 6.0).abs() < 1e-9,
        "+ the up check 20 days ago"
    );
    assert_eq!(
        stats.avg_response_ms_24h,
        Some(200.0),
        "mean of 100, 200, 300; nulls ignored"
    );
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn the_series_has_48_half_hour_buckets_with_nulls_for_gaps() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let bucket = current_bucket_start();
    let now = bucket + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let series = store::monitor_stats(&ch, project, monitor, now)
        .await
        .unwrap()
        .series;

    assert_eq!(series.len(), 48);
    assert_eq!(series[0].bucket_start, bucket - Duration::minutes(30 * 47));
    assert_eq!(series[47].bucket_start, bucket);
    assert!(
        series
            .windows(2)
            .all(|w| w[1].bucket_start - w[0].bucket_start == Duration::minutes(30))
    );
    assert_eq!(series[47].avg_response_ms, Some(150.0));
    assert!(
        (series[47].up_ratio.unwrap() - 2.0 / 3.0).abs() < 1e-9,
        "the row after `now` is excluded"
    );
    assert_eq!(
        series[37],
        SeriesBucket {
            bucket_start: bucket - Duration::minutes(300),
            avg_response_ms: Some(300.0),
            up_ratio: Some(1.0)
        }
    );
    assert_eq!(
        series.iter().filter(|b| b.up_ratio.is_some()).count(),
        2,
        "every other bucket is a null gap"
    );
    assert!(
        series
            .iter()
            .filter(|b| b.up_ratio.is_none())
            .all(|b| b.avg_response_ms.is_none())
    );
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn a_monitor_without_checks_has_null_stats_and_an_empty_series() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let stats = store::monitor_stats(&ch, Uuid::new_v4(), Uuid::new_v4(), Utc::now())
        .await
        .unwrap();
    assert_eq!(
        (
            stats.uptime_24h,
            stats.uptime_7d,
            stats.uptime_30d,
            stats.avg_response_ms_24h
        ),
        (None, None, None, None)
    );
    assert_eq!(stats.series.len(), 48);
    assert!(
        stats
            .series
            .iter()
            .all(|b| b.up_ratio.is_none() && b.avg_response_ms.is_none())
    );
    ch.drop_database().await.unwrap();
}

/// Window and bucket edges with `now` on a bucket boundary, so the clock is
/// pinned and nothing depends on where in a half hour the test runs. A check
/// exactly 24 h / 7 d / 30 d before `now` is inside that window and one
/// millisecond earlier is outside it; a check at `now` counts and one at
/// `now + 1 ms` does not. The series starts 23.5 h before `now`: a check on
/// the first millisecond of its first bucket is in it, one millisecond
/// earlier is in the 24 h window but not in the series.
#[tokio::test]
async fn window_and_bucket_edges_are_exact() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor) = (Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start();
    let ms = Duration::milliseconds(1);
    // The first bucket of the series starts 23.5 h before `now`.
    let first_bucket_offset = -Duration::minutes(30 * 47);
    let first_bucket = now + first_bucket_offset;
    let row = |offset: Duration, up: bool, response_ms: Option<u32>| {
        check(project, monitor, now + offset, up, response_ms)
    };
    let rows = [
        // 30 d window
        row(-Duration::days(30) - ms, false, None), // outside every window
        row(-Duration::days(30), true, Some(30)),   // 30 d only
        // 7 d window
        row(-Duration::days(7) - ms, false, None), // 30 d, not 7 d
        row(-Duration::days(7), true, Some(70)),   // 7 d and 30 d
        // 24 h window
        row(-Duration::hours(24) - ms, false, None), // 7 d and 30 d, not 24 h
        row(-Duration::hours(24), true, Some(240)),  // every window, before the series
        // the series
        row(first_bucket_offset - ms, false, None), // 24 h window, before the series
        row(first_bucket_offset, true, Some(500)),  // first bucket
        // now
        row(Duration::zero(), true, Some(100)), // last bucket
        row(ms, false, None),                   // after `now`: counted nowhere
    ];
    store::insert_checks(&ch, &rows).await.unwrap();

    let stats = store::monitor_stats(&ch, project, monitor, now)
        .await
        .unwrap();

    assert_eq!(
        stats.uptime_24h,
        Some(0.75),
        "3 up of the 4 checks in [now-24h, now]"
    );
    assert!(
        (stats.uptime_7d.unwrap() - 4.0 / 6.0).abs() < 1e-9,
        "4 up of 6 in [now-7d, now]: {:?}",
        stats.uptime_7d
    );
    assert_eq!(stats.uptime_30d, Some(0.625), "5 up of 8 in [now-30d, now]");
    assert_eq!(
        stats.avg_response_ms_24h,
        Some(280.0),
        "mean of 240, 500 and 100"
    );
    assert_eq!(stats.series.len(), 48);
    assert_eq!(
        stats.series[0],
        SeriesBucket {
            bucket_start: first_bucket,
            avg_response_ms: Some(500.0),
            up_ratio: Some(1.0),
        },
        "a check on bucket 0's first millisecond is in it, one a millisecond earlier is not"
    );
    assert_eq!(
        stats.series[47],
        SeriesBucket {
            bucket_start: now,
            avg_response_ms: Some(100.0),
            up_ratio: Some(1.0),
        },
        "a check at `now` is in the last bucket, one a millisecond later is not"
    );
    assert_eq!(
        stats.series.iter().filter(|b| b.up_ratio.is_some()).count(),
        2
    );

    let by_monitor = store::uptime_24h_by_monitor(&ch, project, &[monitor], now)
        .await
        .unwrap();
    assert_eq!(
        by_monitor[&monitor], 0.75,
        "the grouped query has the same 24 h window"
    );
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn an_all_down_monitor_has_zero_uptime_and_no_average() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor) = (Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start();
    // Both land in series[45], the bucket that starts an hour before `now`.
    let rows = [
        check(project, monitor, now - Duration::minutes(60), false, None),
        check(project, monitor, now - Duration::minutes(50), false, None),
    ];
    store::insert_checks(&ch, &rows).await.unwrap();

    let stats = store::monitor_stats(&ch, project, monitor, now)
        .await
        .unwrap();

    assert_eq!(
        (stats.uptime_24h, stats.uptime_7d, stats.uptime_30d),
        (Some(0.0), Some(0.0), Some(0.0)),
        "0 up of 2 is 0.0, not null"
    );
    assert_eq!(
        stats.avg_response_ms_24h, None,
        "no check carried a response time"
    );
    assert_eq!(
        stats.series[45],
        SeriesBucket {
            bucket_start: now - Duration::minutes(60),
            avg_response_ms: None,
            up_ratio: Some(0.0),
        }
    );
    assert_eq!(
        stats.series.iter().filter(|b| b.up_ratio.is_some()).count(),
        1
    );
    ch.drop_database().await.unwrap();
}

#[tokio::test]
async fn uptime_by_monitor_is_grouped_per_monitor_of_the_project() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let now = current_bucket_start() + Duration::minutes(10);
    seed_history(&ch, project, monitor, other, now).await;

    let uptime = store::uptime_24h_by_monitor(&ch, project, &[monitor, other], now)
        .await
        .unwrap();

    assert_eq!(uptime.len(), 2, "{uptime:?}");
    assert_eq!(uptime[&monitor], 0.75);
    assert_eq!(uptime[&other], 0.0);
    ch.drop_database().await.unwrap();
}

/// The caller passes the project's own monitor ids, which lets ClickHouse use
/// the `(monitor_id, checked_at)` sort key. Only those come back, and an id
/// that belongs to another project still returns nothing.
#[tokio::test]
async fn uptime_by_monitor_only_returns_the_requested_monitors_of_the_project() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, other_project) = (Uuid::new_v4(), Uuid::new_v4());
    let (asked_up, asked_down, not_asked, foreign) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let now = current_bucket_start() + Duration::minutes(10);
    let at = now - Duration::minutes(1);
    let rows = [
        check(project, asked_up, at, true, Some(10)),
        check(project, asked_down, at, false, None),
        check(project, not_asked, at, true, Some(10)),
        check(other_project, foreign, at, true, Some(10)),
    ];
    store::insert_checks(&ch, &rows).await.unwrap();

    let uptime = store::uptime_24h_by_monitor(&ch, project, &[asked_up, asked_down, foreign], now)
        .await
        .unwrap();

    assert_eq!(uptime, HashMap::from([(asked_up, 1.0), (asked_down, 0.0)]));
    ch.drop_database().await.unwrap();
}

/// A project without monitors has nothing to look up: no query is sent.
#[tokio::test]
async fn uptime_by_monitor_for_no_monitors_sends_no_query() {
    let (hung, accepted) = hung_clickhouse().await;

    let uptime = store::uptime_24h_by_monitor(&hung, Uuid::new_v4(), &[], Utc::now())
        .await
        .unwrap();

    assert!(uptime.is_empty());
    assert_eq!(accepted.load(Ordering::SeqCst), 0);
}

/// The route confirms the monitor belongs to the project first, but the query
/// itself must also scope by both ids: a sibling monitor's rows and another
/// project's rows under the same monitor id are newer here, so a missing
/// filter would put them first.
#[tokio::test]
async fn recent_checks_only_returns_the_given_monitor_of_the_given_project() {
    let ch = ClickHouse::create_test_database().await.unwrap();
    let (project, monitor, sibling, elsewhere) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    // Whole milliseconds: checked_at is DateTime64(3).
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let ours = check(
        project,
        monitor,
        now - Duration::minutes(1),
        true,
        Some(100),
    );
    let rows = [
        ours.clone(),
        check(project, sibling, now - Duration::seconds(30), false, None),
        check(elsewhere, monitor, now - Duration::seconds(20), false, None),
    ];
    store::insert_checks(&ch, &rows).await.unwrap();

    let checks = store::recent_checks(&ch, project, monitor, 10)
        .await
        .unwrap();

    assert_eq!(checks, vec![ours]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_monitor_list_includes_24h_uptime_or_null(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let checked = seed_http_monitor(&pool, project, "a-checked", "https://example.com", 2).await;
    let fresh = seed_http_monitor(&pool, project, "b-fresh", "https://example.com", 2).await;
    let hour_ago = Utc::now() - Duration::hours(1);
    let rows: Vec<UptimeCheckRow> = [true, true, true, false]
        .into_iter()
        .map(|up| check(project, checked, hour_ago, up, Some(120)))
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&viewer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["id"], checked.to_string());
    assert_eq!(list[0]["uptime_24h"], json!(0.75));
    assert_eq!(
        list[0]["name"], "a-checked",
        "monitor fields are flattened alongside uptime_24h"
    );
    assert_eq!(list[1]["id"], fresh.to_string());
    assert!(
        list[1].get("uptime_24h").is_some_and(Value::is_null),
        "a monitor without checks has the key, null: {list}"
    );
    ch.drop_database().await.unwrap();
}

/// With ClickHouse unavailable the list still loads, with the Postgres status
/// and `uptime_24h: null`, instead of a 500.
#[sqlx::test]
async fn the_monitor_list_still_loads_when_clickhouse_is_unavailable(pool: sqlx::PgPool) {
    // AppState::for_test points ClickHouse at a database that does not exist.
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    sqlx::query("update monitors set current_status = 'down' where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&viewer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["current_status"], "down");
    assert!(
        list[0].get("uptime_24h").is_some_and(Value::is_null),
        "the key is present, null: {list}"
    );
}

/// A ClickHouse that accepts the connection and never answers must not hold
/// the monitor list hostage: it is served, without uptime, once the lookup
/// deadline passes, not after the HTTP client's own 30 s timeout.
#[sqlx::test]
async fn the_monitor_list_gives_up_on_a_hung_clickhouse_after_a_short_deadline(pool: sqlx::PgPool) {
    let (hung, accepted) = hung_clickhouse().await;
    let app = app(AppState {
        clickhouse: hung,
        ..AppState::for_test(pool.clone())
    });
    let project = seed_project(&pool).await;
    seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let started = Instant::now();
    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&viewer),
        None,
    )
    .await;
    let elapsed = started.elapsed();

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["name"], "API");
    assert!(
        list[0].get("uptime_24h").is_some_and(Value::is_null),
        "the key is present, null: {list}"
    );
    assert!(
        accepted.load(Ordering::SeqCst) >= 1,
        "the lookup must have reached the server, or this test proves nothing"
    );
    assert!(
        elapsed < StdDuration::from_secs(5),
        "the list waited {elapsed:?} for a hung ClickHouse"
    );
}

/// With no monitors there is nothing to look up: ClickHouse is not asked.
#[sqlx::test]
async fn a_project_without_monitors_never_calls_clickhouse(pool: sqlx::PgPool) {
    let (hung, accepted) = hung_clickhouse().await;
    let app = app(AppState {
        clickhouse: hung,
        ..AppState::for_test(pool.clone())
    });
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&viewer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list, json!([]));
    assert_eq!(accepted.load(Ordering::SeqCst), 0, "no query was sent");
}

#[sqlx::test]
async fn checks_are_newest_first_with_a_default_and_clamped_limit(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    // Whole milliseconds: checked_at is DateTime64(3), so finer precision
    // would not round-trip.
    let start =
        DateTime::from_timestamp_millis((Utc::now() - Duration::hours(2)).timestamp_millis())
            .unwrap();
    let rows: Vec<UptimeCheckRow> = (0..501)
        .map(|i| {
            check(
                project,
                monitor,
                start + Duration::seconds(i),
                i % 2 == 0,
                Some(10),
            )
        })
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}/checks");
    let get = |query: &str| {
        let (app, viewer, uri) = (app.clone(), viewer.clone(), format!("{uri}{query}"));
        async move { call(&app, "GET", &uri, Some(&viewer), None).await }
    };

    let (status, newest_two) = get("?limit=2").await;
    assert_eq!(status, StatusCode::OK, "{newest_two}");
    let newest: Vec<DateTime<Utc>> = newest_two
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["checked_at"].as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(
        newest,
        vec![
            start + Duration::seconds(500),
            start + Duration::seconds(499)
        ]
    );
    assert_eq!(newest_two[0]["status"], "up");
    assert!(
        newest_two[0].get("error").is_some_and(Value::is_null),
        "an up check carries the error key, null: {}",
        newest_two[0]
    );
    assert_eq!(newest_two[1]["status"], "down");
    assert!(
        newest_two[1].get("status_code").is_some_and(Value::is_null),
        "a down check without a response carries the status_code key, null: {}",
        newest_two[1]
    );
    assert_eq!(newest_two[1]["error"], "connection refused");
    assert_eq!(
        get("").await.1.as_array().unwrap().len(),
        50,
        "default limit"
    );
    assert_eq!(
        get("?limit=1000").await.1.as_array().unwrap().len(),
        500,
        "limit is capped at 500"
    );
    assert_eq!(
        get("?limit=0").await.1.as_array().unwrap().len(),
        1,
        "limit is at least 1"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_malformed_limit_is_a_400(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    for limit in ["abc", "-1"] {
        let uri = format!("/projects/{project}/monitors/{monitor}/checks?limit={limit}");
        assert_eq!(
            call(&app, "GET", &uri, Some(&viewer), None).await.0,
            StatusCode::BAD_REQUEST,
            "{uri}"
        );
    }
}

#[sqlx::test]
async fn a_monitor_without_checks_serves_empty_history_and_null_stats(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let base = format!("/projects/{project}/monitors/{monitor}");

    let (status, checks) = call(&app, "GET", &format!("{base}/checks"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{checks}");
    assert_eq!(checks, json!([]));

    let (status, stats) = call(&app, "GET", &format!("{base}/stats"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    for key in [
        "uptime_24h",
        "uptime_7d",
        "uptime_30d",
        "avg_response_ms_24h",
    ] {
        assert!(
            stats.get(key).is_some_and(Value::is_null),
            "{key} is present, null: {stats}"
        );
    }
    let series = stats["series"].as_array().unwrap();
    assert_eq!(series.len(), 48);
    assert!(series.iter().all(|bucket| {
        bucket.get("bucket_start").is_some_and(Value::is_string)
            && bucket.get("avg_response_ms").is_some_and(Value::is_null)
            && bucket.get("up_ratio").is_some_and(Value::is_null)
    }));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_stats_endpoint_returns_uptime_and_48_buckets(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let hour_ago = Utc::now() - Duration::hours(1);
    let rows: Vec<UptimeCheckRow> = [true, true, true, false]
        .into_iter()
        .map(|up| check(project, monitor, hour_ago, up, Some(100)))
        .collect();
    store::insert_checks(&ch, &rows).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, stats) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors/{monitor}/stats"),
        Some(&viewer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["uptime_24h"], json!(0.75));
    assert_eq!(stats["avg_response_ms_24h"], json!(100.0));
    assert_eq!(stats["series"].as_array().unwrap().len(), 48);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn read_endpoints_enforce_rbac_and_project_isolation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let own = seed_http_monitor(&pool, project, "own", "https://example.com", 2).await;
    let foreign =
        seed_http_monitor(&pool, other_project, "foreign", "https://example.com", 2).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    for uri in [
        format!("/projects/{project}/monitors"),
        format!("/projects/{project}/monitors/{own}/checks"),
        format!("/projects/{project}/monitors/{own}/stats"),
        format!("/projects/{project}/incidents"),
    ] {
        assert_eq!(
            call(&app, "GET", &uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }
    for uri in [
        format!("/projects/{project}/monitors/{foreign}/checks"),
        format!("/projects/{project}/monitors/{foreign}/stats"),
    ] {
        assert_eq!(
            call(&app, "GET", &uri, Some(&viewer), None).await.0,
            StatusCode::NOT_FOUND,
            "{uri}"
        );
    }
}

async fn seed_incident(
    pool: &sqlx::PgPool,
    project: Uuid,
    monitor: Uuid,
    status: &str,
    started_minutes_ago: i32,
) -> Uuid {
    sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title, cause, status, started_at, resolved_at)
         values ($1, $2, 'down', 'connection refused', $3,
                 now() - make_interval(mins => $4),
                 case when $3 = 'resolved' then now() else null end)
         returning id",
    )
    .bind(project)
    .bind(monitor)
    .bind(status)
    .bind(started_minutes_ago)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn incidents_are_newest_first_and_filterable(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let api = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let web = seed_http_monitor(&pool, project, "Web", "https://example.com", 2).await;
    let elsewhere =
        seed_http_monitor(&pool, other_project, "Elsewhere", "https://example.com", 2).await;
    let old_resolved = seed_incident(&pool, project, api, "resolved", 120).await;
    let api_open = seed_incident(&pool, project, api, "open", 60).await;
    let web_open = seed_incident(&pool, project, web, "open", 30).await;
    seed_incident(&pool, other_project, elsewhere, "open", 10).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let ids = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let base = format!("/projects/{project}/incidents");

    let (status, all) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert_eq!(
        ids(&all),
        vec![
            web_open.to_string(),
            api_open.to_string(),
            old_resolved.to_string()
        ]
    );
    assert_eq!(all[0]["source_name"], "Web");
    assert_eq!(all[0]["monitor_id"], web.to_string());
    assert!(
        all[0].get("heartbeat_id").is_some_and(Value::is_null),
        "monitor incidents carry the heartbeat_id key, null: {}",
        all[0]
    );
    assert!(
        all[0].get("resolved_at").is_some_and(Value::is_null),
        "an open incident carries the resolved_at key, null: {}",
        all[0]
    );

    let (_, open) = call(
        &app,
        "GET",
        &format!("{base}?status=open"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(ids(&open), vec![web_open.to_string(), api_open.to_string()]);
    let (_, resolved) = call(
        &app,
        "GET",
        &format!("{base}?status=resolved"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(ids(&resolved), vec![old_resolved.to_string()]);
    assert!(
        resolved[0].get("resolved_at").is_some_and(Value::is_string),
        "a resolved incident carries its resolved_at: {}",
        resolved[0]
    );
    let (_, for_api) = call(
        &app,
        "GET",
        &format!("{base}?monitor_id={api}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        ids(&for_api),
        vec![api_open.to_string(), old_resolved.to_string()]
    );
    let (status, _) = call(
        &app,
        "GET",
        &format!("{base}?status=bogus"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
