// Availability (SLA) report routes. Requires `docker compose up -d postgres`;
// ClickHouse is not involved.
mod common;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn url(kind: &str, project: Uuid, id: Uuid, from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    format!(
        "/projects/{project}/{kind}/{id}/availability?from={}&to={}",
        ts(from),
        ts(to)
    )
}

fn secs(v: &Value, key: &str) -> i64 {
    v[key].as_i64().unwrap_or_else(|| panic!("{key} in {v}"))
}

async fn age(pool: &PgPool, table: &'static str, id: Uuid, days: i64) {
    let sql = match table {
        "monitors" => "update monitors set created_at = $2 where id = $1",
        _ => "update heartbeats set created_at = $2 where id = $1",
    };
    sqlx::query(sql)
        .bind(id)
        .bind(Utc::now() - Duration::days(days))
        .execute(pool)
        .await
        .unwrap();
}

async fn incident(
    pool: &PgPool,
    project: Uuid,
    monitor: Option<Uuid>,
    heartbeat: Option<Uuid>,
    started: DateTime<Utc>,
    resolved: Option<DateTime<Utc>>,
) {
    sqlx::query(
        "insert into incidents (project_id, monitor_id, heartbeat_id, title, status, started_at, resolved_at)
         values ($1, $2, $3, 'down', $4, $5, $6)",
    )
    .bind(project)
    .bind(monitor)
    .bind(heartbeat)
    .bind(if resolved.is_some() { "resolved" } else { "open" })
    .bind(started)
    .bind(resolved)
    .execute(pool)
    .await
    .unwrap();
}

