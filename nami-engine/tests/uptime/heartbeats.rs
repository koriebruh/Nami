// Requires `docker compose up -d postgres mailpit`.
use crate::common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use chrono::{DateTime, Utc};
use common::{
    LogCapture, add_member, call, seed_http_monitor, seed_project, seed_user, settled_mail,
    silent_server, test_app, unique_email, user_with_role,
};
use nami_engine::{
    core::email::Mailer,
    core::state::AppState,
    incidents::model as incident,
    logs::ingest::rate_limit::TokenBuckets,
    uptime::heartbeat::routes::{
        MAX_PING_BODIES_IN_FLIGHT, MAX_PING_BODIES_PER_PROJECT, MAX_PING_BODY_BYTES,
        MAX_PINGS_IN_FLIGHT, PING_BODY_TIMEOUT, PING_BURST, PING_SLOT_WAIT,
    },
    uptime::{
        alerts::{Subject, Transition, TransitionKind},
        heartbeat::{
            MAX_OUTPUT_BYTES, OUTAGE_GAP, OUTPUT_BUSY_MARKER, OUTPUT_LATE_MARKER, PingOutcome,
            TRUNCATED_MARKER, note_worker_alive, record_ping, sweep_overdue,
        },
        worker::{Notifications, SWEEP_BATCH, run_forever, sweep_heartbeats, sweep_pass},
    },
};
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tower::ServiceExt;
use uuid::Uuid;

/// A project with one member whose address is unique to this test.
async fn project_with_recipient(pool: &sqlx::PgPool) -> (Uuid, String) {
    let project = seed_project(pool).await;
    let email = unique_email("oncall");
    let user = seed_user(pool, &email, "member").await;
    add_member(pool, project, user, "viewer").await;
    (project, email)
}

/// Inserts a heartbeat created `created_seconds_ago` seconds ago that has
/// never pinged. Returns (id, token).
async fn seed_heartbeat(
    pool: &sqlx::PgPool,
    project: Uuid,
    name: &str,
    period: i32,
    grace: i32,
    created_seconds_ago: i32,
) -> (Uuid, String) {
    // A real token's shape: 64 lowercase hex characters.
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let id = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         values ($1, $2, $3, $4, $5, now() - make_interval(secs => $6))
         returning id",
    )
    .bind(project)
    .bind(name)
    .bind(&token)
    .bind(period)
    .bind(grace)
    .bind(f64::from(created_seconds_ago))
    .fetch_one(pool)
    .await
    .unwrap();
    (id, token)
}

