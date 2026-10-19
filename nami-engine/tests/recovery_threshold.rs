// Requires `docker compose up -d postgres mailpit clickhouse`.
mod common;

use axum::http::StatusCode;
use common::{
    add_member, at, call, make_due, outcome, seed_http_monitor, seed_project, seed_user,
    settled_mail, state_with_clickhouse, test_app, unique_email, user_with_role,
};
use nami_engine::{
    uptime::monitor::model::Monitor,
    uptime::{
        alerts::TransitionKind,
        checker::CheckStatus,
        worker::{apply_outcome, run_due_checks},
    },
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

/// A monitor that goes down on its first failure and needs `recovery`
/// consecutive successes to come back.
async fn monitor_with_recovery(pool: &sqlx::PgPool, recovery: i32) -> Monitor {
    let project = seed_project(pool).await;
    let id = seed_http_monitor(pool, project, "Checkout API", "http://127.0.0.1:9/", 1).await;
    sqlx::query_as("update monitors set recovery_threshold = $2 where id = $1 returning *")
        .bind(id)
        .bind(recovery)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// (status, consecutive_failures, consecutive_successes)
async fn counters(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, i32) {
    sqlx::query_as(
        "select current_status, consecutive_failures, consecutive_successes from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn incident_statuses(pool: &sqlx::PgPool, id: Uuid) -> Vec<String> {
    sqlx::query_scalar("select status from incidents where monitor_id = $1 order by started_at")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn apply(
    pool: &sqlx::PgPool,
    monitor: &Monitor,
    status: CheckStatus,
) -> Option<TransitionKind> {
    apply_outcome(pool, monitor, &outcome(status))
        .await
        .unwrap()
        .map(|t| t.kind)
}

async fn apply_at(
    pool: &sqlx::PgPool,
    monitor: &Monitor,
    status: CheckStatus,
    seconds: i64,
) -> Option<TransitionKind> {
    apply_outcome(pool, monitor, &at(outcome(status), seconds))
        .await
        .unwrap()
        .map(|t| t.kind)
}

#[sqlx::test]
async fn the_default_threshold_recovers_on_the_first_success(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 1).await;
    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Down).await,
        Some(TransitionKind::Down)
    );

    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Up).await,
        Some(TransitionKind::Recovered)
    );

    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["resolved"]);
}

#[sqlx::test]
async fn a_threshold_of_three_needs_three_consecutive_successes(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;

    for expected_successes in 1..=2 {
        assert_eq!(
            apply(&pool, &monitor, CheckStatus::Up).await,
            None,
            "no recovery yet"
        );
        assert_eq!(
            counters(&pool, monitor.id).await,
            ("down".to_string(), 0, expected_successes)
        );
        assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["open"]);
    }

    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Up).await,
        Some(TransitionKind::Recovered)
    );
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["resolved"]);
}

#[sqlx::test]
async fn a_failure_while_recovering_starts_the_count_again(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    apply(&pool, &monitor, CheckStatus::Up).await;
    apply(&pool, &monitor, CheckStatus::Up).await;

    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Down).await,
        None,
        "already down: no second alert"
    );
    // The successes had reset the failure count, so this is failure #1 again.
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 1, 0)
    );
    assert_eq!(
        incident_statuses(&pool, monitor.id).await,
        vec!["open"],
        "still exactly one open incident"
    );

    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Up).await,
        Some(TransitionKind::Recovered)
    );
}

#[sqlx::test]
async fn successes_while_up_or_pending_never_build_up_a_count(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    for _ in 0..3 {
        assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    }
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
}

