// Log alerts: API, evaluation, incidents and email. Requires
// `docker compose up -d postgres clickhouse mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{app, call, logs::{post_json, seed_source}, public_app, seed_project, settled_mail, state_with_clickhouse, test_app, user_with_role};
use nami_engine::{logs::alert, core::state::AppState};
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
    assert_eq!(subjects, ["[Nami] Log alert Checkout errors triggered", "[Nami] Log alert Checkout errors resolved"]);
    assert!(mail[0].1.contains("Condition: 3 matching events in the last 5 min (threshold: > 2)"), "{}", mail[0].1);

    let (_, listed) = call(&app, "GET", &format!("/projects/{project}/incidents"), Some(&member), None).await;
    assert_eq!(listed[0]["source_name"], "Checkout errors");
    assert_eq!(listed[0]["log_alert_id"], json!(id));
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
    let claimed = alert::claim_due_alerts(&pool, 10).await.unwrap();
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

#[sqlx::test]
async fn disabled_alerts_are_skipped_and_disabling_resolves_quietly(pool: sqlx::PgPool) {
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
    assert_eq!(settled_mail(&email, 1).await.len(), 1, "only the trigger email: disabling sends nothing");
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