async fn heartbeat_state(pool: &sqlx::PgPool, id: Uuid) -> (String, bool) {
    sqlx::query_as("select current_status, last_ping_at is not null from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn last_ping_at(pool: &sqlx::PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select last_ping_at from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The `details` of the heartbeat's incidents, oldest first.
async fn details_of(pool: &sqlx::PgPool, heartbeat: Uuid) -> Vec<Option<String>> {
    sqlx::query_scalar("select details from incidents where heartbeat_id = $1 order by started_at")
        .bind(heartbeat)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn incidents_of(
    pool: &sqlx::PgPool,
    heartbeat: Uuid,
) -> Vec<(String, String, Option<String>)> {
    sqlx::query_as(
        "select status, title, cause from incidents where heartbeat_id = $1 order by started_at",
    )
    .bind(heartbeat)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn member_creates_reads_and_updates_admin_deletes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/heartbeats");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Nightly backup", "period_seconds": 86400 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let token = created["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(created["grace_seconds"], 60, "default grace");
    assert_eq!(created["current_status"], "pending");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    assert_eq!(
        detail["token"], token,
        "the detail view keeps showing the token"
    );
    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Backup", "grace_seconds": 300, "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        (patched["name"].clone(), patched["grace_seconds"].clone()),
        (json!("Backup"), json!(300))
    );
    assert_eq!(patched["period_seconds"], 86400);
    assert_eq!(patched["token"], token, "editing never rotates the token");

    assert_eq!(
        call(&app, "DELETE", &item, Some(&member), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&admin), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn heartbeat_rbac_and_project_isolation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (foreign, _) = seed_heartbeat(&pool, other_project, "foreign", 60, 0, 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/heartbeats");
    let body = json!({ "name": "x", "period_seconds": 60 });

    assert_eq!(
        call(&app, "POST", &collection, Some(&viewer), Some(body.clone()))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", &collection, Some(&outsider), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let foreign_uri = format!("{collection}/{foreign}");
    assert_eq!(
        call(&app, "GET", &foreign_uri, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &foreign_uri,
            Some(&admin),
            Some(json!({ "name": "hijacked" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &foreign_uri, Some(&admin), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let name: String = sqlx::query_scalar("select name from heartbeats where id = $1")
        .bind(foreign)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "foreign");
}

#[sqlx::test]
async fn invalid_heartbeats_are_rejected_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (
            json!({ "name": "x", "period_seconds": 29 }),
            "period_seconds",
        ),
        (
            json!({ "name": "x", "period_seconds": 60, "grace_seconds": -1 }),
            "grace_seconds",
        ),
        (json!({ "name": " ", "period_seconds": 60 }), "name"),
    ] {
        let (status, json) = call(
            &app,
            "POST",
            &format!("/projects/{project}/heartbeats"),
            Some(&member),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        assert!(
            json["error"].as_str().unwrap().contains(fragment),
            "{body}: {json}"
        );
    }
    let count: i64 = sqlx::query_scalar("select count(*) from heartbeats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn a_ping_needs_no_session_and_marks_the_heartbeat_up(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, body) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "ok": true }));
    assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));

    let (status, _) = call(
        &public,
        "GET",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET pings work too");
    assert!(incidents_of(&pool, id).await.is_empty());
    assert!(
        settled_mail(&recipient, 0).await.is_empty(),
        "pending -> up sends no email"
    );
}

#[sqlx::test]
async fn an_unknown_token_is_404(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let (status, body) = call(
        &public,
        "POST",
        "/api/v1/heartbeat/not-a-real-token",
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({ "error": "not found" }));
}

#[sqlx::test]
async fn the_sweep_marks_an_overdue_heartbeat_down_and_alerts_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    // Never pinged, created 120 s ago, period 30 s + grace 0 s: overdue.
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);

    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    assert_eq!(
        incidents_of(&pool, id).await,
        vec![(
            "open".to_string(),
            "Nightly backup missed its heartbeat".to_string(),
            Some("no ping received within 30s (+0s grace)".to_string())
        )]
    );
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert_eq!(mail[0].0, "[Nami] [high] Nightly backup missed its heartbeat");
    // The email links to the incident, which links to the heartbeat.
    let incident: Uuid = sqlx::query_scalar("select id from incidents where heartbeat_id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let link = format!(
        "{}/projects/{project}/incidents/{incident}",
        state.config.ui_base_url
    );
    assert!(mail[0].1.contains(&link), "{}", mail[0].1);

    assert_eq!(
        sweep_heartbeats(&state).await.unwrap(),
        0,
        "already down: no second incident"
    );
    assert_eq!(incidents_of(&pool, id).await.len(), 1);
    assert_eq!(settled_mail(&recipient, 1).await.len(), 1);
}

#[sqlx::test]
async fn a_ping_on_a_down_heartbeat_resolves_it_and_sends_a_recovery(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let public = common::public_app(state.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();

    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert_eq!(incidents_of(&pool, id).await[0].0, "resolved");
    // The recovery email is sent in the background: settled_mail waits for it.
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec![
            "[Nami] [high] Nightly backup missed its heartbeat",
            "[Nami] Resolved: Nightly backup missed its heartbeat"
        ]
    );
}

#[sqlx::test]
async fn fresh_recently_pinged_and_paused_heartbeats_are_not_swept(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (fresh, _) = seed_heartbeat(&pool, project, "fresh", 60, 0, 0).await;
    let (pinged, _) = seed_heartbeat(&pool, project, "pinged", 60, 30, 86_400).await;
    sqlx::query("update heartbeats set last_ping_at = now() - interval '80 seconds' where id = $1")
        .bind(pinged)
        .execute(&pool)
        .await
        .unwrap();
    let (paused, _) = seed_heartbeat(&pool, project, "paused", 30, 0, 120).await;
    sqlx::query("update heartbeats set paused = true where id = $1")
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    for id in [fresh, pinged, paused] {
        assert_eq!(heartbeat_state(&pool, id).await.0, "pending");
    }
}

#[sqlx::test]
async fn a_paused_heartbeat_accepts_pings_without_changing_state(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&AppState::for_test(pool.clone()))
        .await
        .unwrap();
    sqlx::query("update heartbeats set paused = true where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();

    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("down".to_string(), true),
        "ping recorded, status frozen"
    );
    assert_eq!(incidents_of(&pool, id).await[0].0, "open");
    assert_eq!(
        settled_mail(&recipient, 1).await.len(),
        1,
        "only the original DOWN email"
    );
}

#[sqlx::test]
async fn heartbeat_incidents_appear_in_the_incident_list(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/incidents"),
        Some(&viewer),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["incidents"][0]["heartbeat_id"], id.to_string());
    assert_eq!(list["incidents"][0]["monitor_id"], Value::Null);
    assert_eq!(list["incidents"][0]["source_name"], "Nightly backup");
}

// ---------------------------------------------------------------------------
// The ping endpoint answers anyone who can reach it: what it replies, and what
// it leaves in the log, depends on nothing but the token.
// ---------------------------------------------------------------------------

/// Text that cannot be a token gets the same 404 as any unknown token, without
/// a query. (A NUL byte used to make Postgres fail the lookup: a 500 and a
/// "database error" log line for anyone who sent one; invalid UTF-8 got a
/// plain-text 400 from the path extractor.)
#[sqlx::test]
async fn text_that_cannot_be_a_token_is_an_ordinary_404_without_a_query(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let public = common::test_public_app(pool.clone());
    let lookups = || {
        log.lines()
            .iter()
            .filter(|line| line.contains("from heartbeats where token"))
            .count()
    };

    // Control: a well-formed token nobody holds is looked up, and the log shows it.
    let unknown = "a".repeat(64);
    let (status, _) = call(
        &public,
        "GET",
        &format!("/api/v1/heartbeat/{unknown}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        lookups(),
        1,
        "statements are visible in the log: {:?}",
        log.lines()
    );

    let too_long = "a".repeat(129);
    let (one_short, one_over) = ("a".repeat(63), "a".repeat(65));
    let (upper, not_hex) = ("A".repeat(64), "g".repeat(64));
    for token in [
        "abc%00def",
        "%0A",
        "x%0D%0Ay",
        "a%20b",
        "%E2%82%AC",
        "%ff",
        "not-a-real-token",
        too_long.as_str(),
        one_short.as_str(),
        one_over.as_str(),
        upper.as_str(),
        not_hex.as_str(),
    ] {
        for method in ["GET", "POST"] {
            let uri = format!("/api/v1/heartbeat/{token}");
            let (status, body) = call(&public, method, &uri, None, None).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{method} {token}");
            assert_eq!(body, json!({ "error": "not found" }), "{method} {token}");
        }
    }
    assert_eq!(lookups(), 1, "none of them reached the database");
    assert!(
        !log.lines()
            .iter()
            .any(|line| line.contains("database error")),
        "{:?}",
        log.lines()
    );
}

/// One request with no session, keeping the headers.
async fn ping_raw(app: &Router, method: &str, token: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let request = Request::builder()
        .method(method)
        .uri(format!("/api/v1/heartbeat/{token}"))
        .body(Body::empty())
        .unwrap();
    let (parts, body) = app.clone().oneshot(request).await.unwrap().into_parts();
    let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (parts.status, parts.headers, body.to_vec())
}

/// POST `body` to a ping path with no session; returns status, headers, JSON.
async fn ping_with(public: &Router, path: &str, body: Vec<u8>) -> (StatusCode, HeaderMap, Value) {
    ping_body(public, path, Body::from(body)).await
}

/// POST any `body` to a ping path with no session; returns status, headers,
/// JSON.
async fn ping_body(public: &Router, path: &str, body: Body) -> (StatusCode, HeaderMap, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded") // what curl --data-binary sends
        .body(body)
        .unwrap();
    let (parts, body) = public.clone().oneshot(request).await.unwrap().into_parts();
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    (
        parts.status,
        parts.headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Whoever pings a token it holds learns nothing about the heartbeat: the same
/// 200 and the same bytes whether it is pending, up, down or paused, and no
/// session or cookie is ever created.
#[sqlx::test]
async fn the_ping_reply_is_the_same_whatever_state_the_heartbeat_is_in(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let mut replies = Vec::new();
    for (status, paused) in [
        ("pending", false),
        ("up", false),
        ("down", false),
        ("pending", true),
        ("up", true),
        ("down", true),
    ] {
        let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
        sqlx::query("update heartbeats set current_status = $2, paused = $3 where id = $1")
            .bind(id)
            .bind(status)
            .bind(paused)
            .execute(&pool)
            .await
            .unwrap();
        for method in ["GET", "POST"] {
            let reply = ping_raw(&public, method, &token).await;
            replies.push((format!("{method} {status} paused={paused}"), reply));
        }
    }

    let (first_label, first) = &replies[0];
    for (label, reply) in &replies {
        assert_eq!(reply, first, "{label} differs from {first_label}");
    }
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(first.2, br#"{"ok":true}"#);
    assert!(first.1.get("set-cookie").is_none(), "{:?}", first.1);
}

/// Every step of a heartbeat's life is traced at every level, the libraries'
/// debug and trace lines included: no token appears in any line.
#[sqlx::test]
async fn no_token_ever_reaches_a_log_line(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let public = common::public_app(state.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/heartbeats");

    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Nightly backup", "period_seconds": 30, "grace_seconds": 0 })),
    )
    .await;
    let token = created["token"].as_str().unwrap().to_string();
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let item = format!("{collection}/{id}");
    let unknown = "f".repeat(64);
    let ping = format!("/api/v1/heartbeat/{token}");

    assert_eq!(
        call(&public, "POST", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &public,
            "GET",
            &format!("/api/v1/heartbeat/{unknown}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // Overdue: down, with an alert; then a ping recovers it, with another.
    // (The sweep counts from the latest of the ping and the creation, so both
    // are moved back.)
    sqlx::query(
        "update heartbeats
         set last_ping_at = now() - interval '1 hour', created_at = now() - interval '2 hours'
         where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(
        call(&public, "POST", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(settled_mail(&recipient, 2).await.len(), 2);
    // Paused pings, edits and the detail view too.
    let patch = Some(json!({ "paused": true }));
    assert_eq!(
        call(&app, "PATCH", &item, Some(&member), patch).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&public, "GET", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &item, Some(&member), None).await.0,
        StatusCode::OK
    );
    // Failure and exit-code pings, with and without a body, and an unknown
    // token on the failure route.
    let patch = Some(json!({ "paused": false }));
    assert_eq!(
        call(&app, "PATCH", &item, Some(&member), patch).await.0,
        StatusCode::OK
    );
    for (path, body) in [
        (format!("{ping}/fail"), b"backup failed: disk full".to_vec()),
        (format!("{ping}/3"), vec![]),
        (format!("{ping}/0"), vec![]),
    ] {
        assert_eq!(
            ping_with(&public, &path, body).await.0,
            StatusCode::OK,
            "{path}"
        );
    }
    assert_eq!(
        ping_with(
            &public,
            &format!("/api/v1/heartbeat/{unknown}/fail"),
            vec![]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    // The second DOWN/UP pair falls inside the paging gap of the heartbeat
    // (R31): its reopened incident pages nobody.
    assert_eq!(settled_mail(&recipient, 2).await.len(), 2);

    let lines = log.lines();
    let all = lines.join("\n");
    assert!(
        all.contains("update heartbeats set last_ping_at"),
        "the capture sees the statements, so it would see a token too:\n{all}"
    );
    for secret in [&token, &unknown] {
        assert!(
            !all.contains(secret.as_str()),
            "a token is in the log:\n{all}"
        );
    }
}

/// The heartbeat a transition is about.
fn heartbeat_of(transition: &Transition) -> Uuid {
    match transition.subject {
        Subject::Heartbeat(id) => id,
        Subject::Monitor(id) => panic!("a monitor ({id}) in a heartbeat sweep"),
        Subject::LogAlert(id) => panic!("a log alert ({id}) in a heartbeat sweep"),
    }
}

/// The heartbeats the transitions are about, in a fixed order.
fn heartbeats_of(transitions: &[Transition]) -> Vec<Uuid> {
    let mut ids: Vec<Uuid> = transitions.iter().map(heartbeat_of).collect();
    ids.sort();
    ids
}

/// One sweep as the worker runs it, but with no restart bound.
async fn sweep(pool: &sqlx::PgPool) -> Vec<Transition> {
    sweep_overdue(pool, SWEEP_BATCH).await.unwrap()
}

/// A ping racing a sweep (`record_ping` takes the heartbeat's row lock first):
/// when the sweep has flipped the heartbeat but not committed, the ping waits
/// on the locking select (observed in `pg_stat_activity`, not assumed from a
/// pause), then sees it down and recovers it, resolving the incident the sweep
/// opened. Without the lock it would read the old state, write `up` over the
/// sweep's `down` and leave the incident open.
#[sqlx::test]
async fn a_ping_waits_for_a_sweep_in_progress_and_then_recovers_what_it_flipped(
    pool: sqlx::PgPool,
) {
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    let mut sweep = pool.begin().await.unwrap();
    sqlx::query("update heartbeats set current_status = 'down' where id = $1")
        .bind(id)
        .execute(&mut *sweep)
        .await
        .unwrap();
    incident::open_for_heartbeat(
        &mut sweep,
        project,
        id,
        "Nightly backup missed its heartbeat",
        None,
        Utc::now(),
    )
    .await
    .unwrap();

    let pinging = tokio::spawn({
        let pool = pool.clone();
        async move { record_ping(&pool, &token).await }
    });
    wait_for_a_lock_wait(&pool, "for update").await;
    assert!(!pinging.is_finished(), "the ping must wait for the sweep");
    sweep.commit().await.unwrap();

    let outcome = pinging.await.unwrap().unwrap();
    let PingOutcome::Recorded {
        transition: Some(transition),
    } = outcome
    else {
        panic!("the ping should have recovered the heartbeat: {outcome:?}");
    };
    assert_eq!(transition.kind, TransitionKind::Recovered);
    assert_eq!(transition.subject, Subject::Heartbeat(id));
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert_eq!(incidents_of(&pool, id).await[0].0, "resolved");
}

/// The other order: a ping in progress (written, not committed) holds the
/// heartbeat's row, and the sweep goes past it instead of waiting for it. That
/// is no false DOWN: once the ping commits the heartbeat is fresh.
#[sqlx::test]
async fn a_sweep_goes_past_a_heartbeat_whose_ping_is_in_progress(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    let mut ping = pool.begin().await.unwrap();
    sqlx::query("update heartbeats set last_ping_at = now(), current_status = 'up' where id = $1")
        .bind(id)
        .execute(&mut *ping)
        .await
        .unwrap();

    let during = tokio::time::timeout(Duration::from_secs(10), sweep(&pool))
        .await
        .expect("a sweep must not wait for a ping in progress");
    assert_eq!(during, vec![]);
    ping.commit().await.unwrap();

    assert_eq!(sweep(&pool).await, vec![], "the ping made it fresh");
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert!(incidents_of(&pool, id).await.is_empty());
}

/// A heartbeat that another transaction holds (an edit, a ping, a sweep that
/// has not committed) is skipped, not waited for: a sweep stuck behind one row
/// would hold up the DOWN detection of every other heartbeat. The skipped one
/// is neither flipped nor lost; the next sweep takes it.
#[sqlx::test]
async fn a_sweep_skips_heartbeats_that_another_transaction_holds(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (held, _) = seed_heartbeat(&pool, project, "held", 30, 0, 120).await;
    let (free, _) = seed_heartbeat(&pool, project, "free", 30, 0, 120).await;
    let mut other = pool.begin().await.unwrap();
    sqlx::query("select id from heartbeats where id = $1 for update")
        .bind(held)
        .fetch_one(&mut *other)
        .await
        .unwrap();

    let swept = tokio::time::timeout(Duration::from_secs(10), sweep(&pool))
        .await
        .expect("a sweep must skip rows another transaction holds, not wait for them");

    assert_eq!(heartbeats_of(&swept), vec![free]);
    assert_eq!(heartbeat_state(&pool, held).await.0, "pending");
    other.commit().await.unwrap();
    assert_eq!(heartbeats_of(&sweep(&pool).await), vec![held]);
    assert_eq!(sweep(&pool).await, vec![]);
}

/// Sweeps started together (overlapping ticks, or several engine instances)
/// take different heartbeats: nothing is flipped, or alerted, twice, and what
/// one skipped the next sweeps pick up.
#[sqlx::test]
async fn sweeps_running_at_the_same_time_flip_each_heartbeat_once(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let mut all = Vec::new();
    for i in 0..12 {
        all.push(
            seed_heartbeat(&pool, project, &format!("hb{i}"), 30, 0, 120)
                .await
                .0,
        );
    }
    all.sort();
    let sweeps: Vec<_> = (0..4)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move { sweep_overdue(&pool, 4).await })
        })
        .collect();
    let mut flipped = Vec::new();
    for sweep in sweeps {
        flipped.extend(heartbeats_of(&sweep.await.unwrap().unwrap()));
    }
    let mut distinct = flipped.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), flipped.len(), "flipped twice: {flipped:?}");

    loop {
        let more = sweep_overdue(&pool, 4).await.unwrap();
        if more.is_empty() {
            break;
        }
        flipped.extend(heartbeats_of(&more));
    }

    flipped.sort();
    assert_eq!(flipped, all, "every heartbeat went down exactly once");
    for id in all {
        assert_eq!(heartbeat_state(&pool, id).await.0, "down");
        assert_eq!(incidents_of(&pool, id).await.len(), 1);
    }
}

/// The deadline is the last ping plus the period plus the grace, to the
/// second: a heartbeat 80 s after its last ping is inside 60 s + 30 s, one 100 s
/// after it is not.
#[sqlx::test]
async fn the_grace_period_extends_the_deadline_by_exactly_that_much(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let mut seeded = Vec::new();
    for (name, seconds_since_ping) in [("inside", 80), ("outside", 100)] {
        let (id, _) = seed_heartbeat(&pool, project, name, 60, 30, 86_400).await;
        sqlx::query(
            "update heartbeats set last_ping_at = now() - make_interval(secs => $2) where id = $1",
        )
        .bind(id)
        .bind(f64::from(seconds_since_ping))
        .execute(&pool)
        .await
        .unwrap();
        seeded.push(id);
    }

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);

    assert_eq!(heartbeat_state(&pool, seeded[0]).await.0, "pending");
    assert_eq!(heartbeat_state(&pool, seeded[1]).await.0, "down");
    assert_eq!(
        incidents_of(&pool, seeded[1]).await[0].2.as_deref(),
        Some("no ping received within 60s (+30s grace)")
    );
}

/// The deadline is computed in float8: a row with the largest period and grace
/// the columns hold (the API stops at 31 days, but a row can be written around
/// it) must not make the whole sweep fail, as an int4 sum would, or no other
/// heartbeat would ever be swept again.
#[sqlx::test]
async fn a_huge_period_and_grace_cannot_break_the_sweep(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (huge, _) = seed_heartbeat(&pool, project, "huge", i32::MAX, i32::MAX, 0).await;
    let (overdue, _) = seed_heartbeat(&pool, project, "overdue", 30, 0, 120).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);

    assert_eq!(heartbeat_state(&pool, huge).await.0, "pending");
    assert_eq!(heartbeat_state(&pool, overdue).await.0, "down");
}

/// The sweep hands its alerts to the same bounded background set as the
/// checks: with an SMTP server that is up but silent, the sweep still
/// returns at once, the heartbeat is down and its incident open, and the
/// alert is still waiting.
#[sqlx::test]
async fn a_hung_smtp_server_does_not_hold_up_the_sweep(pool: sqlx::PgPool) {
    let (smtp_port, _silent) = silent_server().await;
    // The default limit for one email is 10 s: far longer than the sweep takes.
    let state = AppState {
        mailer: Mailer::new("127.0.0.1", smtp_port),
        ..AppState::for_test(pool.clone())
    };
    let (project, _recipient) = project_with_recipient(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    let notifications = Notifications::new();

    let started = Instant::now();
    let swept = sweep_pass(&state, &notifications).await.unwrap();
    let elapsed = started.elapsed();

    assert_eq!(swept, 1);
    assert!(
        elapsed < Duration::from_secs(5),
        "the sweep took {elapsed:?} while the SMTP server never answered"
    );
    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    assert_eq!(incidents_of(&pool, id).await.len(), 1);
    assert_eq!(
        notifications.reap(),
        1,
        "the alert is still waiting on the silent server, off the sweep's path"
    );
}

/// With `limit` alert batches already sending, the sweep neither waits nor
/// loses the state change: the alert is dropped, and the log says which one.
#[sqlx::test]
async fn heartbeat_alerts_are_dropped_and_logged_when_too_many_batches_are_sending(
    pool: sqlx::PgPool,
) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    assert_eq!(
        sweep_pass(&state, &Notifications::with_limit(0))
            .await
            .unwrap(),
        1
    );

    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    assert_eq!(incidents_of(&pool, id).await.len(), 1);
    assert!(settled_mail(&recipient, 0).await.is_empty());
    let lines = log.lines();
    let dropped: Vec<_> = lines
        .iter()
        .filter(|line| line.contains("alerts dropped"))
        .collect();
    assert_eq!(dropped.len(), 1, "{lines:?}");
    assert!(dropped[0].contains(&id.to_string()), "{}", dropped[0]);
    assert!(
        !lines.join("\n").contains(&token),
        "the token is in the log"
    );
}

/// The worker sweeps by itself, and its alerts reach the members: the first
/// tick fires at once, so waiting for the mail is all the test needs.
#[sqlx::test]
async fn run_forever_sweeps_overdue_heartbeats_by_itself(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    let worker = tokio::spawn(run_forever(state.clone()));
    // Waited for before stopping the worker: it owns the tasks that send.
    let mail = settled_mail(&recipient, 1).await;
    worker.abort();

    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    assert_eq!(
        mail.iter()
            .map(|(subject, _)| subject.as_str())
            .collect::<Vec<_>>(),
        vec!["[Nami] [high] Nightly backup missed its heartbeat"]
    );
}

/// A ping only ever touches the heartbeat that holds its token.
#[sqlx::test]
async fn a_ping_touches_only_the_heartbeat_that_holds_the_token(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let (first_project, other_project) = (seed_project(&pool).await, seed_project(&pool).await);
    let (pinged, token) = seed_heartbeat(&pool, first_project, "pinged", 3600, 60, 0).await;
    let (same_project, _) = seed_heartbeat(&pool, first_project, "same project", 3600, 60, 0).await;
    let (other, _) = seed_heartbeat(&pool, other_project, "other project", 3600, 60, 0).await;

    assert_eq!(
        call(
            &public,
            "POST",
            &format!("/api/v1/heartbeat/{token}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );

    assert_eq!(
        heartbeat_state(&pool, pinged).await,
        ("up".to_string(), true)
    );
    for untouched in [same_project, other] {
        assert_eq!(
            heartbeat_state(&pool, untouched).await,
            ("pending".to_string(), false)
        );
    }
}

/// A heartbeat has at most one unresolved incident, and can open a new one
/// once the last is resolved (`incidents_one_unresolved_per_heartbeat`).
#[sqlx::test]
async fn a_heartbeat_has_at_most_one_open_incident(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 0).await;
    let mut conn = pool.acquire().await.unwrap();

    let first = incident::open_for_heartbeat(&mut conn, project, id, "first", None, Utc::now())
        .await
        .unwrap();
    let again = incident::open_for_heartbeat(&mut conn, project, id, "again", None, Utc::now())
        .await
        .unwrap();
    assert!(first.is_some());
    assert_eq!(again, None);
    assert_eq!(
        incident::resolve_for_heartbeat(&mut conn, id, Utc::now())
            .await
            .unwrap(),
        first
    );
    assert_eq!(
        incident::resolve_for_heartbeat(&mut conn, id, Utc::now())
            .await
            .unwrap(),
        None
    );
    // Soon after its resolution, the incident is reopened, not followed.
    let reopened = incident::open_for_heartbeat(&mut conn, project, id, "next", None, Utc::now())
        .await
        .unwrap();
    assert_eq!(reopened, first);
    assert_eq!(
        incident::open_for_heartbeat(&mut conn, project, id, "again", None, Utc::now())
            .await
            .unwrap(),
        None,
        "still at most one open"
    );
    incident::resolve_for_heartbeat(&mut conn, id, Utc::now() - chrono::Duration::minutes(6))
        .await
        .unwrap();
    // Resolved longer ago than the reopen window: a new incident.
    let next = incident::open_for_heartbeat(&mut conn, project, id, "next", None, Utc::now())
        .await
        .unwrap();
    assert!(next.is_some());
    assert_ne!(next, first);
}

/// Deleting a heartbeat, or its project, deletes its incidents; a deleted
/// heartbeat's token stops answering.
#[sqlx::test]
async fn deleting_a_heartbeat_or_its_project_deletes_its_incidents(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let public = common::public_app(state.clone());
    let project = seed_project(&pool).await;
    let (doomed, token) = seed_heartbeat(&pool, project, "doomed", 30, 0, 120).await;
    let (kept, _) = seed_heartbeat(&pool, project, "kept", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;

    let uri = format!("/projects/{project}/heartbeats/{doomed}");
    assert_eq!(
        call(&app, "DELETE", &uri, Some(&admin), None).await.0,
        StatusCode::OK
    );

    assert!(incidents_of(&pool, doomed).await.is_empty());
    assert_eq!(incidents_of(&pool, kept).await.len(), 1);
    assert_eq!(
        call(
            &public,
            "POST",
            &format!("/api/v1/heartbeat/{token}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    sqlx::query("delete from projects where id = $1")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    let (heartbeats, incidents): (i64, i64) = sqlx::query_as(
        "select (select count(*) from heartbeats), (select count(*) from incidents)",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((heartbeats, incidents), (0, 0));
}

/// Viewers read but never change anything; callers without a session get 401
/// from every project route, and a global admin who is no member can manage
/// heartbeats (and gets a 404 for a project that does not exist).
#[sqlx::test]
async fn heartbeat_routes_check_the_callers_role(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let global_admin_email = unique_email("root");
    seed_user(&pool, &global_admin_email, "admin").await;
    let root = common::login(&app, &global_admin_email).await;
    let collection = format!("/projects/{project}/heartbeats");
    let item = format!("{collection}/{id}");
    let patch = || Some(json!({ "name": "renamed" }));

    assert_eq!(
        call(&app, "GET", &collection, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    let (status, detail) = call(&app, "GET", &item, Some(&viewer), None).await;
    assert_eq!(
        (status, &detail["token"]),
        (StatusCode::OK, &Value::Null),
        "viewers do not get the ping token"
    );
    assert_eq!(
        call(&app, "PATCH", &item, Some(&viewer), patch()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&viewer), None).await.0,
        StatusCode::FORBIDDEN
    );

    let new = Some(json!({ "name": "x", "period_seconds": 60 }));
    assert_eq!(
        call(&app, "GET", &collection, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "POST", &collection, None, new.clone()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", &item, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "PATCH", &item, None, patch()).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "DELETE", &item, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(
        call(&app, "PATCH", &item, Some(&root), patch()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "POST", &collection, Some(&root), new.clone())
            .await
            .0,
        StatusCode::CREATED
    );
    let nowhere = format!("/projects/{}/heartbeats", Uuid::new_v4());
    assert_eq!(
        call(&app, "POST", &nowhere, Some(&root), new).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&root), None).await.0,
        StatusCode::OK
    );
}

/// The token and the state belong to the server: a body that names them is
/// not obeyed, on create or on edit, and every heartbeat gets its own token.
#[sqlx::test]
async fn clients_cannot_choose_the_token_the_state_or_the_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/heartbeats");
    let body = json!({
        "name": "Nightly backup",
        "period_seconds": 60,
        "token": "chosen-by-the-client",
        "current_status": "down",
        "last_ping_at": "2020-01-01T00:00:00Z",
        "project_id": other_project,
        "id": Uuid::nil(),
    });

    let (status, first) = call(&app, "POST", &collection, Some(&member), Some(body.clone())).await;
    let (_, second) = call(&app, "POST", &collection, Some(&member), Some(body)).await;

    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_ne!(first["token"], "chosen-by-the-client");
    assert_ne!(
        first["token"], second["token"],
        "every heartbeat has its own token"
    );
    assert_eq!(first["current_status"], "pending");
    assert_eq!(first["last_ping_at"], Value::Null);
    assert_eq!(first["project_id"], project.to_string());
    assert_ne!(first["id"], Uuid::nil().to_string());

    let item = format!("{collection}/{}", first["id"].as_str().unwrap());
    let edit = json!({
        "token": "chosen-by-the-client",
        "current_status": "down",
        "last_ping_at": "2020-01-01T00:00:00Z",
        "project_id": other_project,
    });
    let (status, patched) = call(&app, "PATCH", &item, Some(&member), Some(edit)).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched, first, "nothing the client may not edit changed");
}

/// An edit is validated as a whole: one bad value rejects it and nothing is
/// written, and a name that could inject mail headers never gets in.
#[sqlx::test]
async fn an_invalid_edit_is_rejected_and_changes_nothing(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 60, 10, 0).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let item = format!("/projects/{project}/heartbeats/{id}");
    let (_, before) = call(&app, "GET", &item, Some(&member), None).await;

    for (edit, fragment) in [
        (
            json!({ "name": "renamed", "period_seconds": 29 }),
            "period_seconds",
        ),
        (
            json!({ "name": "renamed", "grace_seconds": -1 }),
            "grace_seconds",
        ),
        (
            json!({ "name": "x\r\nBcc: evil@evil.test" }),
            "control characters",
        ),
        (json!({ "name": "x".repeat(201) }), "200 characters"),
    ] {
        let (status, error) = call(&app, "PATCH", &item, Some(&member), Some(edit.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{edit}: {error}");
        assert!(
            error["error"].as_str().unwrap().contains(fragment),
            "{edit}: {error}"
        );
    }
    let hostile = json!({ "name": "x\r\nBcc: evil@evil.test", "period_seconds": 60 });
    let (status, _) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(&member),
        Some(hostile),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, after) = call(&app, "GET", &item, Some(&member), None).await;
    assert_eq!(after, before);
    let count: i64 = sqlx::query_scalar("select count(*) from heartbeats")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

// ---------------------------------------------------------------------------
// Fix round 1: ping tokens belong to members, rotation, pause/resume, limits.
// ---------------------------------------------------------------------------

/// Waits (up to 10 s) until a backend of this test's database is blocked on a
/// lock while running a statement that contains `statement`. The wait is
/// observed, not assumed from a pause: a statement that never waits (a removed
/// `for update`, say) fails the test instead of slipping past it.
async fn wait_for_a_lock_wait(pool: &sqlx::PgPool, statement: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
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
            Instant::now() < deadline,
            "no statement containing {statement:?} ever waited for a lock"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A ping token lets whoever holds it mark the heartbeat up (resolving its
/// incident, hiding a dead job), so viewers read heartbeats without it; members
/// and above get it, a global admin included.
#[sqlx::test]
async fn viewers_never_see_the_ping_token(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let root_email = unique_email("root");
    seed_user(&pool, &root_email, "admin").await;
    let root = common::login(&app, &root_email).await;
    let collection = format!("/projects/{project}/heartbeats");
    let item = format!("{collection}/{id}");

    for (who, cookie, sees_token) in [
        ("viewer", &viewer, false),
        ("member", &member, true),
        ("project admin", &admin, true),
        ("global admin", &root, true),
    ] {
        let (list_status, list) = call(&app, "GET", &collection, Some(cookie), None).await;
        let (detail_status, detail) = call(&app, "GET", &item, Some(cookie), None).await;
        assert_eq!(
            (list_status, detail_status),
            (StatusCode::OK, StatusCode::OK),
            "{who}"
        );
        let expected = if sees_token {
            json!(token)
        } else {
            Value::Null
        };
        assert_eq!(list[0]["token"], expected, "{who}: list");
        assert_eq!(detail["token"], expected, "{who}: detail");
        if !sees_token {
            assert!(
                list[0].get("token").is_some_and(Value::is_null),
                "the key is there, null: {}",
                list[0]
            );
            let everything = format!("{list} {detail}");
            assert!(
                !everything.contains(&token),
                "{who} got the token: {everything}"
            );
            assert_eq!(
                detail["name"], "Nightly backup",
                "the rest is still readable"
            );
        }
    }
}

/// Members get the token back from every response that returns a heartbeat.
#[sqlx::test]
async fn members_get_the_token_from_create_and_edit_responses(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/heartbeats");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Nightly backup", "period_seconds": 3600 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let token = created["token"].as_str().unwrap();
    assert_eq!(token.len(), 64);
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    let (_, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "x" })),
    )
    .await;
    assert_eq!(patched["token"], token);
}

/// Rotating replaces the token at once: the old one stops working, the new one
/// works, and nothing else about the heartbeat changes.
#[sqlx::test]
async fn rotating_the_token_replaces_it_at_once(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/heartbeats");
    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Nightly backup", "period_seconds": 3600 })),
    )
    .await;
    let first = created["token"].as_str().unwrap().to_string();
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    let rotate = format!("{item}/rotate-token");
    assert_eq!(
        call(
            &public,
            "POST",
            &format!("/api/v1/heartbeat/{first}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, before) = call(&app, "GET", &item, Some(&member), None).await;

    let (status, rotated) = call(&app, "POST", &rotate, Some(&member), Some(json!({}))).await;

    assert_eq!(status, StatusCode::OK, "{rotated}");
    let second = rotated["token"].as_str().unwrap().to_string();
    assert_eq!(second.len(), 64);
    assert!(second.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(second, first);
    assert_eq!(
        (
            rotated["id"].clone(),
            rotated["name"].clone(),
            rotated["current_status"].clone()
        ),
        (before["id"].clone(), before["name"].clone(), json!("up")),
    );
    assert_eq!(
        rotated["last_ping_at"], before["last_ping_at"],
        "a rotation is not a ping"
    );
    let (status, body) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{first}"),
        None,
        None,
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::NOT_FOUND, json!({ "error": "not found" }))
    );
    assert_eq!(
        call(
            &public,
            "GET",
            &format!("/api/v1/heartbeat/{second}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::OK
    );
    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    assert_eq!(detail["token"], second.as_str());
    assert_eq!(
        call(&app, "GET", &item, Some(&viewer), None).await.1["token"],
        Value::Null
    );

    let (_, again) = call(&app, "POST", &rotate, Some(&member), Some(json!({}))).await;
    let third = again["token"].as_str().unwrap();
    assert!(third != second && third != first);
    assert_eq!(
        call(
            &public,
            "GET",
            &format!("/api/v1/heartbeat/{second}"),
            None,
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// Members rotate (and global admins), viewers and outsiders are refused, a
/// heartbeat of another project is a 404, and every refusal leaves the token
/// as it was.
#[sqlx::test]
async fn only_members_rotate_and_only_within_their_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
    let (foreign, foreign_token) = seed_heartbeat(&pool, other_project, "foreign", 60, 0, 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let root_email = unique_email("root");
    seed_user(&pool, &root_email, "admin").await;
    let root = common::login(&app, &root_email).await;
    let rotate = format!("/projects/{project}/heartbeats/{id}/rotate-token");
    let token_of = |id: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("select token from heartbeats where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };

    assert_eq!(
        call(&app, "POST", &rotate, Some(&viewer), Some(json!({})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", &rotate, Some(&outsider), Some(json!({})))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", &rotate, None, Some(json!({}))).await.0,
        StatusCode::UNAUTHORIZED
    );
    let foreign_uri = format!("/projects/{project}/heartbeats/{foreign}/rotate-token");
    assert_eq!(
        call(&app, "POST", &foreign_uri, Some(&member), Some(json!({})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let unknown = format!(
        "/projects/{project}/heartbeats/{}/rotate-token",
        Uuid::new_v4()
    );
    assert_eq!(
        call(&app, "POST", &unknown, Some(&member), Some(json!({})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        token_of(id).await,
        token,
        "refused rotations change nothing"
    );
    assert_eq!(
        token_of(foreign).await,
        foreign_token,
        "nor does a cross-project one"
    );

    assert_eq!(
        call(&app, "POST", &rotate, Some(&member), Some(json!({})))
            .await
            .0,
        StatusCode::OK
    );
    assert_ne!(token_of(id).await, token);
    let (status, body) = call(&app, "POST", &rotate, Some(&root), Some(json!({}))).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a global admin who is no member may rotate: {body}"
    );
}

/// A ping that has found the old token but not yet the row lock, while a
/// rotation is committing, loses: it re-checks the token under the lock, so
/// nothing is recorded once the rotation is done.
#[sqlx::test]
async fn a_ping_in_flight_during_a_rotation_does_not_survive_it(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
    let mut rotation = pool.begin().await.unwrap();
    sqlx::query("update heartbeats set token = 'rotated' where id = $1")
        .bind(id)
        .execute(&mut *rotation)
        .await
        .unwrap();

    let pinging = tokio::spawn({
        let pool = pool.clone();
        async move { record_ping(&pool, &token).await }
    });
    wait_for_a_lock_wait(&pool, "for update").await;
    rotation.commit().await.unwrap();

    assert_eq!(pinging.await.unwrap().unwrap(), PingOutcome::UnknownToken);
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false)
    );
}

/// Period and grace stop at 31 days, on create and on edit.
#[sqlx::test]
async fn period_and_grace_are_capped_at_31_days(pool: sqlx::PgPool) {
    const MAX: i64 = 31 * 24 * 60 * 60;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/heartbeats");

    for (body, accepted) in [
        (json!({ "name": "x", "period_seconds": MAX }), true),
        (json!({ "name": "x", "period_seconds": MAX + 1 }), false),
        (
            json!({ "name": "x", "period_seconds": 60, "grace_seconds": MAX }),
            true,
        ),
        (
            json!({ "name": "x", "period_seconds": 60, "grace_seconds": MAX + 1 }),
            false,
        ),
        (json!({ "name": "x", "period_seconds": i32::MAX }), false),
    ] {
        let (status, reply) =
            call(&app, "POST", &collection, Some(&member), Some(body.clone())).await;
        if accepted {
            assert_eq!(status, StatusCode::CREATED, "{body}: {reply}");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {reply}");
            let field = if body.get("grace_seconds").is_some() {
                "grace_seconds"
            } else {
                "period_seconds"
            };
            let message = reply["error"].as_str().unwrap();
            assert!(
                message.contains(field) && message.contains("2678400"),
                "{body}: {reply}"
            );
        }
    }

    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
    let item = format!("{collection}/{id}");
    for field in ["period_seconds", "grace_seconds"] {
        let (status, reply) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ field: MAX + 1 })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {reply}");
        let (status, reply) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ field: MAX })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{field}: {reply}");
    }
}

/// A ping is a GET with a side effect: no cache may answer it or keep a copy,
/// whatever the reply.
#[sqlx::test]
async fn ping_replies_are_never_cacheable(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let unknown = "e".repeat(64);

    for (label, method, token, expected) in [
        ("GET, known", "GET", token.as_str(), StatusCode::OK),
        ("POST, known", "POST", token.as_str(), StatusCode::OK),
        (
            "GET, unknown",
            "GET",
            unknown.as_str(),
            StatusCode::NOT_FOUND,
        ),
        ("POST, not a token", "POST", "a%20b", StatusCode::NOT_FOUND),
        (
            "PUT, known",
            "PUT",
            token.as_str(),
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let (status, headers, _) = ping_raw(&public, method, token).await;
        assert_eq!(status, expected, "{label}");
        assert_eq!(
            headers.get("cache-control").map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{label}"
        );
    }
    // The failure and exit-code routes, a bad suffix (400) and a heartbeat
    // over its rate limit (429).
    let limits = Arc::new(TokenBuckets::new(0.000_1, 2.0));
    let limited = nami_engine::core::router::build_public_router_with_ping_limits(
        AppState::for_test(pool.clone()),
        Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT)),
        limits,
    );
    for (label, path, expected) in [
        (
            "fail",
            format!("/api/v1/heartbeat/{token}/fail"),
            StatusCode::OK,
        ),
        (
            "exit code",
            format!("/api/v1/heartbeat/{token}/1"),
            StatusCode::OK,
        ),
        (
            "bad suffix",
            format!("/api/v1/heartbeat/{token}/nope"),
            StatusCode::BAD_REQUEST,
        ),
        (
            "rate limited",
            format!("/api/v1/heartbeat/{token}/0"),
            StatusCode::TOO_MANY_REQUESTS,
        ),
    ] {
        let (status, headers, _) = ping_with(&limited, &path, vec![]).await;
        assert_eq!(status, expected, "{label}");
        assert_eq!(
            headers.get("cache-control").map(|v| v.to_str().unwrap()),
            Some("no-store"),
            "{label}"
        );
    }
}

/// An unknown token costs one plain lookup: the locking statement, which only
/// runs inside a transaction, is never issued for it (a known token's ping
/// issues it once). sqlx does not log `BEGIN`, so the statement stands in for
/// the transaction.
#[sqlx::test]
async fn an_unknown_token_is_answered_by_a_plain_lookup_not_a_locking_transaction(
    pool: sqlx::PgPool,
) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let statements_containing = |text: &str| {
        log.lines()
            .iter()
            .filter(|line| line.contains("sqlx::query") && line.contains(text))
            .count()
    };

    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(statements_containing("for update"), 1, "{:?}", log.lines());
    let lookups_before = statements_containing("from heartbeats where token");

    let unknown = "d".repeat(64);
    for method in ["GET", "POST"] {
        let (status, _) = call(
            &public,
            method,
            &format!("/api/v1/heartbeat/{unknown}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    assert_eq!(
        statements_containing("from heartbeats where token"),
        lookups_before + 2,
        "each unknown ping is one lookup: {:?}",
        log.lines()
    );
    assert_eq!(
        statements_containing("for update"),
        1,
        "no locking statement for unknown tokens"
    );
}

/// A heartbeat that was paused for longer than its period and grace, with its
/// job switched off meanwhile, gets a whole period and grace after the resume
/// before it can go down: pausing must not turn into a false DOWN the moment
/// it is resumed.
#[sqlx::test]
async fn a_resumed_heartbeat_gets_a_full_period_before_it_can_go_down(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 86_400).await;
    sqlx::query("update heartbeats set paused = true, last_ping_at = now() - interval '1 day' where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let item = format!("/projects/{project}/heartbeats/{id}");
    assert_eq!(
        sweep_heartbeats(&state).await.unwrap(),
        0,
        "paused heartbeats are not swept"
    );

    let (status, _) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "paused": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(
        sweep_heartbeats(&state).await.unwrap(),
        0,
        "resuming starts the clock afresh"
    );
    assert_eq!(heartbeat_state(&pool, id).await.0, "pending");
    // Time passes with no ping: 29 s after the resume is still inside 30 s + 0 s.
    let resumed_ago = |seconds: i32| {
        let pool = pool.clone();
        async move {
            sqlx::query("update heartbeats set resumed_at = now() - make_interval(secs => $2) where id = $1")
                .bind(id)
                .bind(f64::from(seconds))
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    resumed_ago(29).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    resumed_ago(31).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
}

/// Only a resume moves the clock: creating, renaming, pausing, and "resuming"
/// a heartbeat that is not paused leave `resumed_at` as it was.
#[sqlx::test]
async fn only_resuming_moves_the_resume_clock(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/heartbeats");
    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Nightly backup", "period_seconds": 3600 })),
    )
    .await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let item = format!("{collection}/{id}");
    let resumed_at = || {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<chrono::DateTime<Utc>>>(
                "select resumed_at from heartbeats where id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let patch = |body: Value| {
        let (app, item, member) = (app.clone(), item.clone(), member.clone());
        async move {
            assert_eq!(
                call(&app, "PATCH", &item, Some(&member), Some(body))
                    .await
                    .0,
                StatusCode::OK
            )
        }
    };
    assert_eq!(resumed_at().await, None, "created: never resumed");

    patch(json!({ "name": "renamed" })).await;
    patch(json!({ "paused": false })).await;
    assert_eq!(resumed_at().await, None, "not paused: nothing to resume");
    patch(json!({ "paused": true })).await;
    patch(json!({ "paused": true })).await;
    assert_eq!(resumed_at().await, None, "pausing is not resuming");

    patch(json!({ "paused": false })).await;
    let first = resumed_at().await;
    assert!(first.is_some(), "the resume is recorded");
    patch(json!({ "name": "renamed again" })).await;
    patch(json!({ "paused": false })).await;
    assert_eq!(
        resumed_at().await,
        first,
        "only a paused -> unpaused edit moves it"
    );
}

/// The sweep counts from the latest of the last ping, the last resume and the
/// creation: a ping or a resume 10 s ago keeps a 60 s heartbeat alive however
/// old the others are, and each of them still expires on its own.
#[sqlx::test]
async fn the_sweep_counts_from_the_latest_of_ping_resume_and_creation(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let mut ids = Vec::new();
    // (name, last ping s ago, resumed s ago, due?)
    for (name, ping, resumed, due) in [
        ("resumed after the last ping", Some(100), Some(10), false),
        ("pinged after the resume", Some(10), Some(86_400), false),
        ("resumed long enough ago", Some(100), Some(90), true),
        ("pinged long enough ago", Some(90), Some(100), true),
        ("only created", None, None, true),
        ("only created, resumed", None, Some(10), false),
    ] {
        let (id, _) = seed_heartbeat(&pool, project, name, 60, 0, 100).await;
        sqlx::query(
            "update heartbeats
             set last_ping_at = now() - make_interval(secs => $2),
                 resumed_at = now() - make_interval(secs => $3)
             where id = $1",
        )
        .bind(id)
        .bind(ping.map(f64::from))
        .bind(resumed.map(f64::from))
        .execute(&pool)
        .await
        .unwrap();
        ids.push((id, name, due));
    }

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 3);

    for (id, name, due) in ids {
        let expected = if due { "down" } else { "pending" };
        assert_eq!(heartbeat_state(&pool, id).await.0, expected, "{name}");
    }
}

/// Pretends the worker last recorded that it was alive `seconds_ago` seconds
/// ago.
async fn set_last_alive(pool: &sqlx::PgPool, seconds_ago: i32) {
    sqlx::query(
        "insert into worker_state (id, last_alive_at)
         values (1, now() - make_interval(secs => $1))
         on conflict (id) do update set last_alive_at = excluded.last_alive_at",
    )
    .bind(f64::from(seconds_ago))
    .execute(pool)
    .await
    .unwrap();
}

async fn outage_anchor(pool: &sqlx::PgPool, id: Uuid) -> Option<DateTime<Utc>> {
    sqlx::query_scalar("select outage_anchor_at from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The first start ever has no outage to make up for: it records the liveness
/// and leaves every deadline alone, so an overdue heartbeat goes down.
#[sqlx::test]
async fn the_first_start_ever_gives_no_heartbeat_extra_time(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    assert_eq!(note_worker_alive(&pool, true).await.unwrap(), None);

    let recorded: bool = sqlx::query_scalar(
        "select last_alive_at > now() - interval '5 seconds' from worker_state where id = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(recorded);
    assert_eq!(outage_anchor(&pool, id).await, None);
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
}

/// The worker was alive until 50 s ago. Only the heartbeat whose deadline
/// fell inside the outage gets a fresh period; one overdue before the outage
/// goes down, one due later keeps its deadline, and paused or down heartbeats
/// are left alone.
#[sqlx::test]
async fn a_restart_gives_a_fresh_period_only_to_heartbeats_due_during_the_outage(
    pool: sqlx::PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    // Period 30 s, no grace: due 30 s after creation.
    let (before, _) = seed_heartbeat(&pool, project, "due 90 s ago", 30, 0, 120).await;
    let (during, _) = seed_heartbeat(&pool, project, "due 30 s ago", 30, 0, 60).await;
    let (later, _) = seed_heartbeat(&pool, project, "due in 20 s", 30, 0, 10).await;
    let (paused, _) = seed_heartbeat(&pool, project, "paused", 30, 0, 60).await;
    sqlx::query("update heartbeats set paused = true where id = $1")
        .bind(paused)
        .execute(&pool)
        .await
        .unwrap();
    set_last_alive(&pool, 50).await;

    let outage = note_worker_alive(&pool, true)
        .await
        .unwrap()
        .expect("the time since the last record is an outage");

    assert_eq!(outage.extended, 1);
    let length = (outage.to - outage.from).num_seconds();
    assert!((49..=52).contains(&length), "an outage of {length} s");
    assert_eq!(outage_anchor(&pool, during).await, Some(outage.to));
    for untouched in [before, later, paused] {
        assert_eq!(outage_anchor(&pool, untouched).await, None);
    }
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(heartbeat_state(&pool, before).await.0, "down");
    for alive in [during, later, paused] {
        assert_eq!(heartbeat_state(&pool, alive).await.0, "pending");
    }
}

/// An engine that restarts every 20 s must not hide a dead job: the heartbeat
/// overdue before the first outage goes down at the first restart, and the one
/// whose deadline fell inside it gets one fresh period, not one per restart:
/// once that period has passed while the worker was alive, it goes down too.
#[sqlx::test]
async fn frequent_restarts_cannot_hide_a_dead_job(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (overdue, _) = seed_heartbeat(&pool, project, "overdue", 30, 0, 120).await;
    let (during, _) = seed_heartbeat(&pool, project, "due during", 30, 0, 40).await;

    for restart in 0..5 {
        // Alive until 20 s ago, starting again now.
        set_last_alive(&pool, 20).await;
        note_worker_alive(&pool, true).await.unwrap();
        sweep_heartbeats(&state).await.unwrap();
        assert_eq!(
            heartbeat_state(&pool, overdue).await.0,
            "down",
            "restart {restart}"
        );
        assert_eq!(
            heartbeat_state(&pool, during).await.0,
            "pending",
            "restart {restart}"
        );
    }
    assert_eq!(incidents_of(&pool, overdue).await.len(), 1);

    // Its fresh period ran out 20 s ago, while the worker was alive (it last
    // recorded so 2 s ago): the next restart gives it no other one.
    sqlx::query(
        "update heartbeats set outage_anchor_at = now() - interval '50 seconds' where id = $1",
    )
    .bind(during)
    .execute(&pool)
    .await
    .unwrap();
    set_last_alive(&pool, 2).await;
    assert_eq!(
        note_worker_alive(&pool, true)
            .await
            .unwrap()
            .map(|o| o.extended),
        Some(0)
    );
    sweep_heartbeats(&state).await.unwrap();
    assert_eq!(heartbeat_state(&pool, during).await.0, "down");
}

/// While the worker keeps running, a short gap between two records is normal
/// (they come every few seconds) and makes up for nothing. A gap longer than
/// `OUTAGE_GAP` means the database was out of reach, and pings with it: the
/// heartbeats that fell due during it get a fresh period.
#[sqlx::test]
async fn only_a_long_gap_while_running_counts_as_an_outage(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (due, _) = seed_heartbeat(&pool, project, "due 5 s ago", 30, 0, 35).await;

    set_last_alive(&pool, 10).await;
    assert_eq!(note_worker_alive(&pool, false).await.unwrap(), None);
    assert_eq!(outage_anchor(&pool, due).await, None);

    let gap = OUTAGE_GAP.as_secs() as i32 + 30;
    set_last_alive(&pool, gap).await;
    let outage = note_worker_alive(&pool, false)
        .await
        .unwrap()
        .expect("a gap longer than OUTAGE_GAP is an outage");
    assert_eq!(outage.extended, 1);
    assert_eq!(outage_anchor(&pool, due).await, Some(outage.to));
}

/// A heartbeat pinged after the fresh period began counts from its ping, like
/// any other: the outage anchor only ever pushes a deadline back.
#[sqlx::test]
async fn the_outage_anchor_is_one_more_anchor_not_a_deadline(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 86_400).await;
    sqlx::query(
        "update heartbeats
         set outage_anchor_at = now() - interval '100 seconds',
             last_ping_at = now() - interval '10 seconds'
         where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        sweep_heartbeats(&state).await.unwrap(),
        0,
        "pinged 10 s ago"
    );

    sqlx::query(
        "update heartbeats
         set outage_anchor_at = now() - interval '10 seconds',
             last_ping_at = now() - interval '100 seconds'
         where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(
        sweep_heartbeats(&state).await.unwrap(),
        0,
        "a fresh period that began 10 s ago"
    );

    sqlx::query(
        "update heartbeats set outage_anchor_at = now() - interval '61 seconds' where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
}

/// One sweep takes at most `limit` heartbeats, the most overdue first, so a
/// burst of overdue heartbeats (an outage of the jobs' host) is alerted in
/// batches, not all at once.
#[sqlx::test]
async fn a_sweep_takes_at_most_its_limit_the_most_overdue_first(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let mut by_overdue = Vec::new();
    for seconds in [500, 100, 400, 200, 300] {
        let (id, _) = seed_heartbeat(&pool, project, &format!("{seconds}"), 30, 0, seconds).await;
        by_overdue.push((seconds, id));
    }
    by_overdue.sort();
    let ids: Vec<Uuid> = by_overdue.iter().map(|(_, id)| *id).collect();
    let most_overdue = |range: std::ops::Range<usize>| {
        let mut picked: Vec<Uuid> = ids[range].to_vec();
        picked.sort();
        picked
    };

    let first = sweep_overdue(&pool, 2).await.unwrap();
    assert_eq!(heartbeats_of(&first), most_overdue(3..5), "500 s and 400 s");
    let second = sweep_overdue(&pool, 2).await.unwrap();
    assert_eq!(
        heartbeats_of(&second),
        most_overdue(1..3),
        "300 s and 200 s"
    );
    let third = sweep_overdue(&pool, 2).await.unwrap();
    assert_eq!(heartbeats_of(&third), most_overdue(0..1), "100 s");
    assert_eq!(sweep_overdue(&pool, 2).await.unwrap(), vec![]);
    for id in ids {
        assert_eq!(incidents_of(&pool, id).await.len(), 1);
    }
}

/// A worker pass flips at most `SWEEP_BATCH` heartbeats, and the next pass
/// takes the rest.
#[sqlx::test]
async fn a_pass_flips_a_bounded_batch_and_the_next_takes_the_rest(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let extra = 5;
    sqlx::query(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         select $1, 'hb' || n, md5(random()::text || n::text), 30, 0, now() - interval '120 seconds'
         from generate_series(1, $2::bigint) n",
    )
    .bind(project)
    .bind(SWEEP_BATCH + extra)
    .execute(&pool)
    .await
    .unwrap();
    let down = || async {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from heartbeats where current_status = 'down'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
    };

    assert_eq!(sweep_heartbeats(&state).await.unwrap() as i64, SWEEP_BATCH);
    assert_eq!(down().await, SWEEP_BATCH);
    assert_eq!(sweep_heartbeats(&state).await.unwrap() as i64, extra);
    assert_eq!(down().await, SWEEP_BATCH + extra);
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    let incidents: i64 = sqlx::query_scalar("select count(*) from incidents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(incidents, SWEEP_BATCH + extra, "one incident each");
}

/// A slow check pass must not delay DOWN detection: the sweep runs on its own
/// schedule, and its first run is at once. The monitor's check hangs (a silent
/// server, up to its 5 s timeout), so the pass is still running when the
/// heartbeat goes down.
#[sqlx::test]
async fn the_sweep_runs_before_a_slow_check_pass(pool: sqlx::PgPool) {
    let (port, _silent) = silent_server().await;
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(
        &pool,
        project,
        "Hung API",
        &format!("http://127.0.0.1:{port}/"),
        1,
    )
    .await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;

    let worker = tokio::spawn(run_forever(state));
    let deadline = Instant::now() + Duration::from_secs(3);
    while heartbeat_state(&pool, id).await.0 != "down" && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let checked: bool =
        sqlx::query_scalar("select last_checked_at is not null from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap();
    worker.abort();

    assert_eq!(
        heartbeat_state(&pool, id).await.0,
        "down",
        "the heartbeat should go down while the check is still hanging"
    );
    assert!(
        !checked,
        "the check pass had not finished when the heartbeat went down"
    );
}

// ---------------------------------------------------------------------------
// Final review: pings live on the public listener only, and shed load.
// ---------------------------------------------------------------------------

/// The ping endpoint is served by the public listener and by nothing else: the
/// private API answers 404 for it and the heartbeat is not touched.
#[sqlx::test]
async fn pings_are_served_on_the_public_listener_only(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let ping = format!("/api/v1/heartbeat/{token}");

    // The POST carries JSON: the API refuses anything else with 415 before
    // routing, which would hide whether the route exists.
    for (method, body) in [("GET", None), ("POST", Some(json!({})))] {
        let (status, _) = call(&app, method, &ping, None, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} on the API");
    }
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false)
    );

    assert_eq!(
        call(&public, "POST", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));
}

/// Nothing of the API is reachable through the public listener, so exposing
/// it to cron jobs exposes nothing else (in particular not the login, whose
/// rate limit trusts the `X-Forwarded-For` nami-ui sends). Its `/health` says
/// the listener is up without touching the database: it answers even when the
/// database cannot be reached at all.
#[sqlx::test]
async fn the_public_listener_serves_none_of_the_api(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    for (method, uri, body) in [
        (
            "POST",
            "/auth/login".to_string(),
            Some(json!({ "email": "a@example.com", "password": "x" })),
        ),
        (
            "POST",
            "/auth/register".to_string(),
            Some(json!({ "email": "a@example.com", "password": "x" })),
        ),
        ("GET", "/auth/me".to_string(), None),
        ("GET", "/projects".to_string(), None),
        ("GET", format!("/projects/{project}/monitors"), None),
        ("GET", format!("/projects/{project}/heartbeats"), None),
        ("GET", format!("/projects/{project}/incidents"), None),
        ("GET", format!("/projects/{project}/sources"), None),
        (
            "POST",
            format!("/projects/{project}/sources"),
            Some(json!({ "name": "x" })),
        ),
    ] {
        let (status, _) = call(&public, method, &uri, None, body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
    }

    let unreachable = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(200))
        .connect_lazy("postgres://nami:nami@127.0.0.1:1/nami")
        .unwrap();
    let public = common::test_public_app(unreachable);
    let (status, body) = call(&public, "GET", "/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "status": "ok" }));
}

/// With every ping place taken, a ping waits PING_SLOT_WAIT for one and is
/// then turned away with 503 and `Retry-After: 1`, before any database work,
/// and the heartbeat is not touched. Text that is not a token is still an ordinary 404: it needs no
/// place. Once a place is free, pings are served again.
#[sqlx::test]
async fn a_full_ping_endpoint_turns_pings_away_with_retry_after(pool: sqlx::PgPool) {
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(2));
    let public = nami_engine::core::router::build_public_router_with_ping_slots(
        AppState::for_test(pool.clone()),
        slots.clone(),
    );
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let held = slots.clone().acquire_many_owned(2).await.unwrap();

    for method in ["GET", "POST"] {
        let started = Instant::now();
        let (status, headers, body) = ping_raw(&public, method, &token).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{method}");
        let waited = started.elapsed();
        assert!(
            waited >= PING_SLOT_WAIT && waited < PING_SLOT_WAIT + Duration::from_secs(2),
            "waited the bounded time: {waited:?}"
        );
        assert_eq!(
            headers.get("retry-after").map(|v| v.to_str().unwrap()),
            Some("1")
        );
        assert_eq!(
            headers.get("cache-control").map(|v| v.to_str().unwrap()),
            Some("no-store")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "error": "busy, retry shortly" })
        );
    }
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false),
        "nothing was recorded"
    );
    let (status, _, _) = ping_raw(&public, "GET", "not-a-real-token").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A place freed while a ping waits serves it.
    let waiting = tokio::spawn({
        let public = public.clone();
        let token = token.clone();
        async move { ping_raw(&public, "POST", &token).await.0 }
    });
    tokio::time::sleep(PING_SLOT_WAIT / 4).await;
    drop(held);
    assert_eq!(waiting.await.unwrap(), StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));
}

/// A cron fleet pinging at the same second: twenty heartbeats at once, with
/// the default places, are all recorded (they wait their turn).
#[sqlx::test]
async fn simultaneous_pings_of_many_heartbeats_all_succeed(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let mut ids = Vec::new();
    let mut pings = Vec::new();
    for n in 0..20 {
        let (id, token) = seed_heartbeat(&pool, project, &format!("job {n}"), 3600, 60, 0).await;
        ids.push(id);
        let public = public.clone();
        pings.push(tokio::spawn(async move {
            ping_raw(&public, "GET", &token).await.0
        }));
    }
    for ping in pings {
        assert_eq!(ping.await.unwrap(), StatusCode::OK);
    }
    for id in ids {
        assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));
    }
}

/// A ping holds its place for as long as it runs, database work included: with
/// one place, a ping stuck on the heartbeat's row lock keeps it, and a second
/// ping meanwhile is turned away instead of queueing for a connection. Once
/// the lock is released, the first ping completes.
#[sqlx::test]
async fn a_ping_holds_its_place_while_it_waits_for_the_database(pool: sqlx::PgPool) {
    let slots = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let public = nami_engine::core::router::build_public_router_with_ping_slots(
        AppState::for_test(pool.clone()),
        slots,
    );
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let (_, other) = seed_heartbeat(&pool, project, "Other job", 3600, 60, 0).await;

    let mut lock = pool.begin().await.unwrap();
    sqlx::query("select 1 from heartbeats where id = $1 for update")
        .bind(id)
        .execute(&mut *lock)
        .await
        .unwrap();
    let first = tokio::spawn({
        let public = public.clone();
        let token = token.clone();
        async move { ping_raw(&public, "POST", &token).await.0 }
    });
    wait_for_a_lock_wait(&pool, "from heartbeats where token = $1 for update").await;

    let (status, _, _) = ping_raw(&public, "POST", &other).await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the only place is taken"
    );

    lock.rollback().await.unwrap();
    assert_eq!(first.await.unwrap(), StatusCode::OK);
    let (status, _, _) = ping_raw(&public, "POST", &other).await;
    assert_eq!(status, StatusCode::OK, "the place was given back");
}

// ---------------------------------------------------------------------------
// Failure and exit-code pings, ping bodies and the per-heartbeat rate limit
// (NAMI-UP-20).
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn a_fail_ping_takes_the_heartbeat_down_at_once_and_a_success_ping_recovers_it(
    pool: sqlx::PgPool,
) {
    let public = common::test_public_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, _, body) =
        ping_with(&public, &format!("/api/v1/heartbeat/{token}/fail"), vec![]).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(heartbeat_state(&pool, id).await, ("down".to_string(), true));
    // incidents_of: (status, title, cause), oldest first.
    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].0, "open");
    assert_eq!(incidents[0].1, "Nightly backup reported a failure");
    assert_eq!(
        incidents[0].2.as_deref(),
        Some("Heartbeat reported failure")
    );
    // The page goes out from the incident's state: let it leave before the
    // recovery resolves the incident.
    settled_mail(&recipient, 1).await;

    let (status, _, _) = ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert_eq!(incidents_of(&pool, id).await[0].0, "resolved");
    let mail = settled_mail(&recipient, 2).await;
    assert_eq!(mail[0].0, "[Nami] [high] Nightly backup reported a failure");
    assert!(
        // Mailpit hands the text part back with CRLF line endings.
        mail[0]
            .1
            .replace("\r\n", "\n")
            .contains("Cause: Heartbeat reported failure\n"),
        "{}",
        mail[0].1
    );
    assert_eq!(mail[1].0, "[Nami] Resolved: Nightly backup reported a failure");
}

#[sqlx::test]
async fn a_non_zero_exit_code_is_a_failure_naming_the_code(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, _, body) =
        ping_with(&public, &format!("/api/v1/heartbeat/{token}/137"), vec![]).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(heartbeat_state(&pool, id).await.0, "down");
    let cause = Some("Heartbeat reported failure (exit code 137)".to_string());
    assert_eq!(incidents_of(&pool, id).await[0].2, cause);

    let first_ping = last_ping_at(&pool, id)
        .await
        .expect("the failure counts as a ping");

    // A second failure while down: still one incident, still naming 137, and
    // the ping still counts (last_ping_at moves).
    tokio::time::sleep(Duration::from_millis(5)).await;
    let (status, _, _) = ping_with(&public, &format!("/api/v1/heartbeat/{token}/2"), vec![]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(last_ping_at(&pool, id).await.unwrap() > first_ping);
    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0].0, "open");
    assert_eq!(incidents[0].2, cause);
}

#[sqlx::test]
async fn a_signal_that_is_neither_fail_nor_an_integer_is_400(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    for suffix in ["FAIL", "1.5", "2147483648", "ok", "fail%20"] {
        let (status, _, body) = ping_with(
            &public,
            &format!("/api/v1/heartbeat/{token}/{suffix}"),
            vec![],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{suffix}");
        assert_eq!(
            body["error"], "the last path segment must be fail or an exit code",
            "{suffix}"
        );
    }
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false)
    );
}

#[sqlx::test]
async fn fail_and_exit_code_routes_answer_404_for_unknown_or_malformed_tokens(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let unknown = "f".repeat(64);
    let upper = "F".repeat(64);
    for path in [
        format!("/api/v1/heartbeat/{unknown}/fail"),
        format!("/api/v1/heartbeat/{unknown}/1"),
        "/api/v1/heartbeat/not-a-token/fail".to_string(),
        format!("/api/v1/heartbeat/{upper}/0"),
    ] {
        let (status, _, body) = ping_with(&public, &path, vec![]).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body, json!({ "error": "not found" }), "{path}");
    }
}

#[sqlx::test]
async fn fail_pings_during_maintenance_or_pause_change_nothing_but_last_ping_at(
    pool: sqlx::PgPool,
) {
    let public = common::test_public_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (a, token_a) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let (b, token_b) = seed_heartbeat(&pool, project, "B", 3600, 60, 0).await;
    sqlx::query("update heartbeats set current_status = 'up' where project_id = $1")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update heartbeats set paused = true where id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let window: Uuid = sqlx::query_scalar(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, null, 'maintenance', 'once', now() - interval '1 hour', now() + interval '1 hour')
         returning id",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();

    for token in [&token_a, &token_b] {
        let (status, _, _) =
            ping_with(&public, &format!("/api/v1/heartbeat/{token}/fail"), vec![]).await;
        assert_eq!(status, StatusCode::OK);
    }
    for id in [a, b] {
        assert_eq!(heartbeat_state(&pool, id).await, ("up".to_string(), true));
        assert!(incidents_of(&pool, id).await.is_empty());
    }
    assert!(settled_mail(&recipient, 0).await.is_empty());

    sqlx::query("delete from maintenance_windows where id = $1")
        .bind(window)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update heartbeats set paused = false where id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    for token in [&token_a, &token_b] {
        let (status, _, _) =
            ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
        assert_eq!(status, StatusCode::OK);
    }
    for id in [a, b] {
        assert_eq!(heartbeat_state(&pool, id).await.0, "up");
        assert!(incidents_of(&pool, id).await.is_empty());
    }
}

#[sqlx::test]
async fn only_a_declared_body_over_one_mebibyte_is_413(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let path = format!("/api/v1/heartbeat/{token}/fail");
    let too_large = json!({ "error": "ping body too large (max 1 MiB)" });

    let (status, _, body) = ping_with(&public, &path, vec![b'x'; MAX_PING_BODY_BYTES + 1]).await;
    assert_eq!(
        (status, body),
        (StatusCode::PAYLOAD_TOO_LARGE, too_large.clone())
    );
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false)
    );

    // A declared length over the cap is refused before anything is read.
    let request = Request::builder()
        .method("POST")
        .uri(&path)
        .header("content-length", (MAX_PING_BODY_BYTES + 1).to_string())
        .body(Body::empty())
        .unwrap();
    let response = public.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        heartbeat_state(&pool, id).await,
        ("pending".to_string(), false)
    );

    // The same size, chunked: no length is declared, so the signal counts;
    // the first MiB is read and the output capped as usual.
    let chunks = [vec![b'x'; 600 * 1024], vec![b'x'; 600 * 1024]]
        .map(|chunk| Ok::<_, std::io::Error>(axum::body::Bytes::from(chunk)));
    let streamed = Body::from_stream(tokio_stream::iter(chunks));
    let (status, _, _) = ping_body(&public, &path, streamed).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_state(&pool, id).await, ("down".to_string(), true));
    assert_eq!(
        details_of(&pool, id).await,
        vec![Some(format!(
            "{}{TRUNCATED_MARKER}",
            "x".repeat(MAX_OUTPUT_BYTES)
        ))]
    );
}

/// A body that fails the test if anyone reads it.
fn unreadable_body() -> Body {
    use tokio_stream::StreamExt;
    Body::from_stream(tokio_stream::iter([()]).map(
        |()| -> Result<axum::body::Bytes, std::io::Error> {
            panic!("a ping body was read that should not have been")
        },
    ))
}

#[sqlx::test]
async fn no_body_is_read_for_an_unknown_token(pool: sqlx::PgPool) {
    let slots = Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT));
    let public = nami_engine::core::router::build_public_router_with_ping_slots(
        AppState::for_test(pool.clone()),
        slots.clone(),
    );
    let unknown = format!("/api/v1/heartbeat/{}/fail", "e".repeat(64));

    let (status, _, body) = ping_body(&public, &unknown, unreadable_body()).await;
    assert_eq!(
        (status, body),
        (StatusCode::NOT_FOUND, json!({ "error": "not found" }))
    );

    // With every ping slot held, an unknown well-formed token is 503, not
    // 404: the lookup still needs a slot.
    let _held = slots
        .clone()
        .acquire_many_owned(MAX_PINGS_IN_FLIGHT as u32)
        .await
        .unwrap();
    let (status, headers, _) = ping_body(&public, &unknown, unreadable_body()).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers["retry-after"], "1");
}

#[sqlx::test]
async fn one_body_per_heartbeat_is_read_at_a_time(pool: sqlx::PgPool) {
    // One database place: the slow body must not hold it while it arrives,
    // or B's ping below would be 503.
    let public = nami_engine::core::router::build_public_router_with_ping_slots(
        AppState::for_test(pool.clone()),
        Arc::new(Semaphore::new(1)),
    );
    let project = seed_project(&pool).await;
    let (a, token_a) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let (_, token_b) = seed_heartbeat(&pool, project, "B", 3600, 60, 0).await;
    let path_a = format!("/api/v1/heartbeat/{token_a}/fail");

    // A body that never finishes: one chunk, then the sender stays open.
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    sender
        .send(Ok(axum::body::Bytes::from_static(b"still running")))
        .await
        .unwrap();
    let slow = tokio::spawn({
        let public = public.clone();
        let path_a = path_a.clone();
        let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver));
        async move { ping_body(&public, &path_a, body).await }
    });
    // The first chunk was taken: the slow ping is reading its body.
    let deadline = Instant::now() + Duration::from_secs(5);
    while sender.capacity() == 0 {
        assert!(Instant::now() < deadline, "the slow body was never read");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // A second body for A finds A's reading place taken: the failure is
    // recorded all the same, with the busy marker in place of its output.
    let (status, _, body) = ping_with(&public, &path_a, b"again".to_vec()).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(heartbeat_state(&pool, a).await, ("down".to_string(), true));
    assert_eq!(
        details_of(&pool, a).await,
        vec![Some(OUTPUT_BUSY_MARKER.to_string())]
    );
    let (status, _, _) = ping_with(
        &public,
        &format!("/api/v1/heartbeat/{token_b}/fail"),
        b"x".to_vec(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "other heartbeats are unaffected, and no database place is held during the body read"
    );

    // The slow body runs out of time: the ping still counts.
    tokio::time::pause();
    tokio::time::advance(PING_BODY_TIMEOUT + Duration::from_secs(1)).await;
    let (status, _, body) = slow.await.unwrap();
    tokio::time::resume();
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    drop(sender);
}

/// A body that never finishes, sent to `path`: returns its sender (keep it to
/// keep the body open) and the ping's task, once the first chunk was taken
/// (the ping holds its reading places).
async fn hold_body(
    public: &Router,
    path: &str,
) -> (
    tokio::sync::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>,
    tokio::task::JoinHandle<(StatusCode, HeaderMap, Value)>,
) {
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(1);
    sender
        .send(Ok(axum::body::Bytes::from_static(b"still running")))
        .await
        .unwrap();
    let task = tokio::spawn({
        let public = public.clone();
        let path = path.to_string();
        let body = Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(receiver));
        async move { ping_body(&public, &path, body).await }
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while sender.capacity() == 0 {
        assert!(Instant::now() < deadline, "the held body was never read");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (sender, task)
}

/// A failure whose body does not arrive in time is recorded without its
/// output: the heartbeat goes down and its incident opens with the marker.
#[sqlx::test]
async fn a_slow_body_never_loses_the_signal(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let (sender, slow) = hold_body(&public, &format!("/api/v1/heartbeat/{token}/fail")).await;

    tokio::time::pause();
    tokio::time::advance(PING_BODY_TIMEOUT + Duration::from_secs(1)).await;
    let (status, _, body) = slow.await.unwrap();
    tokio::time::resume();
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    drop(sender);
    assert_eq!(heartbeat_state(&pool, id).await, ("down".to_string(), true));
    assert_eq!(
        incidents_of(&pool, id).await[0].2.as_deref(),
        Some("Heartbeat reported failure")
    );
    assert_eq!(
        details_of(&pool, id).await,
        vec![Some(OUTPUT_LATE_MARKER.to_string())]
    );
}

/// With every body place of the endpoint held (two per project, so by eight
/// projects), another tenant's failure with output is still recorded: down,
/// an incident, the busy marker.
#[sqlx::test]
async fn held_body_places_never_lose_another_tenants_signal(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let mut held = Vec::new();
    for _ in 0..MAX_PING_BODIES_IN_FLIGHT / MAX_PING_BODIES_PER_PROJECT {
        let project = seed_project(&pool).await;
        for n in 0..MAX_PING_BODIES_PER_PROJECT {
            let (_, token) = seed_heartbeat(&pool, project, &format!("hog {n}"), 3600, 60, 0).await;
            held.push(hold_body(&public, &format!("/api/v1/heartbeat/{token}/0")).await);
        }
    }
    let victim_project = seed_project(&pool).await;
    let (victim, token) = seed_heartbeat(&pool, victim_project, "backup", 3600, 60, 0).await;
    let (status, _, body) = ping_with(
        &public,
        &format!("/api/v1/heartbeat/{token}/fail"),
        b"disk full".to_vec(),
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "ok": true })));
    assert_eq!(
        heartbeat_state(&pool, victim).await,
        ("down".to_string(), true)
    );
    assert_eq!(incidents_of(&pool, victim).await.len(), 1);
    assert_eq!(
        details_of(&pool, victim).await,
        vec![Some(OUTPUT_BUSY_MARKER.to_string())]
    );
    for (sender, task) in held {
        drop(sender);
        assert_eq!(task.await.unwrap().0, StatusCode::OK);
    }
}

/// One project reads at most MAX_PING_BODIES_PER_PROJECT bodies at once: its
/// next body gets the busy marker, while another project's body is read.
#[sqlx::test]
async fn body_reads_are_capped_per_project(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let mut held = Vec::new();
    for n in 0..MAX_PING_BODIES_PER_PROJECT {
        let (_, token) = seed_heartbeat(&pool, project, &format!("slow {n}"), 3600, 60, 0).await;
        held.push(hold_body(&public, &format!("/api/v1/heartbeat/{token}/0")).await);
    }
    let (third, token) = seed_heartbeat(&pool, project, "third", 3600, 60, 0).await;
    let (status, _, _) = ping_with(
        &public,
        &format!("/api/v1/heartbeat/{token}/fail"),
        b"disk full".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        details_of(&pool, third).await,
        vec![Some(OUTPUT_BUSY_MARKER.to_string())]
    );

    let other_project = seed_project(&pool).await;
    let (other, token) = seed_heartbeat(&pool, other_project, "other", 3600, 60, 0).await;
    let (status, _, _) = ping_with(
        &public,
        &format!("/api/v1/heartbeat/{token}/fail"),
        b"disk full".to_vec(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        details_of(&pool, other).await,
        vec![Some("disk full".to_string())]
    );
    drop(held);
}

#[sqlx::test]
async fn more_than_the_burst_of_pings_per_heartbeat_is_429(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    // A refill rate of ~0 makes the test independent of how fast pings run.
    let limits = Arc::new(TokenBuckets::new(0.000_1, PING_BURST));
    let public = nami_engine::core::router::build_public_router_with_ping_limits(
        state,
        Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT)),
        limits,
    );
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let (_, other) = seed_heartbeat(&pool, project, "B", 3600, 60, 0).await;
    for n in 0..10 {
        let path = if n % 2 == 0 {
            format!("/api/v1/heartbeat/{token}")
        } else {
            format!("/api/v1/heartbeat/{token}/fail")
        };
        assert_eq!(
            ping_with(&public, &path, vec![]).await.0,
            StatusCode::OK,
            "ping {n}"
        );
    }
    let (status, headers, body) =
        ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers["retry-after"], "1");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(body, json!({ "error": "too many requests" }));
    // Buckets are per heartbeat: another token is unaffected; an unknown token stays 404.
    assert_eq!(
        ping_with(&public, &format!("/api/v1/heartbeat/{other}"), vec![])
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        ping_with(
            &public,
            &format!("/api/v1/heartbeat/{}", "e".repeat(64)),
            vec![]
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// A heartbeat over its rate limit is turned away before its body is read.
#[sqlx::test]
async fn the_rate_limit_is_checked_before_any_body_is_read(pool: sqlx::PgPool) {
    let limits = Arc::new(TokenBuckets::new(0.000_1, 1.0));
    let public = nami_engine::core::router::build_public_router_with_ping_limits(
        AppState::for_test(pool.clone()),
        Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT)),
        limits,
    );
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project, "A", 3600, 60, 0).await;
    let path = format!("/api/v1/heartbeat/{token}/fail");

    // The only token of the bucket.
    assert_eq!(ping_with(&public, &path, vec![]).await.0, StatusCode::OK);

    let (status, headers, body) = ping_body(&public, &path, unreadable_body()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(headers["retry-after"], "1");
    assert_eq!(body, json!({ "error": "too many requests" }));
}