async fn one_off_window(
    pool: &PgPool,
    project: Uuid,
    monitor: Option<Uuid>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) {
    sqlx::query(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, $2, 'w', 'once', $3, $4)",
    )
    .bind(project)
    .bind(monitor)
    .bind(from)
    .bind(to)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn monitor_availability_counts_incidents_minus_maintenance(pool: PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let other = seed_http_monitor(&pool, project, "other", "https://example.org", 1).await;
    age(&pool, "monitors", monitor, 10).await;

    let now = Utc::now();
    let resolved_start = now - Duration::days(3);
    incident(
        &pool,
        project,
        Some(monitor),
        None,
        resolved_start,
        Some(resolved_start + Duration::hours(2)),
    )
    .await;
    incident(
        &pool,
        project,
        Some(monitor),
        None,
        now - Duration::hours(1),
        None,
    )
    .await;
    // Project-wide, 30 minutes at the end of the resolved incident.
    one_off_window(
        &pool,
        project,
        None,
        resolved_start + Duration::minutes(90),
        resolved_start + Duration::hours(2),
    )
    .await;
    // Scoped to another monitor, over the open incident: must not count.
    one_off_window(
        &pool,
        project,
        Some(other),
        now - Duration::hours(2),
        now + Duration::hours(2),
    )
    .await;

    let from = now - Duration::days(7);
    let (status, body) = call(
        &app,
        "GET",
        &url("monitors", project, monitor, from, now + Duration::days(1)),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let to: DateTime<Utc> = body["to"].as_str().unwrap().parse().unwrap();
    assert!(
        (to - now).num_seconds().abs() <= 5,
        "to is clipped to now: {body}"
    );
    let got_from: DateTime<Utc> = body["from"].as_str().unwrap().parse().unwrap();
    assert_eq!(ts(got_from), ts(from));
    assert_eq!(secs(&body, "maintenance_seconds"), 1800, "{body}");
    assert!(
        (secs(&body, "downtime_seconds") - 9000).abs() <= 5,
        "{body}"
    );
    assert_eq!(secs(&body, "incidents"), 2);
    assert!(
        (secs(&body, "longest_incident_seconds") - 5400).abs() <= 5,
        "{body}"
    );
    let total = (7 * 86400 - 1800) as f64;
    let expected = 100.0 * (1.0 - 9000.0 / total);
    assert!(
        (body["uptime_pct"].as_f64().unwrap() - expected).abs() < 0.001,
        "{body}"
    );

    // The other monitor has no incidents: 100 %, scoped window counted for it.
    age(&pool, "monitors", other, 10).await;
    let (_, body) = call(
        &app,
        "GET",
        &url("monitors", project, other, from, now),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(secs(&body, "downtime_seconds"), 0);
    assert!(
        (secs(&body, "maintenance_seconds") - 9000).abs() <= 2,
        "{body}"
    );
}

#[sqlx::test]
async fn a_range_before_creation_has_no_uptime(pool: PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    age(&pool, "monitors", monitor, 10).await;
    let now = Utc::now();
    incident(
        &pool,
        project,
        Some(monitor),
        None,
        now - Duration::days(25),
        Some(now - Duration::days(24)),
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &url(
            "monitors",
            project,
            monitor,
            now - Duration::days(30),
            now - Duration::days(20),
        ),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(secs(&body, "total_seconds"), 0);
    assert!(body["uptime_pct"].is_null(), "{body}");
    assert_eq!(secs(&body, "incidents"), 0);
    assert_eq!(
        body["from"], body["to"],
        "both bounds are the clipped start: {body}"
    );

    // A range entirely in the future is empty too (clipped to now).
    let (_, body) = call(
        &app,
        "GET",
        &url(
            "monitors",
            project,
            monitor,
            now + Duration::days(1),
            now + Duration::days(2),
        ),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(secs(&body, "total_seconds"), 0);
    assert!(body["uptime_pct"].is_null(), "{body}");
}

#[sqlx::test]
async fn heartbeat_availability_uses_project_wide_windows_only(pool: PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(&member),
        Some(json!({ "name": "Nightly", "period_seconds": 86400 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let heartbeat: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    age(&pool, "heartbeats", heartbeat, 10).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;

    let now = Utc::now();
    let start = now - Duration::days(3);
    incident(
        &pool,
        project,
        None,
        Some(heartbeat),
        start,
        Some(start + Duration::hours(2)),
    )
    .await;
    let from = now - Duration::days(7);

    // Monitor-scoped window over it: ignored.
    one_off_window(
        &pool,
        project,
        Some(monitor),
        start,
        start + Duration::hours(2),
    )
    .await;
    let (status, body) = call(
        &app,
        "GET",
        &url("heartbeats", project, heartbeat, from, now),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(secs(&body, "maintenance_seconds"), 0);
    assert_eq!(secs(&body, "downtime_seconds"), 7200);
    assert_eq!(secs(&body, "incidents"), 1);

    // A project-wide one over the first hour: counts.
    one_off_window(&pool, project, None, start, start + Duration::hours(1)).await;
    let (_, body) = call(
        &app,
        "GET",
        &url("heartbeats", project, heartbeat, from, now),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(secs(&body, "maintenance_seconds"), 3600);
    assert_eq!(secs(&body, "downtime_seconds"), 3600);
    assert_eq!(secs(&body, "longest_incident_seconds"), 3600);
}

#[sqlx::test]
async fn availability_checks_ranges_access_and_project(pool: PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let foreign = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "https://example.com", 1).await;
    let foreign_monitor = seed_http_monitor(&pool, foreign, "x", "https://example.net", 1).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(&member),
        Some(json!({ "name": "Nightly", "period_seconds": 86400 })),
    )
    .await;
    let heartbeat: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let foreign_heartbeat: Uuid = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, period_seconds, token) values ($1, 'h', 3600, $2) returning id",
    )
    .bind(foreign)
    .bind("a".repeat(64))
    .fetch_one(&pool)
    .await
    .unwrap();

    let now = Utc::now();
    for (kind, id, foreign_id) in [
        ("monitors", monitor, foreign_monitor),
        ("heartbeats", heartbeat, foreign_heartbeat),
    ] {
        let ok_from = now - Duration::days(1);
        let (status, body) = call(
            &app,
            "GET",
            &url(kind, project, id, now, now),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "from must be before to");
        let (status, _) = call(
            &app,
            "GET",
            &url(kind, project, id, now, now - Duration::days(1)),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, body) = call(
            &app,
            "GET",
            &url(kind, project, id, now - Duration::days(1097), now),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "the range must be at most 1096 days");
        let (status, _) = call(
            &app,
            "GET",
            &url(kind, project, id, now - Duration::days(1096), now),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "1096 days is allowed");
        let garbage = format!(
            "/projects/{project}/{kind}/{id}/availability?from=garbage&to={}",
            ts(now)
        );
        let (status, _) = call(&app, "GET", &garbage, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = call(
            &app,
            "GET",
            &format!("/projects/{project}/{kind}/{id}/availability"),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "missing range");

        let (status, _) = call(
            &app,
            "GET",
            &url(kind, project, id, ok_from, now),
            Some(&outsider),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = call(
            &app,
            "GET",
            &url(kind, project, id, ok_from, now),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        // Another project's id inside an accessible project path.
        let (status, _) = call(
            &app,
            "GET",
            &url(kind, project, foreign_id, ok_from, now),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        // The viewer has no access to the foreign project itself.
        let (status, _) = call(
            &app,
            "GET",
            &url(kind, foreign, foreign_id, ok_from, now),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
