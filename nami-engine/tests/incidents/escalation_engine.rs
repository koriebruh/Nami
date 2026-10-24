// The escalation engine. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::{HeaderValue, StatusCode};
use chrono::{Duration, Utc};
use common::{
    call, insert_channel, make_escalation_due, run_incident_ticks, seed_http_monitor, seed_project, set_policy,
    settled_mail, test_app, user_with_role,
};
use nami_engine::{
    core::state::AppState,
    incidents::channel::model::ChannelConfig,
    incidents::escalation::run,
    incidents::lifecycle::{self, IncidentSource},
    incidents::model as incident,
};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

async fn open(app: &axum::Router, project: Uuid, member: &HeaderValue) -> String {
    let (status, body) =
        call(app, "POST", &format!("/projects/{project}/incidents"), Some(member), Some(json!({"title": "db down"})))
            .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    body["id"].as_str().unwrap().to_string()
}

async fn act(app: &axum::Router, project: Uuid, member: &HeaderValue, id: &str, action: &str) {
    let (status, body) =
        call(app, "POST", &format!("/projects/{project}/incidents/{id}/{action}"), Some(member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{action}: {body}");
}

async fn kinds(pool: &sqlx::PgPool, id: &str) -> Vec<String> {
    sqlx::query_scalar("select kind from incident_events where incident_id = $1::uuid order by id")
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn uid(id: &str) -> Uuid {
    id.parse().unwrap()
}

fn one_step(delay: i64, targets: Value) -> Value {
    json!({"delay_minutes": delay, "targets": targets})
}

/// The default policy (all members) emails once every member who may act on
/// the incident: project members and admins, and global admins whatever
/// their project role. Viewers, who cannot act, are left out, personal
/// channels included.
#[sqlx::test]
async fn the_default_policy_emails_every_member_who_may_act_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, alice, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, admin_email, _) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (viewer, viewer_email, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let global_admin_email = common::unique_email("global-admin");
    let global_admin = common::seed_user(&pool, &global_admin_email, "admin").await;
    common::add_member(&pool, project, global_admin, "viewer").await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&slack).await;
    let config = ChannelConfig::Slack { webhook_url: format!("{}/services/x", slack.uri()) };
    insert_channel(&pool, None, Some(viewer), "mine", config).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    for email in [&alice, &admin_email, &global_admin_email] {
        let mail = settled_mail(email, 1).await;
        assert_eq!(mail.len(), 1, "{email}");
        assert_eq!(mail[0].0, "[Nami] [high] db down");
    }
    assert!(settled_mail(&viewer_email, 0).await.is_empty(), "a viewer is not paged");
    assert!(slack.received_requests().await.unwrap().is_empty(), "nor through their personal channel");
    assert!(kinds(&pool, &id).await.contains(&"escalated".to_string()));
}

#[sqlx::test]
async fn a_later_step_waits_for_its_delay_and_an_acknowledge_stops_it(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "steps": [
            one_step(0, json!([{"type": "user", "id": alice}])),
            one_step(10, json!([{"type": "user", "id": bob}]))]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1);
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "step 2 waits 10 minutes");
    act(&app, project, &member, &id, "acknowledge").await;
    make_escalation_due(&pool, uid(&id)).await;
    run_incident_ticks(&state).await;
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "acknowledged: no step 2");
}

#[sqlx::test]
async fn an_acknowledge_cancels_queued_step_notifications(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (bob, bob_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]}))
        .await;
    let id = open(&app, project, &member).await;
    run::tick(&state).await.unwrap(); // queued, not yet sent
    act(&app, project, &member, &id, "acknowledge").await;
    run_incident_ticks(&state).await; // the drain finds it acknowledged
    let status: String = sqlx::query_scalar("select status from notification_deliveries where incident_id = $1::uuid")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(status, "cancelled");
    assert!(settled_mail(&bob_email, 0).await.is_empty());
}