#[sqlx::test]
async fn recovery_threshold_is_part_of_the_api(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let body = |extra: serde_json::Value| {
        let mut body = json!({ "name": "API", "check_type": "http", "url": "https://example.com" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };

    let (status, default) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(body(json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{default}");
    assert_eq!(default["recovery_threshold"], 1);
    assert_eq!(default["consecutive_successes"], 0);

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(body(json!({ "recovery_threshold": 3 }))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["recovery_threshold"], 3);

    for invalid in [0, 11] {
        let (status, error) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(body(json!({ "recovery_threshold": invalid }))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("recovery_threshold"),
            "{error}"
        );
    }

    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "recovery_threshold": 5 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["recovery_threshold"], 5);
    let due: bool = sqlx::query_scalar("select next_check_at <= now() from monitors where id = $1")
        .bind(Uuid::parse_str(created["id"].as_str().unwrap()).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        !due,
        "the recovery threshold is not check config: no reschedule"
    );
}

// ---------------------------------------------------------------------------
// The edges: the range, a threshold edited while a check is in flight, results
// that must not count (stale, repeated, or from a paused monitor), what the
// API and the database keep to themselves, and the alert end to end.
// ---------------------------------------------------------------------------

/// The range is 1 to 10 inclusive, on create and on PATCH, and a refused edit
/// stores nothing.
#[sqlx::test]
async fn the_threshold_range_is_one_to_ten_on_create_and_patch(pool: sqlx::PgPool) {
    const RANGE: &str = "recovery_threshold must be between 1 and 10";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let create = |threshold: serde_json::Value| {
        json!({
            "name": "API", "check_type": "http", "url": "https://example.com",
            "recovery_threshold": threshold,
        })
    };

    for edge in [1, 10] {
        let (status, created) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(create(json!(edge))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["recovery_threshold"], edge);
    }
    for invalid in [-1, 0, 11, i32::MAX] {
        let (status, error) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(create(json!(invalid))),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "POST {invalid}");
        assert_eq!(error["error"], RANGE, "POST {invalid}");
    }
    // Not integers that fit: refused before validation, never a 500.
    for invalid in [json!("3"), json!(3.5), json!(i64::MAX), json!([3])] {
        let (status, _) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(create(invalid.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "POST {invalid}");
    }

    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(create(json!(4))),
    )
    .await;
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    for invalid in [-1, 0, 11] {
        let (status, error) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ "recovery_threshold": invalid })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "PATCH {invalid}");
        assert_eq!(error["error"], RANGE, "PATCH {invalid}");
    }
    let (_, unchanged) = call(&app, "GET", &item, Some(&member), None).await;
    assert_eq!(
        unchanged["recovery_threshold"], 4,
        "refused edits store nothing"
    );
    for edge in [1, 10] {
        let (status, patched) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ "recovery_threshold": edge })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{patched}");
        assert_eq!(patched["recovery_threshold"], edge);
    }
}