async fn create_group(app: &Router, cookie: &axum::http::HeaderValue, project: Uuid) -> String {
    let (status, group) = call(
        app,
        "POST",
        &format!("/projects/{project}/monitor-groups"),
        Some(cookie),
        Some(json!({ "name": "Jobs" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{group}");
    group["id"].as_str().unwrap().to_string()
}

async fn stored_group(pool: &sqlx::PgPool, heartbeat: &str) -> Option<Uuid> {
    sqlx::query_scalar("select group_id from heartbeats where id = $1::uuid")
        .bind(heartbeat)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn heartbeats_join_their_projects_groups(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let group = create_group(&app, &member, project).await;
    let foreign = create_group(&app, &other_member, other).await;
    let collection = format!("/projects/{project}/heartbeats");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Backup", "period_seconds": 86400, "group_id": group })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["group_id"], group);
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("{collection}/{id}");

    let (_, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    assert_eq!(list[0]["group_id"], group, "viewers see the group too");

    // Omitted keeps, null clears, a value sets.
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Backup 2" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["group_id"], group);
    let (_, cleared) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "group_id": null })),
    )
    .await;
    assert!(cleared["group_id"].is_null(), "{cleared}");
    let (_, set) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "group_id": group })),
    )
    .await;
    assert_eq!(set["group_id"], group);

    // Another project's group, and an unknown one: 404, row unchanged.
    for bad in [foreign.clone(), Uuid::new_v4().to_string()] {
        let (status, body) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ "group_id": bad, "name": "changed" })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(!body.to_string().contains(&foreign), "{body}");
        assert_eq!(stored_group(&pool, &id).await, Some(group.parse().unwrap()));
        let (_, now) = call(&app, "GET", &item, Some(&member), None).await;
        assert_eq!(now["name"], "Backup 2", "{now}");
        // Same on create: nothing inserted.
        let (status, body) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(json!({ "name": "Other", "period_seconds": 60, "group_id": bad })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        let count: i64 =
            sqlx::query_scalar("select count(*) from heartbeats where project_id = $1")
                .bind(project)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);
    }
}