#[sqlx::test]
async fn a_quick_reopen_never_sends_the_previous_generations_queued_page(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (bob, bob_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]}))
        .await;
    let id = open(&app, project, &member).await;
    run::tick(&state).await.unwrap(); // generation 0's page queued, not yet sent
    act(&app, project, &member, &id, "resolve").await;
    act(&app, project, &member, &id, "reopen").await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&bob_email, 1).await;
    assert_eq!(mail.len(), 1, "one page for the reopened incident, not two");
    let statuses: Vec<(String, String)> = sqlx::query_as(
        "select round, status from notification_deliveries where incident_id = $1::uuid order by round",
    )
    .bind(&id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        [("g0p0r0s0".to_string(), "cancelled".to_string()), ("g1p0r0s0".to_string(), "sent".to_string())]
    );
}

#[sqlx::test]
async fn a_schedule_target_pages_whoever_is_on_call_when_the_step_runs(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, schedule) = call(
        &app,
        "POST",
        &format!("/projects/{project}/schedules"),
        Some(&admin),
        Some(json!({
            "name": "primary", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
            "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice]})),
    )
    .await;
    let sid = schedule["id"].as_str().unwrap();
    call(
        &app,
        "POST",
        &format!("/projects/{project}/schedules/{sid}/overrides"),
        Some(&admin),
        Some(json!({
            "user_id": bob, "starts_at": Utc::now() - Duration::minutes(5), "ends_at": Utc::now() + Duration::hours(1)})),
    )
    .await;
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, json!([{"type": "schedule", "id": sid}]))]}))
        .await;
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&bob_email, 1).await.len(), 1, "the override is on call");
    assert!(settled_mail(&alice_email, 0).await.is_empty());
}

#[sqlx::test]
async fn a_user_target_also_uses_enabled_personal_channels_without_duplicates(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(1).mount(&slack).await;
    insert_channel(&pool, None, Some(alice), "phone", ChannelConfig::Slack { webhook_url: format!("{}/a", slack.uri()) })
        .await;
    let off =
        insert_channel(&pool, None, Some(alice), "old", ChannelConfig::Slack { webhook_url: format!("{}/b", slack.uri()) })
            .await;
    sqlx::query("update notification_channels set enabled = false where id = $1").bind(off).execute(&pool).await.unwrap();
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "steps": [one_step(0, json!([
            {"type": "user", "id": alice}, {"type": "all_members"}]))]}),
    )
    .await;
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1, "named twice, emailed once");
}

#[sqlx::test]
async fn a_pass_that_notifies_nobody_moves_to_the_fallback_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    // Alice is on call only in a window that is never now.
    let (_, schedule) = call(
        &app,
        "POST",
        &format!("/projects/{project}/schedules"),
        Some(&admin),
        Some(json!({
            "name": "nights", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
            "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice],
            "restrictions": [window_not_now()]})),
    )
    .await;
    let sid = schedule["id"].as_str().unwrap();
    let (_, fallback) = call(
        &app,
        "POST",
        &format!("/projects/{project}/escalation-policies"),
        Some(&admin),
        Some(json!({"name": "backup", "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]})),
    )
    .await;
    let fid = fallback["id"].as_str().unwrap();
    let main = set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "fallback_policy_id": fid,
            "steps": [one_step(0, json!([{"type": "schedule", "id": sid}]))]}),
    )
    .await;
    // The fallback points back at the main policy: it must not loop.
    call(
        &app,
        "PUT",
        &format!("/projects/{project}/escalation-policies/{fid}"),
        Some(&admin),
        Some(json!({
            "name": "backup", "fallback_policy_id": main, "steps": [one_step(0, json!([{"type": "user", "id": bob}]))]})),
    )
    .await;
    let id = open(&app, project, &member).await;
    for _ in 0..5 {
        make_escalation_due(&pool, uid(&id)).await;
        run_incident_ticks(&state).await;
    }
    assert_eq!(settled_mail(&bob_email, 1).await.len(), 1, "the fallback ran once");
    let k = kinds(&pool, &id).await;
    assert_eq!(k.iter().filter(|k| *k == "escalation_fallback").count(), 1, "{k:?}");
    assert_eq!(k.iter().filter(|k| *k == "escalation_exhausted").count(), 1, "{k:?}");
    let next: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("select next_escalation_at from incidents where id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(next, None, "nothing left to run");
}

/// A UTC window two to three hours from now: never covers the present.
fn window_not_now() -> Value {
    let from = (Utc::now() + Duration::hours(2)).format("%H:%M").to_string();
    let to = (Utc::now() + Duration::hours(3)).format("%H:%M").to_string();
    json!({"weekdays": [1, 2, 3, 4, 5, 6, 7], "from": from, "to": to})
}