/// The success count is the worker's: a client cannot set it, an edit does not
/// reset it (a rename in the middle of a recovery must not start the count
/// again), and every reader sees it next to the threshold, though only a member
/// may change the threshold.
#[sqlx::test]
async fn the_success_count_belongs_to_the_worker(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "API", "check_type": "http", "url": "https://example.com",
            "consecutive_successes": 7,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["consecutive_successes"], 0, "ignored on create");
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let item = format!("{collection}/{id}");
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "consecutive_successes": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["consecutive_successes"], 0, "ignored on PATCH");

    // Two of three successes in, as the worker would have left it.
    sqlx::query(
        "update monitors set current_status = 'down', recovery_threshold = 3,
                consecutive_successes = 2 where id = $1",
    )
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Renamed", "recovery_threshold": 4 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["consecutive_successes"], 2);
    assert_eq!(counters(&pool, id).await, ("down".to_string(), 0, 2));

    let (_, one) = call(&app, "GET", &item, Some(&viewer), None).await;
    let (_, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    for seen in [&one, &list[0]] {
        assert_eq!(seen["recovery_threshold"], 4, "{seen}");
        assert_eq!(seen["consecutive_successes"], 2, "{seen}");
    }

    // Reading is all a viewer may do with it.
    let (status, _) = call(
        &app,
        "PATCH",
        &item,
        Some(&viewer),
        Some(json!({ "recovery_threshold": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let stored: i32 = sqlx::query_scalar("select recovery_threshold from monitors where id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, 4);
}

/// A row written around the API cannot hold a threshold the worker could not
/// make sense of, and a new row starts with the defaults.
#[sqlx::test]
async fn the_database_keeps_the_threshold_between_one_and_ten(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "Checkout API", "http://127.0.0.1:9/", 1).await;
    let stored = |id: Uuid| {
        sqlx::query_as::<_, (i32, i32)>(
            "select recovery_threshold, consecutive_successes from monitors where id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
    };
    assert_eq!(stored(id).await.unwrap(), (1, 0), "the defaults");

    for threshold in [0, 11, -1] {
        let error = sqlx::query("update monitors set recovery_threshold = $2 where id = $1")
            .bind(id)
            .bind(threshold)
            .execute(&pool)
            .await
            .unwrap_err();
        let code = error.as_database_error().and_then(|e| e.code());
        assert_eq!(
            code.as_deref(),
            Some("23514"),
            "{threshold}: expected a check violation, got {error}"
        );
    }
    for threshold in [1, 10] {
        sqlx::query("update monitors set recovery_threshold = $2 where id = $1")
            .bind(id)
            .bind(threshold)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(stored(id).await.unwrap().0, threshold);
    }
}

/// Like the confirmation threshold, the one in force is the monitor's now, not
/// the one it had when its check was claimed: an edit made while the check was
/// in flight counts, whichever way it went.
#[sqlx::test]
async fn the_threshold_in_force_is_the_one_the_monitor_has_when_the_result_is_applied(
    pool: sqlx::PgPool,
) {
    // (threshold when the check was claimed, threshold when its result arrives)
    for (claimed_with, now) in [(3, 1), (1, 3)] {
        let snapshot = monitor_with_recovery(&pool, claimed_with).await;
        apply(&pool, &snapshot, CheckStatus::Down).await;
        sqlx::query("update monitors set recovery_threshold = $2 where id = $1")
            .bind(snapshot.id)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();

        for _ in 1..now {
            assert_eq!(apply(&pool, &snapshot, CheckStatus::Up).await, None);
        }
        assert_eq!(
            apply(&pool, &snapshot, CheckStatus::Up).await,
            Some(TransitionKind::Recovered),
            "claimed with {claimed_with}, needs {now} now"
        );
    }
}

/// Lowering the threshold below the count already reached recovers the monitor
/// on its next success: a monitor can never be stuck needing more than the
/// threshold it has.
#[sqlx::test]
async fn lowering_the_threshold_mid_recovery_recovers_on_the_next_success(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 5).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    for _ in 0..3 {
        assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    }
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 3)
    );

    sqlx::query("update monitors set recovery_threshold = 2 where id = $1")
        .bind(monitor.id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Up).await,
        Some(TransitionKind::Recovered)
    );
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["resolved"]);
}

/// A result older than the last one applied, or with the same timestamp (a
/// replay), is dropped before the state machine: counting it would let one
/// success recover a monitor twice over, or a late one recover it early.
#[sqlx::test]
async fn a_stale_or_repeated_success_does_not_count(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Down, 0).await,
        Some(TransitionKind::Down)
    );

    assert_eq!(apply_at(&pool, &monitor, CheckStatus::Up, 10).await, None);
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 1)
    );

    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 10).await,
        None,
        "the same result again"
    );
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 5).await,
        None,
        "an older result"
    );
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 1),
        "neither counted"
    );

    assert_eq!(apply_at(&pool, &monitor, CheckStatus::Up, 11).await, None);
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 2)
    );
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 12).await,
        Some(TransitionKind::Recovered)
    );
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
}

/// A stale failure does not restart the count either.
#[sqlx::test]
async fn a_stale_failure_does_not_restart_the_count(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 2).await;
    apply_at(&pool, &monitor, CheckStatus::Down, 0).await;
    assert_eq!(apply_at(&pool, &monitor, CheckStatus::Up, 10).await, None);

    assert_eq!(apply_at(&pool, &monitor, CheckStatus::Down, 5).await, None);

    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 1)
    );
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 11).await,
        Some(TransitionKind::Recovered)
    );
}

/// The result of a paused monitor is discarded whole: it neither counts toward
/// recovery nor recovers the monitor.
#[sqlx::test]
async fn a_paused_monitor_does_not_recover(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 2).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    sqlx::query("update monitors set paused = true where id = $1")
        .bind(monitor.id)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        apply(&pool, &monitor, CheckStatus::Up).await,
        None,
        "this success would have been the second"
    );

    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 1)
    );
    assert_eq!(incident_statuses(&pool, monitor.id).await, vec!["open"]);
}

