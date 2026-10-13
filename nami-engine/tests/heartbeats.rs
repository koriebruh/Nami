// Requires `docker compose up -d postgres mailpit`.
mod common;

use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use chrono::Utc;
use common::{
    LogCapture, add_member, call, seed_project, seed_user, settled_mail, silent_server, test_app,
    unique_email, user_with_role,
};
use nami_engine::{
    email::Mailer,
    models::incident,
    state::AppState,
    uptime::{
        alerts::{Subject, TransitionKind},
        heartbeat::{PingOutcome, record_ping, sweep_overdue},
        worker::{Notifications, run_forever, sweep_heartbeats, sweep_pass},
    },
};
use serde_json::{Value, json};
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
    let token = Uuid::new_v4().simple().to_string();
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
    let app = test_app(pool.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 3600, 60, 0).await;

    let (status, body) = call(
        &app,
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
        &app,
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
    let app = test_app(pool.clone());
    let (status, body) = call(
        &app,
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
    assert_eq!(mail[0].0, "[Nami] Nightly backup is DOWN");
    let link = format!(
        "{}/projects/{project}/heartbeats/{id}",
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
    let app = common::app(state.clone());
    let (project, recipient) = project_with_recipient(&pool).await;
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    sweep_heartbeats(&state).await.unwrap();

    let (status, _) = call(
        &app,
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
            "[Nami] Nightly backup is DOWN",
            "[Nami] Nightly backup is back UP"
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
    let app = test_app(pool.clone());
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
        &app,
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
    assert_eq!(list[0]["heartbeat_id"], id.to_string());
    assert_eq!(list[0]["monitor_id"], Value::Null);
    assert_eq!(list[0]["source_name"], "Nightly backup");
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
    let app = test_app(pool.clone());
    let lookups = || {
        log.lines()
            .iter()
            .filter(|line| line.contains("from heartbeats where token"))
            .count()
    };

    // Control: a well-formed token nobody holds is looked up, and the log shows it.
    let unknown = "a".repeat(64);
    let (status, _) = call(
        &app,
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
    for token in [
        "abc%00def",
        "%0A",
        "x%0D%0Ay",
        "a%20b",
        "%E2%82%AC",
        "%ff",
        too_long.as_str(),
    ] {
        for method in ["GET", "POST"] {
            let uri = format!("/api/v1/heartbeat/{token}");
            let (status, body) = call(&app, method, &uri, None, None).await;
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

/// Whoever pings a token it holds learns nothing about the heartbeat: the same
/// 200 and the same bytes whether it is pending, up, down or paused, and no
/// session or cookie is ever created.
#[sqlx::test]
async fn the_ping_reply_is_the_same_whatever_state_the_heartbeat_is_in(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
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
            let reply = ping_raw(&app, method, &token).await;
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
        call(&app, "POST", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
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
    sqlx::query("update heartbeats set last_ping_at = now() - interval '1 hour' where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(
        call(&app, "POST", &ping, None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(settled_mail(&recipient, 2).await.len(), 2);
    // Paused pings, edits and the detail view too.
    let patch = Some(json!({ "paused": true }));
    assert_eq!(
        call(&app, "PATCH", &item, Some(&member), patch).await.0,
        StatusCode::OK
    );
    assert_eq!(call(&app, "GET", &ping, None, None).await.0, StatusCode::OK);
    assert_eq!(
        call(&app, "GET", &item, Some(&member), None).await.0,
        StatusCode::OK
    );

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

/// A ping racing a sweep (`record_ping` takes the heartbeat's row lock first):
/// when the sweep has flipped the heartbeat but not committed, the ping waits,
/// then sees it down and recovers it, resolving the incident the sweep opened.
/// Without the lock it would read the old state, write `up` over the sweep's
/// `down` and leave the incident open.
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
    tokio::time::sleep(Duration::from_millis(300)).await;
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

/// The other order: a ping in progress (written, not committed) makes the
/// sweep wait; once it commits the sweep re-checks the heartbeat, finds a
/// fresh ping and leaves it alone.
#[sqlx::test]
async fn a_sweep_waits_for_a_ping_in_progress_and_then_finds_the_heartbeat_fresh(
    pool: sqlx::PgPool,
) {
    let project = seed_project(&pool).await;
    let (id, _) = seed_heartbeat(&pool, project, "Nightly backup", 30, 0, 120).await;
    let mut ping = pool.begin().await.unwrap();
    sqlx::query("update heartbeats set last_ping_at = now(), current_status = 'up' where id = $1")
        .bind(id)
        .execute(&mut *ping)
        .await
        .unwrap();

    let sweeping = tokio::spawn({
        let pool = pool.clone();
        async move { sweep_overdue(&pool).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!sweeping.is_finished(), "the sweep must wait for the ping");
    ping.commit().await.unwrap();

    assert_eq!(sweeping.await.unwrap().unwrap(), vec![]);
    assert_eq!(heartbeat_state(&pool, id).await.0, "up");
    assert!(incidents_of(&pool, id).await.is_empty());
}

/// Sweeps started together (overlapping ticks, or several engine instances):
/// all of them wait on the rows' locks while another transaction holds them,
/// and each heartbeat still goes down, and alerts, exactly once.
#[sqlx::test]
async fn sweeps_running_at_the_same_time_flip_each_heartbeat_once(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(
            seed_heartbeat(&pool, project, &format!("hb{i}"), 30, 0, 120)
                .await
                .0,
        );
    }
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("select id from heartbeats where project_id = $1 for update")
        .bind(project)
        .fetch_all(&mut *holder)
        .await
        .unwrap();
    let sweeps: Vec<_> = (0..3)
        .map(|_| {
            let pool = pool.clone();
            tokio::spawn(async move { sweep_overdue(&pool).await })
        })
        .collect();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        sweeps.iter().all(|sweep| !sweep.is_finished()),
        "the sweeps wait for the held rows"
    );
    holder.commit().await.unwrap();

    let mut transitions = Vec::new();
    for sweep in sweeps {
        transitions.extend(sweep.await.unwrap().unwrap());
    }

    assert_eq!(transitions.len(), 6, "{transitions:?}");
    for id in ids {
        assert_eq!(heartbeat_state(&pool, id).await.0, "down");
        assert_eq!(incidents_of(&pool, id).await.len(), 1);
        assert_eq!(
            transitions
                .iter()
                .filter(|t| t.subject == Subject::Heartbeat(id))
                .count(),
            1
        );
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

/// The deadline is computed in float8: a heartbeat with the largest period and
/// grace the columns hold must not make the whole sweep fail (as an int4 sum
/// would), or no other heartbeat would ever be swept again.
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
        vec!["[Nami] Nightly backup is DOWN"]
    );
}

/// A ping only ever touches the heartbeat that holds its token.
#[sqlx::test]
async fn a_ping_touches_only_the_heartbeat_that_holds_the_token(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let (first_project, other_project) = (seed_project(&pool).await, seed_project(&pool).await);
    let (pinged, token) = seed_heartbeat(&pool, first_project, "pinged", 3600, 60, 0).await;
    let (same_project, _) = seed_heartbeat(&pool, first_project, "same project", 3600, 60, 0).await;
    let (other, _) = seed_heartbeat(&pool, other_project, "other project", 3600, 60, 0).await;

    assert_eq!(
        call(
            &app,
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

/// A heartbeat has at most one open incident, and can open a new one once the
/// last is resolved (`incidents_one_open_per_heartbeat`).
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
            &app,
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
    let (id, token) = seed_heartbeat(&pool, project, "Nightly backup", 60, 0, 0).await;
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
        (status, detail["token"].as_str()),
        (StatusCode::OK, Some(token.as_str()))
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
