// Log alerts: API, evaluation, incidents and email. Requires
// `docker compose up -d postgres clickhouse mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{app, call, logs::{post_json, seed_source}, public_app, seed_http_monitor, seed_project, settled_mail, state_with_clickhouse, test_app, user_with_role};
use nami_engine::{
    core::state::AppState,
    logs::{
        alert::{self, MAX_ALERT_EVALUATIONS, MAX_ALERT_EVALUATIONS_PER_PROJECT, model::MAX_LOG_ALERTS_PER_PROJECT},
        query::search::{MAX_QUERIES_PER_USER, QueryPermit},
        view::saved_query::{LogQuery, MAX_QUERY_BYTES},
    },
    uptime::worker::Notifications,
};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use serde_json::{Value, json};

fn alert_body(name: &str) -> Value {
    json!({ "name": name, "query": { "levels": ["error"] }, "threshold": 2, "comparison": "gt",
            "window_seconds": 300, "interval_seconds": 60 })
}

async fn make_due(pool: &sqlx::PgPool, alert: &str) {
    sqlx::query("update log_alerts set next_evaluation_at = now() - interval '1 second' where id = $1::uuid")
        .bind(alert).execute(pool).await.unwrap();
}

async fn alert_row(pool: &sqlx::PgPool, alert: &str) -> (String, Option<i64>) {
    sqlx::query_as("select state, last_count from log_alerts where id = $1::uuid").bind(alert).fetch_one(pool).await.unwrap()
}

async fn incidents(pool: &sqlx::PgPool, alert: &str) -> Vec<(String, String)> {
    sqlx::query_as("select status, title from incidents where log_alert_id = $1::uuid order by started_at")
        .bind(alert).fetch_all(pool).await.unwrap()
}