/// A monitor that recovered starts over: a new outage needs the full count
/// again, and opens its own incident.
#[sqlx::test]
async fn a_second_outage_needs_the_full_count_again(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 2).await;
    for _ in 0..2 {
        assert_eq!(
            apply(&pool, &monitor, CheckStatus::Down).await,
            Some(TransitionKind::Down)
        );
        assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
        assert_eq!(
            apply(&pool, &monitor, CheckStatus::Up).await,
            Some(TransitionKind::Recovered)
        );
        assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    }
    assert_eq!(
        incident_statuses(&pool, monitor.id).await,
        vec!["resolved", "resolved"]
    );
}

async fn respond_with_status(server: &MockServer, status: u16) {
    server.reset().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(status))
        .mount(server)
        .await;
}

/// End to end, through the worker's passes, a real HTTP check and the mail
/// server: a monitor with a recovery threshold of 2 is still down, and still
/// silent, after its first healthy check, and is mailed as recovered after its
/// second.
#[sqlx::test]
async fn the_recovery_alert_goes_out_only_when_the_threshold_is_reached(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let project = seed_project(&pool).await;
    let recipient = unique_email("oncall");
    let user = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, user, "viewer").await;
    let server = MockServer::start().await;
    respond_with_status(&server, 503).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    sqlx::query("update monitors set recovery_threshold = 2 where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let subjects = |mail: &[(String, String)]| -> Vec<String> {
        mail.iter().map(|(subject, _)| subject.clone()).collect()
    };

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);
    assert_eq!(counters(&pool, monitor).await, ("down".to_string(), 1, 0));

    respond_with_status(&server, 200).await;
    make_due(&pool, monitor).await;
    assert_eq!(run_due_checks(&state).await.unwrap(), 1);
    assert_eq!(counters(&pool, monitor).await, ("down".to_string(), 0, 1));
    assert_eq!(incident_statuses(&pool, monitor).await, vec!["open"]);
    assert_eq!(
        subjects(&settled_mail(&recipient, 1).await),
        vec!["[Nami] Checkout API is DOWN"],
        "one healthy check is not a recovery"
    );

    make_due(&pool, monitor).await;
    assert_eq!(run_due_checks(&state).await.unwrap(), 1);
    assert_eq!(counters(&pool, monitor).await, ("up".to_string(), 0, 0));
    assert_eq!(incident_statuses(&pool, monitor).await, vec!["resolved"]);
    assert_eq!(
        subjects(&settled_mail(&recipient, 2).await),
        vec![
            "[Nami] Checkout API is DOWN",
            "[Nami] Checkout API is back UP"
        ]
    );
    ch.drop_database().await.unwrap();
}

/// A member of the monitor's project, logged in, and the monitor's API path.
async fn member_of(
    pool: &sqlx::PgPool,
    monitor: &Monitor,
) -> (axum::Router, axum::http::HeaderValue, String) {
    let app = test_app(pool.clone());
    let (_, _, member) = user_with_role(&app, pool, monitor.project_id, Some("member")).await;
    let item = format!("/projects/{}/monitors/{}", monitor.project_id, monitor.id);
    (app, member, item)
}