#[sqlx::test]
async fn the_database_refuses_a_group_of_another_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let foreign = create_group(&app, &other_member, other).await;
    let heartbeat: Uuid = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds)
         values ($1, 'x', repeat('a', 64), 60, 60) returning id",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();
    let err = sqlx::query("update heartbeats set group_id = $2::uuid where id = $1")
        .bind(heartbeat)
        .bind(&foreign)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(
        err.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23503")
    );
}

// ---------------------------------------------------------------------------
// Flap control (Phase 3 final review): a job that fails and succeeds in turn
// keeps one incident and costs each member at most one DOWN + recovery pair
// per window.
// ---------------------------------------------------------------------------

/// A public router whose rate limit never gets in the way.
fn unthrottled_public(state: AppState) -> Router {
    nami_engine::core::router::build_public_router_with_ping_limits(
        state,
        Arc::new(Semaphore::new(MAX_PINGS_IN_FLIGHT)),
        Arc::new(TokenBuckets::new(1000.0, 1000.0)),
    )
}

#[sqlx::test]
async fn a_flapping_heartbeat_keeps_one_incident_and_sends_one_pair_of_emails(pool: sqlx::PgPool) {
    let public = unthrottled_public(AppState::for_test(pool.clone()));
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Flaky job", 3600, 60, 0).await;

    for n in 0..10 {
        let (status, _, _) = ping_with(
            &public,
            &format!("/api/v1/heartbeat/{token}/fail"),
            format!("boom {n}").into_bytes(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        if n == 0 {
            // The page goes out from the incident's state: let the first one
            // leave before the recovery resolves the incident. The later
            // failures fall inside the paging gap (R31).
            settled_mail(&recipient, 1).await;
        }
        let (status, _, _) =
            ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![]).await;
        assert_eq!(status, StatusCode::OK);
    }

    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents.len(), 1, "{incidents:?}");
    assert_eq!(incidents[0].0, "resolved");
    assert_eq!(
        details_of(&pool, id).await,
        vec![Some("boom 9".to_string())]
    );
    let mail = settled_mail(&recipient, 2).await;
    let subjects: Vec<&str> = mail.iter().map(|(subject, _)| subject.as_str()).collect();
    assert_eq!(
        subjects,
        vec![
            "[Nami] [high] Flaky job reported a failure",
            "[Nami] Resolved: Flaky job reported a failure"
        ]
    );
}