#[sqlx::test]
async fn roles_and_validation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign_source, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/log-alerts");

    let (status, created) = call(&app, "POST", &base, Some(&member), Some(alert_body("Errors"))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!((created["state"].clone(), created["enabled"].clone()), (json!("ok"), json!(true)));
    let one = format!("{base}/{}", created["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &base, Some(&viewer), None).await.1.as_array().unwrap().len(), 1);
    assert_eq!(call(&app, "POST", &base, Some(&viewer), Some(alert_body("x"))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "PATCH", &one, Some(&viewer), Some(json!({"threshold": 5}))).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &one, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &base, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    let preview = format!("{base}/preview");
    let preview_body = json!({"query": {}, "window_seconds": 300});
    assert_eq!(call(&app, "POST", &preview, Some(&outsider), Some(preview_body)).await.0, StatusCode::FORBIDDEN);
    // Another project's source is a 400, stored or only previewed.
    let mut theirs = alert_body("Theirs");
    theirs["query"] = json!({"source_ids": [foreign_source]});
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(theirs)).await.0, StatusCode::BAD_REQUEST);
    let foreign_preview = json!({"query": {"source_ids": [foreign_source]}, "window_seconds": 300});
    assert_eq!(call(&app, "POST", &preview, Some(&viewer), Some(foreign_preview)).await.0, StatusCode::BAD_REQUEST);
    let (status, patched) = call(&app, "PATCH", &one, Some(&member), Some(json!({"threshold": 5}))).await;
    assert_eq!((status, patched["threshold"].clone()), (StatusCode::OK, json!(5)));
    assert_eq!(call(&app, "DELETE", &one, Some(&member), None).await.0, StatusCode::FORBIDDEN, "admins delete alerts");

    for patch in [
        json!({"window_seconds": 59}), json!({"window_seconds": 86401}), json!({"interval_seconds": 59}),
        json!({"comparison": "eq"}), json!({"threshold": -1}), json!({"comparison": "lt", "threshold": 0}),
        json!({"threshold": 1_000_000_000_001_i64}), json!({"name": ""}),
        json!({"query": {"filters": [{"path":"a","op":"gt","value":"x"}]}}),
        json!({"query": {"source_ids": [foreign_source]}}),
    ] {
        let (status, body) = call(&app, "PATCH", &one, Some(&member), Some(patch.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{patch}: {body}");
    }
    assert_eq!(call(&app, "DELETE", &one, Some(&admin), None).await.0, StatusCode::NO_CONTENT);

    let (_, _, theirs) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (_, foreign) = call(&app, "POST", &format!("/projects/{other}/log-alerts"), Some(&theirs), Some(alert_body("Theirs"))).await;
    let foreign = format!("{base}/{}", foreign["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &foreign, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "PATCH", &foreign, Some(&admin), Some(json!({"threshold": 5}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "DELETE", &foreign, Some(&admin), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn the_preview_counts_only_this_projects_matching_rows(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(&public, &token, json!([{"level":"error","message":"a"},{"level":"error","message":"b"},{"message":"c"}])).await;
    post_json(&public, &foreign, json!([{"level":"error","message":"theirs"}])).await;
    let uri = format!("/projects/{project}/log-alerts/preview");
    let (status, body) = call(&app, "POST", &uri, Some(&viewer), Some(json!({"query":{"levels":["error"]},"window_seconds":300}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 2);
    let (status, _) = call(&app, "POST", &uri, Some(&viewer), Some(json!({"query":{},"window_seconds":30}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_alert_opens_and_resolves_its_incident_and_emails_once_each(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Checkout errors"))).await;
    let id = created["id"].as_str().unwrap().to_string();

    // Another project's errors never count.
    post_json(&public, &foreign, json!(vec![json!({"level":"error","message":"x"}); 5])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), Some(0)));

    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    make_due(&pool, &id).await;
    assert_eq!(alert::evaluate_due_alerts(&state).await.unwrap(), 1);
    assert_eq!(alert_row(&pool, &id).await, ("alerting".to_string(), Some(3)));
    assert_eq!(incidents(&pool, &id).await, [("open".to_string(), "Log alert: Checkout errors".to_string())]);

    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(incidents(&pool, &id).await.len(), 1, "still alerting: no second incident");

    let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"threshold": 10}))).await;
    assert_eq!(status, StatusCode::OK);
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "ok");
    assert_eq!(incidents(&pool, &id).await[0].0, "resolved");

    let mail = settled_mail(&email, 2).await;
    let subjects: Vec<&str> = mail.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, ["[Nami] [high] Log alert: Checkout errors", "[Nami] Resolved: Log alert: Checkout errors"]);
    assert!(mail[0].1.contains("Cause: 3 matching events in the last 5 min (threshold: > 2)"), "{}", mail[0].1);

    let (_, listed) = call(&app, "GET", &format!("/projects/{project}/incidents"), Some(&member), None).await;
    assert_eq!(listed["incidents"][0]["source_name"], "Checkout errors");
    assert_eq!(listed["incidents"][0]["log_alert_id"], json!(id));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_failed_count_changes_nothing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("E"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "alerting");
    let evaluated: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select last_evaluated_at from log_alerts where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();

    // ClickHouse unreachable: AppState::for_test's client points at a database that does not exist.
    let broken = AppState::for_test(pool.clone());
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&broken).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("alerting".to_string(), Some(3)));
    assert_eq!(incidents(&pool, &id).await[0].0, "open");
    let (after, error): (Option<chrono::DateTime<chrono::Utc>>, Option<String>) =
        sqlx::query_as("select last_evaluated_at, last_error from log_alerts where id = $1::uuid")
            .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(after, evaluated);
    // Only the failure itself is recorded, as a fixed text (never ClickHouse's own message).
    assert_eq!(error.as_deref(), Some("the log store is unavailable"));
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    let error: Option<String> = sqlx::query_scalar("select last_error from log_alerts where id = $1::uuid")
        .bind(&id).fetch_one(&pool).await.unwrap();
    assert_eq!(error, None, "a successful evaluation clears it");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_project_wide_maintenance_window_holds_the_alert_state(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Deploy"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    sqlx::query("insert into maintenance_windows (project_id, title, kind, starts_at, ends_at)
                 values ($1, 'deploy', 'once', now() - interval '1 minute', now() + interval '1 hour')")
        .bind(project).execute(&pool).await.unwrap();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), Some(3)), "counted, but the state stands still");
    assert!(incidents(&pool, &id).await.is_empty());

    sqlx::query("delete from maintenance_windows where project_id = $1").bind(project).execute(&pool).await.unwrap();
    make_due(&pool, &id).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await.0, "alerting", "the first evaluation after the window applies");
    assert_eq!(settled_mail(&email, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_evaluation_of_an_edited_alert_is_dropped(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member),
        Some(json!({ "name": "Quiet", "query": {}, "threshold": 1, "comparison": "lt", "window_seconds": 300, "interval_seconds": 60 }))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let claimed = alert::claim_due_alerts(&pool, 10, &[]).await.unwrap();
    assert_eq!(claimed.len(), 1);
    // Edited while the count runs: the old settings ("< 1") must not open an incident.
    let (status, _) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"name": "Renamed"}))).await;
    assert_eq!(status, StatusCode::OK);
    let applied = alert::apply_evaluation(&pool, &claimed[0], 0, chrono::Utc::now()).await.unwrap();
    assert!(applied.is_none());
    assert_eq!(alert_row(&pool, &id).await, ("ok".to_string(), None));
    assert!(incidents(&pool, &id).await.is_empty());
    ch.drop_database().await.unwrap();
}

