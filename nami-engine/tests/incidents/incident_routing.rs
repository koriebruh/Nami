// Monitors, heartbeats and log alerts open incidents through escalation
// (NAMI-INC-10). Requires `docker compose up -d postgres clickhouse mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{
    call, seed_http_monitor, seed_project, set_policy, settled_mail, state_with_clickhouse,
    test_app, user_with_role,
};
use nami_engine::{
    logs::alert,
    uptime::{alerts as uptime_alerts, worker},
};
use serde_json::json;
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::any};

async fn severity(pool: &sqlx::PgPool, project: Uuid, name: &str) -> Uuid {
    sqlx::query_scalar("select id from incident_severities where project_id = $1 and name = $2")
        .bind(project)
        .bind(name)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_monitor_pages_through_its_own_policy_with_its_severity(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, policy) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin), Some(json!({
        "name": "api team", "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]}))).await;
    assert_eq!(status, StatusCode::CREATED, "{policy}");
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let monitor = seed_http_monitor(&pool, project, "Checkout API", &server.uri(), 1).await;
    let critical = severity(&pool, project, "critical").await;
    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/monitors/{monitor}"), Some(&admin),
        Some(json!({"escalation_policy_id": policy["id"], "severity_id": critical}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["severity_id"], json!(critical));
    assert_eq!(body["escalation_policy_id"], policy["id"]);

    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let mail = settled_mail(&alice_email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [critical] Checkout API is down");
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "not on this policy");
    let (source, policy_id): (String, Uuid) = sqlx::query_as(
        "select source, escalation_policy_id from incidents where monitor_id = $1")
        .bind(monitor).fetch_one(&pool).await.unwrap();
    assert_eq!((source.as_str(), policy_id.to_string()), ("monitor", policy["id"].as_str().unwrap().to_string()));

    server.reset().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail[1].0, "[Nami] Resolved: Checkout API is down");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn an_acknowledged_monitor_incident_blocks_a_second_one_until_it_recovers(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let server = MockServer::start().await;
    Mock::given(any()).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let monitor = seed_http_monitor(&pool, project, "api", &server.uri(), 1).await;
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let id: Uuid = sqlx::query_scalar("select id from incidents where monitor_id = $1")
        .bind(monitor).fetch_one(&pool).await.unwrap();
    let (status, body) = call(&app, "POST", &format!("/projects/{project}/incidents/{id}/acknowledge"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // Force the monitor through up -> down again while the incident is acknowledged.
    sqlx::query("update monitors set current_status = 'up' where id = $1").bind(monitor).execute(&pool).await.unwrap();
    common::make_due(&pool, monitor).await;
    worker::run_due_checks(&state).await.unwrap();
    let count: i64 = sqlx::query_scalar("select count(*) from incidents where monitor_id = $1")
        .bind(monitor).fetch_one(&pool).await.unwrap();
    assert_eq!(count, 1);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_missed_heartbeat_pages_the_default_policy(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, hb) = call(&app, "POST", &format!("/projects/{project}/heartbeats"), Some(&member),
        Some(json!({"name": "nightly backup", "period_seconds": 60, "grace_seconds": 0}))).await;
    assert_eq!(status, StatusCode::CREATED, "{hb}");
    assert!(hb["escalation_policy_id"].is_null() && hb["severity_id"].is_null(), "{hb}");
    sqlx::query("update heartbeats set created_at = now() - interval '1 hour' where id = $1::uuid")
        .bind(hb["id"].as_str().unwrap()).execute(&pool).await.unwrap();
    worker::sweep_heartbeats(&state).await.unwrap();
    let mail = settled_mail(&email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [high] nightly backup missed its heartbeat");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_log_alert_opens_its_incident_on_its_policy_and_severity(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let policy = set_policy(&app, project, &admin, json!({"name": "Default",
        "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]})).await;
    let low = severity(&pool, project, "low").await;
    let alert: Uuid = sqlx::query_scalar(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds, severity_id)
         values ($1, 'Checkout errors', 10, 'gt', 300, 60, $2) returning id")
        .bind(project).bind(low).fetch_one(&pool).await.unwrap();
    let due = alert::claim_due_alerts(&pool, 10, &[]).await.unwrap();
    let transition = alert::apply_evaluation(&pool, &due[0], 12, chrono::Utc::now()).await.unwrap().unwrap();
    uptime_alerts::notify_transition(&state, &transition).await;
    let mail = settled_mail(&alice_email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [low] Log alert: Checkout errors");
    let (source, policy_id): (String, Uuid) = sqlx::query_as(
        "select source, escalation_policy_id from incidents where log_alert_id = $1")
        .bind(alert).fetch_one(&pool).await.unwrap();
    assert_eq!((source.as_str(), policy_id), ("log_alert", policy));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn foreign_policies_and_severities_are_refused_on_sources(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let foreign_policy: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1")
        .bind(other).fetch_one(&pool).await.unwrap();
    let foreign_severity = severity(&pool, other, "low").await;
    let own_policy: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1")
        .bind(project).fetch_one(&pool).await.unwrap();
    let own_severity = severity(&pool, project, "low").await;
    let (_, hb) = call(&app, "POST", &format!("/projects/{project}/heartbeats"), Some(&admin),
        Some(json!({"name": "job", "period_seconds": 60}))).await;
    let (_, la) = call(&app, "POST", &format!("/projects/{project}/log-alerts"), Some(&admin),
        Some(json!({"name": "errors", "threshold": 1, "comparison": "gt", "window_seconds": 300, "interval_seconds": 60}))).await;
    let items = [
        (format!("/projects/{project}/monitors/{monitor}"), format!("/projects/{project}/monitors")),
        (format!("/projects/{project}/heartbeats/{}", hb["id"].as_str().unwrap()), format!("/projects/{project}/heartbeats")),
        (format!("/projects/{project}/log-alerts/{}", la["id"].as_str().unwrap()), format!("/projects/{project}/log-alerts")),
    ];
    let creates = [
        json!({"name": "api 2", "check_type": "http", "url": "https://example.com/"}),
        json!({"name": "job 2", "period_seconds": 60}),
        json!({"name": "errors 2", "threshold": 1, "comparison": "gt", "window_seconds": 300, "interval_seconds": 60}),
    ];
    for ((item, list), create) in items.iter().zip(creates) {
        // The project's own policy and severity are taken, and shown back.
        let mut own = create.clone();
        own.as_object_mut().unwrap().extend([
            ("escalation_policy_id".to_string(), json!(own_policy)),
            ("severity_id".to_string(), json!(own_severity)),
        ]);
        let (status, created) = call(&app, "POST", list, Some(&admin), Some(own)).await;
        assert_eq!(status, StatusCode::CREATED, "POST {list}: {created}");
        assert_eq!((&created["escalation_policy_id"], &created["severity_id"]), (&json!(own_policy), &json!(own_severity)));
        let (status, cleared) = call(&app, "PATCH", item, Some(&admin), Some(json!({"severity_id": null}))).await;
        assert_eq!((status, &cleared["severity_id"]), (StatusCode::OK, &json!(null)), "PATCH {item}: {cleared}");
        for field in [json!({"escalation_policy_id": foreign_policy}), json!({"severity_id": foreign_severity})] {
            let (status, _) = call(&app, "PATCH", item, Some(&admin), Some(field.clone())).await;
            assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND, "PATCH {item} {field}: {status}");
            let mut body = create.clone();
            body.as_object_mut().unwrap().extend(field.as_object().unwrap().clone());
            let (status, _) = call(&app, "POST", list, Some(&admin), Some(body)).await;
            assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND, "POST {list} {field}: {status}");
        }
    }
}

#[sqlx::test]
async fn disabling_an_alerting_log_alert_resolves_and_tells_the_paged_why(pool: sqlx::PgPool) {
    let state = nami_engine::core::state::AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default",
        "steps": [{"delay_minutes": 0, "targets": [{"type": "user", "id": alice}]}]})).await;
    let alert: Uuid = sqlx::query_scalar(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds)
         values ($1, 'Checkout errors', 10, 'gt', 300, 60) returning id")
        .bind(project).fetch_one(&pool).await.unwrap();
    let due = alert::claim_due_alerts(&pool, 10, &[]).await.unwrap();
    let transition = alert::apply_evaluation(&pool, &due[0], 12, chrono::Utc::now()).await.unwrap().unwrap();
    uptime_alerts::notify_transition(&state, &transition).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1);

    let (status, body) = call(&app, "PATCH", &format!("/projects/{project}/log-alerts/{alert}"), Some(&admin),
        Some(json!({"enabled": false}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    common::run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail.len(), 2, "{mail:?}");
    assert_eq!(mail[1].0, "[Nami] Resolved: Log alert: Checkout errors");
    assert!(mail[1].1.contains("Reason: alert disabled"), "{}", mail[1].1);
    let (status, reason): (String, Option<String>) = sqlx::query_as(
        "select i.status, e.message from incidents i join incident_events e on e.incident_id = i.id and e.kind = 'resolved'
         where i.log_alert_id = $1").bind(alert).fetch_one(&pool).await.unwrap();
    assert_eq!((status.as_str(), reason.as_deref()), ("resolved", Some("alert disabled")));
}

/// R31 holds automatic opens only: a person who reopens a source's incident
/// inside its paging gap pages at once.
#[sqlx::test]
async fn a_manual_reopen_inside_the_paging_gap_pages_at_once(pool: sqlx::PgPool) {
    use nami_engine::incidents::model as incident;
    let state = nami_engine::core::state::AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let mut conn = pool.acquire().await.unwrap();
    let now = chrono::Utc::now();
    let id = incident::open_for_monitor(&mut conn, project, monitor, "api is down", None, now)
        .await.unwrap().expect("opened");
    common::run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&email, 1).await.len(), 1);
    incident::resolve_for_monitor(&mut conn, monitor, now).await.unwrap();

    let (status, body) = call(&app, "POST", &format!("/projects/{project}/incidents/{id}/reopen"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    common::run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 3).await;
    let subjects: Vec<&str> = mail.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, ["[Nami] [high] api is down", "[Nami] Resolved: api is down", "[Nami] [high] api is down"]);
}