#[sqlx::test]
async fn without_a_fallback_an_empty_pass_is_recorded_as_exhausted(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut condition = window_not_now();
    condition["timezone"] = json!("UTC");
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "steps": [
            {"delay_minutes": 0, "condition": condition, "targets": [{"type": "all_members"}]}]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    let k = kinds(&pool, &id).await;
    assert_eq!(&k[k.len() - 2..], ["step_skipped", "escalation_exhausted"], "{k:?}");
    let message: String = sqlx::query_scalar(
        "select message from incident_events where incident_id = $1::uuid and kind = 'escalation_exhausted'",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(message.contains("no fallback policy"), "{message}");
}

#[sqlx::test]
async fn repeats_wait_at_least_the_minimum_gap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "repeat_count": 3, "steps": [one_step(0, json!([{"type": "all_members"}]))]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    run::tick(&state).await.unwrap();
    let wait: f64 = sqlx::query_scalar(
        "select extract(epoch from next_escalation_at - started_at)::float8 from incidents where id = $1::uuid",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((299.0..=301.0).contains(&wait), "repeat after 5 minutes, got {wait}s");
}

#[sqlx::test]
async fn resolve_notifies_everyone_already_notified_once_and_reopen_restarts(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "steps": [
            one_step(0, json!([{"type": "user", "id": alice}])), one_step(30, json!([{"type": "user", "id": bob}]))]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    act(&app, project, &member, &id, "resolve").await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(
        mail.iter().map(|m| m.0.as_str()).collect::<Vec<_>>(),
        ["[Nami] [high] db down", "[Nami] Resolved: db down"]
    );
    assert!(settled_mail(&bob_email, 0).await.is_empty(), "never notified, so not told about the resolution");

    act(&app, project, &member, &id, "reopen").await;
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 3).await.len(), 3, "a reopened incident pages step 1 again");
}

#[sqlx::test]
async fn a_removed_member_is_not_notified_even_when_on_call(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, schedule) = call(
        &app,
        "POST",
        &format!("/projects/{project}/schedules"),
        Some(&admin),
        Some(json!({
            "name": "primary", "timezone": "UTC", "rotation": "weekly", "handoff_time": "00:00",
            "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": [alice]})),
    )
    .await;
    let sid = schedule["id"].as_str().unwrap();
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "steps": [one_step(0, json!([
            {"type": "schedule", "id": sid}, {"type": "user", "id": alice}]))]}),
    )
    .await;
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project)
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();
    open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    assert!(settled_mail(&alice_email, 0).await.is_empty());
}

#[sqlx::test]
async fn the_per_incident_delivery_cap_stops_a_storm(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("member")).await;
    let id = open(&app, project, &member).await;
    sqlx::query(
        "insert into notification_deliveries (incident_id, event, round, target_key, user_id, status, sent_at)
         select $1::uuid, 'triggered', 'old', 'email:' || g, (select user_id from project_members where project_id = $2 limit 1),
                'sent', now() from generate_series(1, 499) g",
    )
    .bind(&id)
    .bind(project)
    .execute(&pool)
    .await
    .unwrap();
    run::tick(&state).await.unwrap();
    let total: i64 = sqlx::query_scalar("select count(*) from notification_deliveries where incident_id = $1::uuid")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(total, run::MAX_DELIVERIES_PER_INCIDENT);
    let capped: i64 = sqlx::query_scalar(
        "select count(*) from incident_events where incident_id = $1::uuid and kind = 'notification_failed'",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(capped, 1);
}