/// Disabling an alerting alert resolves its incident and tells whoever was
/// paged why (R17, superseding Logs Part 2's silent disable).
#[sqlx::test]
async fn disabled_alerts_are_skipped_and_disabling_resolves_with_a_reason(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Quiet"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{id}"), Some(&member), Some(json!({"enabled": false}))).await;
    assert_eq!((status, body["state"].clone()), (StatusCode::OK, json!("ok")));
    assert_eq!(incidents(&pool, &id).await[0].0, "resolved");
    make_due(&pool, &id).await;
    assert_eq!(alert::evaluate_due_alerts(&state).await.unwrap(), 0, "a disabled alert is never claimed");
    common::run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 2).await;
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert_eq!(mail[1].0, "[Nami] Resolved: Log alert: Quiet");
    assert!(mail[1].1.contains("Reason: alert disabled"), "{}", mail[1].1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn overlapping_passes_evaluate_each_alert_once(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Once"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    let (a, b) = tokio::join!(alert::evaluate_due_alerts(&state), alert::evaluate_due_alerts(&state));
    assert_eq!(a.unwrap() + b.unwrap(), 1);
    assert_eq!(incidents(&pool, &id).await.len(), 1);
    assert_eq!(settled_mail(&email, 1).await.len(), 1);
    ch.drop_database().await.unwrap();
}

async fn insert_alerts(pool: &sqlx::PgPool, project: uuid::Uuid, count: i64) {
    sqlx::query(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds)
         select $1, 'filler ' || n, 1, 'gt', 300, 60 from generate_series(1, $2::int) n",
    )
    .bind(project)
    .bind(count)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn a_project_holds_at_most_the_alert_cap(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    insert_alerts(&pool, project, MAX_LOG_ALERTS_PER_PROJECT - 1).await;
    let base = format!("/projects/{project}/log-alerts");
    let (status, body) = call(&app, "POST", &base, Some(&member), Some(alert_body("Last"))).await;
    assert_eq!(status, StatusCode::CREATED, "the alert at the cap: {body}");
    let (status, body) = call(&app, "POST", &base, Some(&member), Some(alert_body("One more"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn a_stored_query_is_capped_in_bytes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    // A level is trimmed before it is checked, so padding keeps it valid
    // and grows only the stored JSON.
    let query = |pad: usize| json!({ "levels": [format!("error{}", " ".repeat(pad))] });
    let size = |pad: usize| serde_json::to_vec(&serde_json::from_value::<LogQuery>(query(pad)).unwrap()).unwrap().len();
    let pad = MAX_QUERY_BYTES - size(0);
    assert_eq!(size(pad), MAX_QUERY_BYTES);
    let base = format!("/projects/{project}/log-alerts");
    let body = |pad: usize| json!({ "name": "Big", "query": query(pad), "threshold": 2, "comparison": "gt",
                                     "window_seconds": 300, "interval_seconds": 60 });
    let (status, created) = call(&app, "POST", &base, Some(&member), Some(body(pad))).await;
    assert_eq!(status, StatusCode::CREATED, "at the bound: {created}");
    let (status, refused) = call(&app, "POST", &base, Some(&member), Some(body(pad + 1))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(refused["error"], format!("query must be at most {MAX_QUERY_BYTES} bytes"), "{refused}");
}

#[sqlx::test]
async fn a_pass_claims_no_more_alerts_than_its_budget_has_places(pool: sqlx::PgPool) {
    // ClickHouse is unreachable here: each claimed count fails, which is
    // enough to see what was claimed.
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    insert_alerts(&pool, project, 2).await;
    let due = || async {
        sqlx::query_scalar::<_, i64>("select count(*) from log_alerts where next_evaluation_at <= now()")
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let notifications = Notifications::new();
    let none = Arc::new(Semaphore::new(0));
    assert_eq!(alert::run_pass_with(&state, &notifications, &none).await.unwrap(), 0);
    assert_eq!(due().await, 2);
    let one = Arc::new(Semaphore::new(1));
    assert_eq!(alert::run_pass_with(&state, &notifications, &one).await.unwrap(), 1);
    assert_eq!(due().await, 1);
    assert_eq!(one.available_permits(), 1, "the place is given back");
}

#[sqlx::test]
async fn the_preview_holds_one_of_the_users_query_slots(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/log-alerts/preview");
    let body = json!({"query": {}, "window_seconds": 300});
    let held: Vec<_> = (0..MAX_QUERIES_PER_USER - 1).map(|_| QueryPermit::acquire(&logs, user).unwrap()).collect();
    let (status, out) = call(&app, "POST", &uri, Some(&viewer), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "the user's last slot: {out}");
    let _full = QueryPermit::acquire(&logs, user).unwrap();
    assert_eq!(call(&app, "POST", &uri, Some(&viewer), Some(body)).await.0, StatusCode::TOO_MANY_REQUESTS);
    drop(held);
    ch.drop_database().await.unwrap();
}

/// Only project-wide windows hold log alerts; one scoped to a monitor does not.
#[sqlx::test]
async fn a_monitor_scoped_maintenance_window_does_not_hold_the_alert(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Scoped"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let monitor = seed_http_monitor(&pool, project, "web", "https://example.com", 1).await;
    sqlx::query("insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
                 values ($1, $2, 'web deploy', 'once', now() - interval '1 minute', now() + interval '1 hour')")
        .bind(project).bind(monitor).execute(&pool).await.unwrap();
    post_json(&public, &token, json!(vec![json!({"level":"error","message":"boom"}); 3])).await;
    alert::evaluate_due_alerts(&state).await.unwrap();
    assert_eq!(alert_row(&pool, &id).await, ("alerting".to_string(), Some(3)));
    assert_eq!(incidents(&pool, &id).await.len(), 1);
    ch.drop_database().await.unwrap();
}

/// A PATCH reads the row under its lock: a write that lands while the PATCH
/// waits is kept, never overwritten with what the PATCH saw before it.
#[sqlx::test]
async fn a_patch_merges_into_the_row_as_it_is_under_the_lock(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(alert_body("Errors"))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let uri = format!("/projects/{project}/log-alerts/{id}");

    let mut held = pool.begin().await.unwrap();
    sqlx::query("select id from log_alerts where id = $1::uuid for update").bind(&id).execute(&mut *held).await.unwrap();
    let concurrent = async {
        // Until the PATCH waits on the row lock.
        let mut waiting = 0_i64;
        for _ in 0..250 {
            waiting = sqlx::query_scalar(
                "select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'",
            )
            .fetch_one(&pool).await.unwrap();
            if waiting > 0 { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(waiting > 0, "the PATCH never waited on the row lock");
        sqlx::query("update log_alerts set name = 'Other' where id = $1::uuid").bind(&id).execute(&mut *held).await.unwrap();
        held.commit().await.unwrap();
    };
    let ((status, patched), ()) = tokio::join!(call(&app, "PATCH", &uri, Some(&member), Some(json!({"threshold": 7}))), concurrent);
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!((patched["name"].clone(), patched["threshold"].clone()), (json!("Other"), json!(7)));
}

/// A stored source deleted since does not block other edits; naming it again
/// is still refused.
#[sqlx::test]
async fn a_patch_checks_only_the_sources_it_names(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut body = alert_body("Scoped");
    body["query"] = json!({"source_ids": [source]});
    let (status, created) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&member), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let uri = format!("/projects/{project}/log-alerts/{}", created["id"].as_str().unwrap());
    sqlx::query("delete from sources where id = $1").bind(source).execute(&pool).await.unwrap();

    let (status, patched) = call(&app, "PATCH", &uri, Some(&member), Some(json!({"enabled": false}))).await;
    assert_eq!((status, patched["enabled"].clone()), (StatusCode::OK, json!(false)), "{patched}");
    let (status, _) = call(&app, "PATCH", &uri, Some(&member), Some(json!({"query": {"source_ids": [source]}}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Alerts of `project` a pass claimed (pushed into the future).
async fn claimed_in(pool: &sqlx::PgPool, project: uuid::Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from log_alerts where project_id = $1 and next_evaluation_at > now()")
        .bind(project).fetch_one(pool).await.unwrap()
}

/// Every alert due again, `busy`'s the most overdue.
async fn all_due(pool: &sqlx::PgPool, busy: uuid::Uuid) {
    sqlx::query("update log_alerts set next_evaluation_at = now() - case when project_id = $1
                 then interval '1 hour' else interval '1 second' end")
        .bind(busy).execute(pool).await.unwrap();
}

/// A project with more due alerts than the pass has places takes at most its
/// share, so another project's due alert is still claimed. Evaluations the
/// project already has running count against that share.
#[sqlx::test]
async fn one_project_takes_at_most_its_share_of_the_evaluations(pool: sqlx::PgPool) {
    // ClickHouse is unreachable here: each claimed count fails, which is
    // enough to see what was claimed.
    let state = AppState::for_test(pool.clone());
    let busy = seed_project(&pool).await;
    let quiet = seed_project(&pool).await;
    insert_alerts(&pool, busy, MAX_ALERT_EVALUATIONS as i64 + 2).await;
    insert_alerts(&pool, quiet, 1).await;
    let notifications = Notifications::new();
    let budget = Arc::new(Semaphore::new(MAX_ALERT_EVALUATIONS));
    let share = MAX_ALERT_EVALUATIONS_PER_PROJECT as i64;

    all_due(&pool, busy).await;
    assert_eq!(alert::run_pass_with(&state, &notifications, &budget).await.unwrap() as i64, share + 1);
    assert_eq!((claimed_in(&pool, busy).await, claimed_in(&pool, quiet).await), (share, 1));

    let running = &state.logs.alert_evaluations;
    let mut held = vec![running.acquire(busy, usize::MAX).unwrap()];
    all_due(&pool, busy).await;
    alert::run_pass_with(&state, &notifications, &budget).await.unwrap();
    assert_eq!((claimed_in(&pool, busy).await, claimed_in(&pool, quiet).await), (share - 1, 1), "one place left");

    held.push(running.acquire(busy, usize::MAX).unwrap());
    all_due(&pool, busy).await;
    alert::run_pass_with(&state, &notifications, &budget).await.unwrap();
    assert_eq!((claimed_in(&pool, busy).await, claimed_in(&pool, quiet).await), (0, 1), "at its share: none");
    drop(held);
    assert_eq!(running.total(), 0, "every evaluation gave its place back");
}