/// A held heartbeat page waits for a project-wide maintenance window to end
/// (ruling on NAMI-INC-10; Phase 3's held-DOWN case under escalation).
#[sqlx::test]
async fn a_held_heartbeat_page_waits_for_maintenance_to_end(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let public = unthrottled_public(state.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Flaky job", 3600, 60, 0).await;
    for (n, path) in ["fail", "0", "fail"].into_iter().enumerate() {
        let (status, _, _) =
            ping_with(&public, &format!("/api/v1/heartbeat/{token}/{path}"), vec![]).await;
        assert_eq!(status, StatusCode::OK);
        if n == 0 {
            // Let the first page leave before the recovery resolves it; the
            // second failure falls inside the paging gap (R31).
            settled_mail(&recipient, 1).await;
        }
    }
    let window: Uuid = sqlx::query_scalar(
        "insert into maintenance_windows (project_id, title, kind, starts_at, ends_at)
         values ($1, 'maintenance', 'once', now() - interval '5 minutes', now() + interval '1 hour')
         returning id",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();
    let held: Uuid =
        sqlx::query_scalar("select id from incidents where heartbeat_id = $1 and status = 'open'")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    common::make_escalation_due(&pool, held).await;
    common::run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&recipient, 2).await.len(), 2, "held by the window");

    sqlx::query("delete from maintenance_windows where id = $1")
        .bind(window)
        .execute(&pool)
        .await
        .unwrap();
    common::make_escalation_due(&pool, held).await;
    common::run_incident_ticks(&state).await;
    let mail = settled_mail(&recipient, 3).await;
    assert_eq!(mail.len(), 3);
    assert_eq!(mail[2].0, "[Nami] [high] Flaky job reported a failure");
}