/// The cap bounds step notifications only: resolved notices (an earlier
/// generation's here) neither count against it nor are stopped by it.
#[sqlx::test]
async fn the_per_incident_cap_counts_step_notifications_only(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let all = json!([{"type": "all_members"}]);
    set_policy(&app, project, &admin, json!({"name": "Default", "steps": [one_step(0, all.clone()), one_step(0, all)]}))
        .await;
    let id = open(&app, project, &member).await;
    // Out of the per-minute window, so the step's email is not deferred.
    sqlx::query(
        "insert into notification_deliveries (incident_id, event, round, target_key, user_id, status, sent_at)
         select $1::uuid, case when g < $3 then 'triggered' else 'resolved' end, 'old', 'email:' || g, $2,
                'sent', now() - interval '1 hour'
         from generate_series(1, $3 + 9) g",
    )
    .bind(&id)
    .bind(member_id)
    .bind(run::MAX_DELIVERIES_PER_INCIDENT as i32)
    .execute(&pool)
    .await
    .unwrap();
    run_incident_ticks(&state).await;
    let count = |event: &'static str| {
        let pool = pool.clone();
        let id = id.clone();
        async move {
            let n: i64 = sqlx::query_scalar(
                "select count(*) from notification_deliveries where incident_id = $1::uuid and event = $2",
            )
            .bind(&id)
            .bind(event)
            .fetch_one(&pool)
            .await
            .unwrap();
            n
        }
    };
    assert_eq!(count("triggered").await, run::MAX_DELIVERIES_PER_INCIDENT, "one more fit, the rest did not");
    assert!(kinds(&pool, &id).await.contains(&"notification_failed".to_string()), "the cap is on the timeline");

    let mut conn = pool.acquire().await.unwrap();
    lifecycle::resolve(&mut conn, uid(&id), None, Utc::now()).await.unwrap();
    assert_eq!(count("resolved").await, 10 + 1, "the page sent past the earlier notices is told");
}

async fn resolved_notices(pool: &sqlx::PgPool, incident: Uuid) -> Vec<String> {
    sqlx::query_scalar("select status from notification_deliveries where incident_id = $1 and event = 'resolved'")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// A reopen, by a person or by a heartbeat failing again, cancels the
/// resolved notices still queued, so an open incident's last message to
/// anyone is never "Resolved".
#[sqlx::test]
async fn a_reopen_cancels_the_resolved_notices_still_queued(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open(&app, project, &member).await;
    run_incident_ticks(&state).await;
    act(&app, project, &member, &id, "resolve").await;
    act(&app, project, &member, &id, "reopen").await;
    assert_eq!(resolved_notices(&pool, uid(&id)).await, ["cancelled", "cancelled"], "the member and the admin");
    run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 2).await;
    let subjects: Vec<&str> = mail.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(subjects, ["[Nami] [high] db down", "[Nami] [high] db down"]);

    let (status, body) = call(&app, "POST", &format!("/projects/{project}/heartbeats"), Some(&admin),
        Some(json!({"name": "nightly", "period_seconds": 60}))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let heartbeat: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let opened = incident::open_for_heartbeat(&mut conn, project, heartbeat, "nightly failed", None, Utc::now())
        .await.unwrap().expect("opened");
    run_incident_ticks(&state).await;
    incident::resolve_for_heartbeat(&mut conn, heartbeat, Utc::now()).await.unwrap();
    let reopened = incident::open_for_heartbeat(&mut conn, project, heartbeat, "nightly failed", None, Utc::now())
        .await.unwrap();
    assert_eq!(reopened, Some(opened), "reopened inside the heartbeat's window");
    assert_eq!(resolved_notices(&pool, opened).await, ["cancelled", "cancelled"]);
}

#[sqlx::test]
async fn racing_ticks_notify_each_target_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    user_with_role(&app, &pool, project, Some("member")).await;
    let id = open(&app, project, &member).await;
    let (a, b) = tokio::join!(run::tick(&state), run::tick(&state));
    a.unwrap();
    b.unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from notification_deliveries where incident_id = $1::uuid")
        .bind(&id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 2, "two members, one delivery each");
}

#[sqlx::test]
async fn an_outage_never_replays_missed_steps_as_a_burst(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "repeat_count": 10, "steps": [one_step(0, json!([{"type": "all_members"}]))]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    // The worker was down for three hours: the first step is long overdue.
    sqlx::query("update incidents set next_escalation_at = now() - interval '3 hours' where id = $1::uuid")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&email, 1).await.len(), 1, "one pass now, not one per missed repeat");
    let wait: f64 =
        sqlx::query_scalar("select extract(epoch from next_escalation_at - now())::float8 from incidents where id = $1::uuid")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!((180.0..=301.0).contains(&wait), "the next repeat is ~5 min from now, got {wait}s");
}