/// Successes against the old target say nothing about the new one: editing
/// what is checked starts both counts again (Phase 2 final review ruling).
#[sqlx::test]
async fn editing_the_target_starts_the_counts_again(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    assert_eq!(apply(&pool, &monitor, CheckStatus::Up).await, None);
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 1)
    );
    let (app, member, item) = member_of(&pool, &monitor).await;

    let (status, body) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "url": "http://127.0.0.1:10/" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        counters(&pool, monitor.id).await,
        ("down".to_string(), 0, 0)
    );

    // Failures too: two of three toward confirmation, then a new target.
    let pending = seed_http_monitor(
        &pool,
        monitor.project_id,
        "Pending",
        "http://127.0.0.1:9/",
        3,
    )
    .await;
    let pending: Monitor = sqlx::query_as("select * from monitors where id = $1")
        .bind(pending)
        .fetch_one(&pool)
        .await
        .unwrap();
    for seconds in 0..2 {
        apply_at(&pool, &pending, CheckStatus::Down, seconds).await;
    }
    assert_eq!(counters(&pool, pending.id).await.1, 2);
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/projects/{}/monitors/{}", pending.project_id, pending.id),
        Some(&member),
        Some(json!({ "timeout_seconds": 7 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(counters(&pool, pending.id).await.1, 0);
}

#[sqlx::test]
async fn resuming_a_paused_monitor_starts_the_counts_again(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "Checkout API", "http://127.0.0.1:9/", 3).await;
    let monitor: Monitor = sqlx::query_as("select * from monitors where id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    for seconds in 0..2 {
        apply_at(&pool, &monitor, CheckStatus::Down, seconds).await;
    }
    let (app, member, item) = member_of(&pool, &monitor).await;
    for paused in [true, false] {
        let (status, body) = call(
            &app,
            "PATCH",
            &item,
            Some(&member),
            Some(json!({ "paused": paused })),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if paused {
            assert_eq!(counters(&pool, id).await.1, 2, "pausing alone keeps them");
        }
    }
    assert_eq!(counters(&pool, id).await, ("pending".to_string(), 0, 0));
}

#[sqlx::test]
async fn renaming_or_retuning_thresholds_keeps_the_counts(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply(&pool, &monitor, CheckStatus::Down).await;
    apply(&pool, &monitor, CheckStatus::Up).await;
    let (app, member, item) = member_of(&pool, &monitor).await;
    for body in [
        json!({ "name": "Renamed" }),
        json!({ "recovery_threshold": 4 }),
        json!({ "confirmation_threshold": 2 }),
        json!({ "paused": false }),
    ] {
        let (status, response) =
            call(&app, "PATCH", &item, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::OK, "{response}");
        assert_eq!(
            counters(&pool, monitor.id).await,
            ("down".to_string(), 0, 1),
            "{body}"
        );
    }
}

/// At threshold 1 only a monitor that was down recovers: a success of an up
/// or pending monitor is no transition at all.
#[sqlx::test]
async fn at_threshold_one_only_a_down_monitor_recovers(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 1).await;
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 0).await,
        None,
        "pending"
    );
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 1).await,
        None,
        "up"
    );
    assert_eq!(counters(&pool, monitor.id).await, ("up".to_string(), 0, 0));
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Down, 2).await,
        Some(TransitionKind::Down)
    );
    assert_eq!(
        apply_at(&pool, &monitor, CheckStatus::Up, 3).await,
        Some(TransitionKind::Recovered)
    );
}

/// Successes arriving together for a down monitor that needs three: whatever
/// order they take the row lock in, exactly one recovers it and alerts.
#[sqlx::test]
async fn concurrent_successes_recover_exactly_once_above_threshold_one(pool: sqlx::PgPool) {
    let monitor = monitor_with_recovery(&pool, 3).await;
    apply_at(&pool, &monitor, CheckStatus::Down, 0).await;

    let mut gate = pool.begin().await.unwrap();
    sqlx::query("select id from monitors where id = $1 for update")
        .bind(monitor.id)
        .fetch_one(&mut *gate)
        .await
        .unwrap();
    let mut applying = tokio::task::JoinSet::new();
    for seconds in 1..=6 {
        let (pool, monitor) = (pool.clone(), monitor.clone());
        applying.spawn(async move {
            apply_outcome(&pool, &monitor, &at(outcome(CheckStatus::Up), seconds)).await
        });
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    gate.commit().await.unwrap();
    let mut recovered = 0;
    while let Some(done) = applying.join_next().await {
        if done
            .unwrap()
            .unwrap()
            .is_some_and(|t| t.kind == TransitionKind::Recovered)
        {
            recovered += 1;
        }
    }
    assert!(recovered <= 1, "{recovered} recoveries");
    let (status, _, _) = counters(&pool, monitor.id).await;
    assert_eq!(
        recovered == 1,
        status == "up",
        "a recovery is the one that brought it back"
    );
    assert_eq!(incident_statuses(&pool, monitor.id).await.len(), 1);
}