/// A failure soon after a recovery reopens the same incident (same id, the
/// latest cause and output); one after the reopen window opens a new one,
/// and alerts.
#[sqlx::test]
async fn a_failure_reopens_a_recent_incident_and_opens_a_new_one_later(pool: sqlx::PgPool) {
    let public = unthrottled_public(AppState::for_test(pool.clone()));
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;
    let fail = |code: i32| format!("/api/v1/heartbeat/{token}/{code}");
    let ids = |pool: sqlx::PgPool| async move {
        sqlx::query_scalar::<_, Uuid>(
            "select id from incidents where heartbeat_id = $1 order by started_at",
        )
        .bind(id)
        .fetch_all(&pool)
        .await
        .unwrap()
    };

    assert_eq!(
        ping_with(&public, &fail(2), b"first".to_vec()).await.0,
        StatusCode::OK
    );
    // The page goes out from the incident's state: let it leave before the
    // recovery resolves the incident.
    settled_mail(&recipient, 1).await;
    assert_eq!(ping_with(&public, &fail(0), vec![]).await.0, StatusCode::OK);
    let first = ids(pool.clone()).await;
    assert_eq!(
        ping_with(&public, &fail(3), b"second".to_vec()).await.0,
        StatusCode::OK
    );
    assert_eq!(
        ids(pool.clone()).await,
        first,
        "the same incident, reopened"
    );
    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents[0].0, "open");
    assert_eq!(
        incidents[0].2.as_deref(),
        Some("Heartbeat reported failure (exit code 3)")
    );
    assert_eq!(
        details_of(&pool, id).await,
        vec![Some("second".to_string())]
    );
    let resolved_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("select resolved_at from incidents where id = $1")
            .bind(first[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(resolved_at, None);

    // Recover, then let six minutes pass (moved back in the database, the
    // heartbeat's pages too: the paging gap has passed as well).
    assert_eq!(ping_with(&public, &fail(0), vec![]).await.0, StatusCode::OK);
    sqlx::query(
        "update incidents set resolved_at = resolved_at - interval '6 minutes',
                              started_at = started_at - interval '7 minutes'
         where heartbeat_id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "update heartbeats set last_paged_at = last_paged_at - interval '7 minutes' where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let later = unthrottled_public(AppState::for_test(pool.clone()));
    let before = settled_mail(&recipient, 2).await.len();
    assert_eq!(
        ping_with(&later, &fail(4), b"third".to_vec()).await.0,
        StatusCode::OK
    );
    let now = ids(pool.clone()).await;
    assert_eq!(now.len(), 2, "a new incident");
    assert_eq!(now[0], first[0]);
    assert_eq!(
        details_of(&pool, id).await,
        vec![Some("second".to_string()), Some("third".to_string())]
    );
    let mail = settled_mail(&recipient, before + 1).await;
    assert_eq!(mail.len(), before + 1);
    assert_eq!(
        mail.last().unwrap().0,
        "[Nami] [high] Nightly backup reported a failure"
    );
}

/// The sweep reopens a recent incident too: a job that recovered and then
/// missed its deadline keeps one incident.
#[sqlx::test]
async fn a_missed_deadline_soon_after_a_recovery_reopens_the_incident(pool: sqlx::PgPool) {
    let public = unthrottled_public(AppState::for_test(pool.clone()));
    let project = seed_project(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 600).await;
    assert_eq!(
        ping_with(&public, &format!("/api/v1/heartbeat/{token}/fail"), vec![])
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        ping_with(&public, &format!("/api/v1/heartbeat/{token}/0"), vec![])
            .await
            .0,
        StatusCode::OK
    );
    sqlx::query("update heartbeats set last_ping_at = now() - interval '2 minutes' where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let transitions = sweep_overdue(&pool, SWEEP_BATCH).await.unwrap();
    assert_eq!(transitions.len(), 1);
    let incidents = incidents_of(&pool, id).await;
    assert_eq!(incidents.len(), 1, "{incidents:?}");
    assert_eq!(incidents[0].0, "open");
    assert_eq!(incidents[0].1, "Nightly backup missed its heartbeat");
    assert_eq!(details_of(&pool, id).await, vec![None]);
}

/// A heartbeat stored with a stray tag character in its name can still be
/// paused; a new name with one is refused.
#[sqlx::test]
async fn a_heartbeat_with_a_stored_stray_tag_character_can_be_paused(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (id, _) = seed_heartbeat(&pool, project, "legacy\u{E0041}", 3600, 60, 0).await;
    let item = format!("/projects/{project}/heartbeats/{id}");
    let (status, body) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["paused"], true);
    let (status, _) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "stray\u{E0041}" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