#[sqlx::test]
async fn a_resolution_with_a_reason_notifies_the_paged_with_it(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let mut conn = pool.acquire().await.unwrap();
    let id = nami_engine::incidents::model::open_for_monitor(&mut conn, project, monitor, "api is down", None, Utc::now())
        .await
        .unwrap()
        .unwrap();
    run_incident_ticks(&state).await;
    assert_eq!(settled_mail(&alice_email, 1).await.len(), 1);

    let resolved =
        lifecycle::resolve_source_with_reason(&mut conn, &IncidentSource::Monitor(monitor), Utc::now(), "alert disabled")
            .await
            .unwrap();
    assert_eq!(resolved, Some(id));
    let queued: Vec<(String, String)> =
        sqlx::query_as("select round, target_key from notification_deliveries where incident_id = $1 and event = 'resolved'")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(queued, [("resolved:g0".to_string(), format!("email:{alice}"))]);
    // Before the drain adds its `notification_sent` events.
    let last: (String, Option<String>, Value) =
        sqlx::query_as("select kind, message, metadata from incident_events where incident_id = $1 order by id desc limit 1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(last, ("resolved".into(), Some("alert disabled".into()), json!({"reason": "alert disabled"})));

    run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 2).await;
    assert_eq!(mail.len(), 2);
    assert_eq!(mail[1].0, "[Nami] Resolved: api is down");
    assert!(mail[1].1.contains("Reason: alert disabled"), "{}", mail[1].1);
}

#[sqlx::test]
async fn a_legacy_incident_tells_every_verified_member_about_its_resolution(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, _, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (carol, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    sqlx::query("update users set email_verified_at = null where id = $1").bind(carol).execute(&pool).await.unwrap();
    let id = open(&app, project, &member).await;
    // As the deploy left an incident the old email path had alerted for (R6).
    sqlx::query("update incidents set escalation_legacy = true, next_escalation_at = null where id = $1::uuid")
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    act(&app, project, &member, &id, "resolve").await;
    let mut queued: Vec<String> = sqlx::query_scalar(
        "select target_key from notification_deliveries where incident_id = $1::uuid and round = 'resolved:g0'",
    )
    .bind(&id)
    .fetch_all(&pool)
    .await
    .unwrap();
    queued.sort();
    let mut expected = vec![format!("email:{alice}"), format!("email:{bob}")];
    expected.sort();
    assert_eq!(queued, expected);
}

#[sqlx::test]
async fn one_tick_runs_at_most_the_step_limit_of_one_incident(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut condition = window_not_now();
    condition["timezone"] = json!("UTC");
    let skipped = json!({"delay_minutes": 0, "condition": condition, "targets": [{"type": "all_members"}]});
    let (_, fallback) = call(
        &app,
        "POST",
        &format!("/projects/{project}/escalation-policies"),
        Some(&admin),
        Some(json!({"name": "backup", "steps": [one_step(0, json!([{"type": "all_members"}]))]})),
    )
    .await;
    set_policy(
        &app,
        project,
        &admin,
        json!({"name": "Default", "fallback_policy_id": fallback["id"],
            "steps": vec![skipped; run::MAX_STEPS_PER_TICK]}),
    )
    .await;
    let id = open(&app, project, &member).await;
    assert_eq!(run::tick(&state).await.unwrap(), run::MAX_STEPS_PER_TICK);
    let k = kinds(&pool, &id).await;
    assert_eq!(k.iter().filter(|k| *k == "step_skipped").count(), run::MAX_STEPS_PER_TICK);
    assert!(!k.contains(&"escalated".to_string()), "the fallback step waits for the next tick: {k:?}");
    assert_eq!(run::tick(&state).await.unwrap(), 1);
    assert!(kinds(&pool, &id).await.contains(&"escalated".to_string()));
}

#[sqlx::test]
async fn one_tick_escalates_at_most_its_batch_of_incidents(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    for _ in 0..=run::ESCALATION_BATCH {
        common::open_test_incident(&pool, project, member).await;
    }
    assert_eq!(run::tick(&state).await.unwrap(), run::ESCALATION_BATCH, "one step each");
    let due: i64 = sqlx::query_scalar("select count(*) from incidents where next_escalation_at <= now()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(due, 1, "the last one waits for the next tick");
    assert_eq!(run::tick(&state).await.unwrap(), 1);
}
