// Requires `docker compose up -d postgres mailpit clickhouse`.
use crate::common;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use chrono::{Duration, Utc};
use common::{
    add_member, call, public_app, seed_project, seed_user, state_with_clickhouse, stored_mail_for,
    test_public_app, unique_email, user_with_role,
};
use nami_engine::{
    core::state::AppState,
    uptime::{
        heartbeat::pings::{HeartbeatPingRow, insert_ping},
        heartbeat::{MAX_OUTPUT_BYTES, PingKind, SUCCESS_OUTPUT_MAX, TRUNCATED_MARKER},
    },
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// POST `body` to a public ping path; returns the status and JSON reply.
async fn ping(public: &Router, path: &str, body: &[u8]) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_vec()))
        .unwrap();
    let response = public.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Creates a heartbeat through the API as `cookie`. Returns (id, token).
async fn create_heartbeat(
    app: &Router,
    project: Uuid,
    cookie: &axum::http::HeaderValue,
) -> (Uuid, String) {
    let (status, created) = call(
        app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(cookie),
        Some(json!({ "name": "Nightly backup", "period_seconds": 86400 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    (
        created["id"].as_str().unwrap().parse().unwrap(),
        created["token"].as_str().unwrap().to_string(),
    )
}

#[sqlx::test]
async fn every_ping_is_recorded_with_kind_exit_code_and_sanitized_output(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = common::app(state.clone());
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (id, token) = create_heartbeat(&app, project, &member).await;
    let base = format!("/api/v1/heartbeat/{token}");

    let big_fail = vec![b'x'; 11 * 1024];
    let sent: [(String, &[u8]); 4] = [
        (base.clone(), b"started"),
        (format!("{base}/0"), b""),
        (format!("{base}/2"), b"err\r\x1b[31mred"),
        (format!("{base}/fail"), &big_fail),
    ];
    for (path, body) in &sent {
        let (status, reply) = ping(&public, path, body).await;
        assert_eq!(status, StatusCode::OK, "{path}: {reply}");
        // Newest-first ordering needs distinct millisecond timestamps.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let (status, rows) = call(
        &app,
        "GET",
        &format!("/projects/{project}/heartbeats/{id}/pings"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    let rows = rows.as_array().unwrap();
    let summary: Vec<(&str, Option<i64>, String)> = rows
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap(),
                r["exit_code"].as_i64(),
                r["output"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "fail",
                None,
                format!("{}{TRUNCATED_MARKER}", "x".repeat(MAX_OUTPUT_BYTES))
            ),
            ("fail", Some(2), "err[31mred".to_string()),
            ("success", Some(0), String::new()),
            ("success", None, "started".to_string()),
        ]
    );
    let mut keys: Vec<&String> = rows[0].as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "exit_code",
            "heartbeat_id",
            "kind",
            "output",
            "project_id",
            "received_at"
        ]
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn success_output_is_kept_to_one_kib_and_failure_output_to_ten(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = common::app(state.clone());
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (id, token) = create_heartbeat(&app, project, &member).await;
    let base = format!("/api/v1/heartbeat/{token}");
    let body = vec![b'y'; 5 * 1024];

    // Success kinds: a plain ping and exit code 0.
    for path in [base.clone(), format!("{base}/0")] {
        assert_eq!(ping(&public, &path, &body).await.0, StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    // Failure kinds: /fail and a non-zero exit code.
    for path in [format!("{base}/fail"), format!("{base}/7")] {
        assert_eq!(ping(&public, &path, &body).await.0, StatusCode::OK);
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let rows = nami_engine::uptime::heartbeat::pings::recent_pings(&ch, project, id, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        match row.kind {
            PingKind::Success => assert_eq!(
                row.output,
                format!("{}{TRUNCATED_MARKER}", "y".repeat(SUCCESS_OUTPUT_MAX)),
                "{:?}",
                row.exit_code
            ),
            PingKind::Fail => assert_eq!(row.output, "y".repeat(5 * 1024), "{:?}", row.exit_code),
        }
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_output_of_the_ping_that_opens_the_incident_is_attached_to_it(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = common::app(state.clone());
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let recipient = unique_email("oncall");
    let oncall = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, oncall, "viewer").await;
    let (_, token) = create_heartbeat(&app, project, &member).await;
    let base = format!("/api/v1/heartbeat/{token}");
    let list = format!("/projects/{project}/incidents");

    let output = "<script>alert(1)</script>\nbackup failed";
    assert_eq!(
        ping(&public, &format!("{base}/1"), output.as_bytes())
            .await
            .0,
        StatusCode::OK
    );
    let (_, incidents) = call(&app, "GET", &list, Some(&member), None).await;
    let incidents = incidents["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1, "{incidents:?}");
    assert_eq!(incidents[0]["details"], output);
    assert_eq!(
        incidents[0]["cause"],
        "Heartbeat reported failure (exit code 1)"
    );

    // A second failure leaves the incident's details alone.
    assert_eq!(
        ping(&public, &format!("{base}/fail"), b"other output")
            .await
            .0,
        StatusCode::OK
    );
    let (_, incidents) = call(&app, "GET", &list, Some(&member), None).await;
    assert_eq!(incidents["incidents"][0]["details"], output);

    // The DOWN email quotes the output.
    let mut mail = vec![];
    for _ in 0..40 {
        mail = stored_mail_for(&recipient).await;
        if !mail.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert_eq!(mail.len(), 1);
    assert!(
        mail[0]
            .text
            .contains("Output:\n> <script>alert(1)</script>\n> backup failed\n"),
        "{:?}",
        mail[0].text
    );

    // Recovery resolves; a new failure with an empty body soon after reopens
    // the same incident (flap control) and clears its details.
    assert_eq!(
        ping(&public, &format!("{base}/0"), b"").await.0,
        StatusCode::OK
    );
    assert_eq!(
        ping(&public, &format!("{base}/fail"), b"").await.0,
        StatusCode::OK
    );
    let (_, incidents) = call(&app, "GET", &list, Some(&member), None).await;
    let incidents = incidents["incidents"].as_array().unwrap();
    assert_eq!(incidents.len(), 1);
    assert_eq!(incidents[0]["status"], "open");
    assert_eq!(incidents[0]["details"], Value::Null);
    assert_eq!(incidents[0]["resolved_at"], Value::Null);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_pings_list_checks_access_project_and_limit(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = common::app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let (id, _) = create_heartbeat(&app, project, &member).await;
    let (other_id, _) = create_heartbeat(&app, other, &other_member).await;
    let uri = format!("/projects/{project}/heartbeats/{id}/pings");

    assert_eq!(
        call(&app, "GET", &uri, Some(&outsider), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/projects/{project}/heartbeats/{other_id}/pings"),
            Some(&member),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("/projects/{project}/heartbeats/{}/pings", Uuid::new_v4()),
            Some(&member),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );

    let now = Utc::now();
    for n in 0..105 {
        insert_ping(
            &ch,
            &HeartbeatPingRow {
                heartbeat_id: id,
                project_id: project,
                received_at: now - Duration::seconds(n),
                kind: PingKind::Success,
                exit_code: None,
                output: String::new(),
            },
        )
        .await
        .unwrap();
    }
    let count = |query: &'static str| {
        let app = app.clone();
        let member = member.clone();
        let uri = uri.clone();
        async move {
            let (status, body) =
                call(&app, "GET", &format!("{uri}{query}"), Some(&member), None).await;
            (status, body.as_array().map_or(0, Vec::len))
        }
    };
    assert_eq!(count("?limit=0").await, (StatusCode::OK, 1));
    assert_eq!(count("?limit=1000").await, (StatusCode::OK, 100));
    assert_eq!(count("").await, (StatusCode::OK, 50));
    assert_eq!(count("?limit=abc").await.0, StatusCode::BAD_REQUEST);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_clickhouse_outage_never_fails_a_ping(pool: sqlx::PgPool) {
    // ClickHouse bound to a database that does not exist.
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let public = test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (id, token) = create_heartbeat(&app, project, &member).await;

    let (status, reply) = ping(&public, &format!("/api/v1/heartbeat/{token}/fail"), b"boom").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply, json!({ "ok": true }));
    let (current_status,): (String,) =
        sqlx::query_as("select current_status from heartbeats where id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(current_status, "down");
    let (open,): (i64,) = sqlx::query_as(
        "select count(*) from incidents where heartbeat_id = $1 and status = 'open'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open, 1);
}

#[sqlx::test]
async fn a_first_failure_with_the_maximum_output_opens_an_incident_with_all_of_it(
    pool: sqlx::PgPool,
) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = common::app(state.clone());
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, token) = create_heartbeat(&app, project, &member).await;

    let body = vec![b'z'; 11 * 1024];
    let (status, _) = ping(&public, &format!("/api/v1/heartbeat/{token}/fail"), &body).await;
    assert_eq!(status, StatusCode::OK);

    let (_, incidents) = call(
        &app,
        "GET",
        &format!("/projects/{project}/incidents"),
        Some(&member),
        None,
    )
    .await;
    let details = incidents["incidents"][0]["details"].as_str().unwrap();
    assert_eq!(details.len(), MAX_OUTPUT_BYTES + TRUNCATED_MARKER.len());
    assert_eq!(
        details,
        format!("{}{TRUNCATED_MARKER}", "z".repeat(MAX_OUTPUT_BYTES))
    );
    ch.drop_database().await.unwrap();
}
