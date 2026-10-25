// The notification queue and its senders. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::StatusCode;
use common::{
    call, insert_channel, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app,
    user_with_role,
};
use chrono::Utc;
use nami_engine::{
    core::state::AppState,
    incidents::channel::model::ChannelConfig,
    incidents::deliveries::{self, DeliveryEvent, NewDelivery, Target},
    incidents::{lifecycle, outbound::OUTBOUND_TIMEOUT},
};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header_exists, method, path},
};

/// Queues one delivery and disarms the incident's escalation, so `kick`
/// sends only what this file queues (the default policy would also page the
/// member's email and personal channels).
async fn queue(pool: &sqlx::PgPool, incident: Uuid, round: &str, target: Target) -> u64 {
    sqlx::query("update incidents set next_escalation_at = null where id = $1")
        .bind(incident)
        .execute(pool)
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    deliveries::enqueue(
        &mut conn,
        &[NewDelivery { incident_id: incident, event: DeliveryEvent::Triggered, round: round.into(), target }],
    )
    .await
    .unwrap()
}

// These helpers look only at the rows and events this file queues itself
// (round "p0r0s0", channel messages).
async fn delivery(pool: &sqlx::PgPool, incident: Uuid) -> (String, i32, Option<String>) {
    sqlx::query_as(
        "select status, attempts, last_error from notification_deliveries where incident_id = $1 and round = 'p0r0s0'",
    )
    .bind(incident)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn timeline(pool: &sqlx::PgPool, incident: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "select kind, message from incident_events where incident_id = $1 and kind like 'notification_%'
                    and message not like 'Email:%' order by id",
    )
    .bind(incident)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Makes every pending/sending delivery due now (skips the backoff wait).
async fn make_due(pool: &sqlx::PgPool) {
    sqlx::query(
        "update notification_deliveries set next_attempt_at = now(), locked_until = null
                 where status in ('pending', 'sending')",
    )
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn a_slack_delivery_is_sent_once_and_recorded(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let app = test_app(pool.clone());
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/services/T/B/x"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&slack)
        .await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/T/B/x", slack.uri()) },
    )
    .await;

    assert_eq!(queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await, 1);
    assert_eq!(
        queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await,
        0,
        "deduplicated"
    );
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("sent".into(), 1, None));
    assert_eq!(timeline(&pool, incident).await, [("notification_sent".into(), Some("Slack: room".into()))]);
    let body: Value = serde_json::from_slice(&slack.received_requests().await.unwrap()[0].body).unwrap();
    assert!(body["text"].as_str().unwrap().contains("[high] test incident"));
}

#[sqlx::test]
async fn a_failing_receiver_is_retried_with_backoff_then_marked_failed(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_string("token=SECRET"))
        .mount(&hook)
        .await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "hook",
        ChannelConfig::Webhook { url: format!("{}/in?key=SECRET", hook.uri()) },
    )
    .await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;

    run_incident_ticks(&state).await;
    let (status, attempts, error) = delivery(&pool, incident).await;
    assert_eq!((status.as_str(), attempts, error.as_deref()), ("pending", 1, Some("HTTP 503")));
    let wait: f64 = sqlx::query_scalar(
        "select extract(epoch from next_attempt_at - now())::float8 from notification_deliveries where round = 'p0r0s0'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!((25.0..=31.0).contains(&wait), "first retry after ~30 s, got {wait}");
    for _ in 1..deliveries::MAX_ATTEMPTS {
        make_due(&pool).await;
        run_incident_ticks(&state).await;
    }
    assert_eq!(delivery(&pool, incident).await.0, "failed");
    let attempts = deliveries::MAX_ATTEMPTS as usize;
    assert_eq!(hook.received_requests().await.unwrap().len(), attempts);
    let events = timeline(&pool, incident).await;
    assert_eq!(events.len(), attempts, "one event per attempt");
    for (_, message) in &events {
        let message = message.as_deref().unwrap();
        assert!(!message.contains("SECRET") && !message.contains("127.0.0.1"), "{message}");
    }
}

#[sqlx::test]
async fn a_rejected_request_is_not_retried(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let discord = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(404)).mount(&discord).await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "d",
        ChannelConfig::Discord { webhook_url: format!("{}/api/webhooks/1/x", discord.uri()) },
    )
    .await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("failed".into(), 1, Some("HTTP 404".into())));
}

#[sqlx::test]
async fn webhooks_are_signed_with_a_timestamp_and_carry_the_delivery_id(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("x-nami-signature"))
        .and(header_exists("x-nami-timestamp"))
        .and(header_exists("x-nami-delivery"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&hook)
        .await;
    let channel = insert_channel(&pool, Some(project), None, "hook", ChannelConfig::Webhook { url: hook.uri() }).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;

    let request = &hook.received_requests().await.unwrap()[0];
    let sealed: Vec<u8> = sqlx::query_scalar("select signing_secret_enc from notification_channels where id = $1")
        .bind(channel)
        .fetch_one(&pool)
        .await
        .unwrap();
    let secret = state
        .cipher
        .decrypt(&nami_engine::incidents::channel::model::signing_secret_context(channel), &sealed)
        .unwrap();
    let timestamp: i64 = request.headers.get("x-nami-timestamp").unwrap().to_str().unwrap().parse().unwrap();
    assert!((chrono::Utc::now().timestamp() - timestamp).abs() <= 60, "a fresh unix-seconds timestamp");
    let expected = nami_engine::incidents::format::signature(&secret, timestamp, &request.body);
    assert_eq!(request.headers.get("x-nami-signature").unwrap().to_str().unwrap(), expected);
    // What a receiver does (R29): HMAC over "{timestamp}.{raw body}".
    let mut signed = format!("{timestamp}.").into_bytes();
    signed.extend_from_slice(&request.body);
    assert_eq!(expected, format!("sha256={}", nami_engine::incidents::format::hmac_hex(&secret, &signed)));
    let delivery_id: Uuid = sqlx::query_scalar("select id from notification_deliveries where round = 'p0r0s0'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(request.headers.get("x-nami-delivery").unwrap().to_str().unwrap(), delivery_id.to_string());
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["event"], "incident.triggered");
    assert_eq!(body["incident"]["id"], json!(incident));
}

#[sqlx::test]
async fn telegram_goes_to_the_configured_api_and_never_logs_the_token(pool: sqlx::PgPool) {
    let telegram = MockServer::start().await;
    let token = format!("123456:{}", "T".repeat(35));
    Mock::given(method("POST"))
        .and(path(format!("/bot{token}/sendMessage")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&telegram)
        .await;
    let mut state = AppState::for_test(pool.clone());
    state.config.telegram_api_base_url = telegram.uri();
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let channel = insert_channel(
        &pool,
        None,
        Some(user),
        "phone",
        ChannelConfig::Telegram { bot_token: token.clone(), chat_id: "42".into() },
    )
    .await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: Some(user) }).await;
    let logs = common::LogCapture::default();
    let _guard = logs.install();
    run_incident_ticks(&state).await;
    assert_eq!(telegram.received_requests().await.unwrap().len(), 1);
    assert!(!logs.lines().join("\n").contains(&"T".repeat(35)), "the bot token never reaches a log line");
    assert!(!timeline(&pool, incident).await.iter().any(|(_, m)| m.as_deref().unwrap_or("").contains("TTTT")));
}

#[sqlx::test]
async fn implicit_email_goes_to_the_user_through_the_mailer(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&email, 1).await;
    assert_eq!(mail[0].0, "[Nami] [high] test incident");
}

#[sqlx::test]
async fn a_channel_never_receives_more_than_its_per_minute_cap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&slack).await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", slack.uri()) },
    )
    .await;
    for _ in 0..(deliveries::CHANNEL_PER_MINUTE + 10) {
        let incident = open_test_incident(&pool, project, user).await;
        queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    }
    for _ in 0..3 {
        run_incident_ticks(&state).await;
    }
    assert_eq!(slack.received_requests().await.unwrap().len() as i64, deliveries::CHANNEL_PER_MINUTE);
    let (pending, attempts): (i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(attempts), 0) from notification_deliveries where status = 'pending' and channel_id = $1",
    )
    .bind(channel)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((pending, attempts), (10, 0), "the excess waits without spending an attempt");
}

#[sqlx::test]
async fn a_user_never_receives_more_emails_than_the_per_minute_cap(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    for _ in 0..(deliveries::EMAIL_PER_USER_PER_MINUTE + 5) {
        let incident = open_test_incident(&pool, project, user).await;
        queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    }
    run_incident_ticks(&state).await;
    run_incident_ticks(&state).await;
    let counts: Vec<(String, i64, i64)> = sqlx::query_as(
        "select status, count(*), sum(attempts) from notification_deliveries where user_id = $1
         group by status order by status",
    )
    .bind(user)
    .fetch_all(&pool)
    .await
    .unwrap();
    let cap = deliveries::EMAIL_PER_USER_PER_MINUTE;
    assert_eq!(counts, [("pending".into(), 5, 0), ("sent".into(), cap, cap)], "the excess waits");
    assert_eq!(settled_mail(&email, cap as usize).await.len() as i64, cap);
}

#[sqlx::test]
async fn a_delivery_whose_channel_no_longer_decrypts_fails_without_sending(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "room",
        ChannelConfig::Slack { webhook_url: "http://127.0.0.1:9/x".into() },
    )
    .await;
    sqlx::query("update notification_channels set config_enc = '\\x00'::bytea where id = $1")
        .bind(channel)
        .execute(&pool)
        .await
        .unwrap();
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    run_incident_ticks(&state).await;
    let (status, _, error) = delivery(&pool, incident).await;
    assert_eq!(status, "failed");
    assert!(error.unwrap().contains("could not be decrypted"));
}

#[sqlx::test]
async fn send_test_reports_the_outcome_and_is_rate_limited(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let teams = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(202)).expect(2).mount(&teams).await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "teams",
        ChannelConfig::Teams { webhook_url: format!("{}/workflows/x", teams.uri()) },
    )
    .await;
    let test_path = format!("/projects/{project}/notification-channels/{channel}/test");

    let (status, _) = call(&app, "POST", &test_path, Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(&app, "POST", &test_path, Some(&admin), Some(json!({}))).await;
    assert_eq!((status, body["ok"].as_bool()), (StatusCode::OK, Some(true)), "{body}");
    let (status, _) = call(&app, "POST", &test_path, Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("update notification_channels set last_tested_at = now() - make_interval(secs => $2) where id = $1")
        .bind(channel)
        .bind(nami_engine::incidents::channel::model::MIN_TEST_INTERVAL.as_secs_f64() + 1.0)
        .execute(&pool)
        .await
        .unwrap();
    let (status, _) = call(&app, "POST", &test_path, Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK, "a test past the interval goes out again");
    let card: Value = serde_json::from_slice(&teams.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(card["attachments"][0]["content"]["type"], "AdaptiveCard");
}

#[sqlx::test]
async fn a_failed_personal_test_answers_502_with_the_error_kind_only(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, cookie) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, other) = user_with_role(&app, &pool, project, Some("admin")).await;
    let hook = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(404).set_body_string("SECRET")).mount(&hook).await;
    let channel = insert_channel(
        &pool,
        None,
        Some(user),
        "mine",
        ChannelConfig::Webhook { url: format!("{}/in?key=SECRET", hook.uri()) },
    )
    .await;
    let test_path = format!("/account/notification-channels/{channel}/test");

    let (status, _) = call(&app, "POST", &test_path, Some(&other), Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "another user's channel");
    let (status, body) = call(&app, "POST", &test_path, Some(&cookie), Some(json!({}))).await;
    assert_eq!((status, body), (StatusCode::BAD_GATEWAY, json!({"error": "HTTP 404"})));
    let sent: Value = serde_json::from_slice(&hook.received_requests().await.unwrap()[0].body).unwrap();
    assert_eq!(sent["event"], "test");
}

#[sqlx::test]
async fn the_strict_policy_refuses_internal_and_off_allowlist_destinations_at_send_time(pool: sqlx::PgPool) {
    let mut state = AppState::for_test(pool.clone());
    state.config.outbound_policy = nami_engine::incidents::outbound::OutboundPolicy::Strict;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let receiver = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&receiver).await;
    // A personal webhook at 127.0.0.1 (public-only) and a Slack channel off hooks.slack.com: both refused.
    let personal = insert_channel(&pool, None, Some(user), "mine", ChannelConfig::Webhook { url: receiver.uri() }).await;
    let slack = insert_channel(
        &pool,
        Some(project),
        None,
        "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", receiver.uri()) },
    )
    .await;
    // A project webhook may reach a private receiver (R9).
    let project_hook =
        insert_channel(&pool, Some(project), None, "hook", ChannelConfig::Webhook { url: receiver.uri() }).await;
    for (round, channel, owner) in
        [("p0r0s0", personal, Some(user)), ("p0r0s1", slack, None), ("p0r0s2", project_hook, None)]
    {
        queue(&pool, incident, round, Target::Channel { channel_id: channel, user_id: owner }).await;
    }
    run_incident_ticks(&state).await;
    let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "select round, status, last_error from notification_deliveries where incident_id = $1 and round like 'p0r0s%' order by round",
    )
    .bind(incident)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("p0r0s0".into(), "failed".into(), Some("destination not allowed".into())),
            ("p0r0s1".into(), "failed".into(), Some("destination not allowed".into())),
            ("p0r0s2".into(), "sent".into(), None),
        ]
    );
    assert_eq!(receiver.received_requests().await.unwrap().len(), 1, "only the project webhook was contacted");
}

#[sqlx::test]
async fn a_lease_that_expired_after_the_last_attempt_fails_the_row_instead_of_wedging_the_queue(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    // A crash mid-send of attempt 5 leaves this behind.
    sqlx::query(
        "insert into notification_deliveries (incident_id, event, round, target_key, user_id, status, attempts, locked_until)
                 values ($1, 'triggered', 'stuck', 'email:x', $2, 'sending', 5, now() - interval '1 minute')",
    )
    .bind(incident)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    run_incident_ticks(&state).await;
    let stuck: (String, Option<String>) =
        sqlx::query_as("select status, last_error from notification_deliveries where round = 'stuck'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stuck, ("failed".into(), Some("delivery interrupted".into())));
    assert_eq!(delivery(&pool, incident).await.0, "sent", "the rest of the queue still drains");
    assert!(!settled_mail(&email, 1).await.is_empty());
}

#[sqlx::test]
async fn a_step_notification_is_cancelled_once_the_incident_is_not_open_or_the_user_left(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (left, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&slack).await;
    let channel = insert_channel(
        &pool,
        Some(project),
        None,
        "room",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", slack.uri()) },
    )
    .await;
    let acknowledged = open_test_incident(&pool, project, user).await;
    sqlx::query("update incidents set status = 'acknowledged', acknowledged_at = now() where id = $1")
        .bind(acknowledged)
        .execute(&pool)
        .await
        .unwrap();
    queue(&pool, acknowledged, "p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    let open = open_test_incident(&pool, project, user).await;
    sqlx::query("delete from project_members where user_id = $1").bind(left).execute(&pool).await.unwrap();
    queue(&pool, open, "p0r0s0", Target::Email { user_id: left }).await;

    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, acknowledged).await.0, "cancelled");
    assert_eq!(delivery(&pool, open).await.0, "cancelled");
}

#[sqlx::test]
async fn a_disabled_incidents_worker_sends_nothing(pool: sqlx::PgPool) {
    let mut state = AppState::for_test(pool.clone());
    state.config.incidents_worker_enabled = false;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    queue(&pool, incident, "p0r0s0", Target::Email { user_id: user }).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("pending".into(), 0, None));
}

#[sqlx::test]
async fn a_name_refused_at_connect_time_fails_at_once_without_a_retry(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let receiver = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&receiver).await;
    // A name, not a literal: the send-time URL check passes it, and the
    // personal webhook's PublicOnlyResolver refuses it when connecting.
    let port = receiver.address().port();
    let personal = insert_channel(
        &pool,
        None,
        Some(user),
        "mine",
        ChannelConfig::Webhook { url: format!("http://localhost:{port}/in") },
    )
    .await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: personal, user_id: Some(user) }).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await, ("failed".into(), 1, Some("destination not allowed".into())));
    assert_eq!(
        timeline(&pool, incident).await,
        [("notification_failed".into(), Some("Webhook: mine: destination not allowed".into()))]
    );
    assert!(receiver.received_requests().await.unwrap().is_empty());
}

#[sqlx::test]
async fn one_drain_claims_at_most_its_batch(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let incident = open_test_incident(&pool, project, user).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&slack).await;
    // Two channels, so the per-channel cap does not bind before the batch.
    let mut channels = Vec::new();
    for name in ["a", "b"] {
        let config = ChannelConfig::Slack { webhook_url: format!("{}/services/{name}", slack.uri()) };
        channels.push(insert_channel(&pool, Some(project), None, name, config).await);
    }
    for n in 0..=deliveries::DRAIN_BATCH {
        let channel_id = channels[n as usize % 2];
        queue(&pool, incident, &format!("batch{n}"), Target::Channel { channel_id, user_id: None }).await;
    }
    // Older than anything else the drain may find, so these are claimed first.
    sqlx::query("update notification_deliveries set next_attempt_at = now() - interval '1 hour' where round like 'batch%'")
        .execute(&pool)
        .await
        .unwrap();
    run_incident_ticks(&state).await;
    assert_eq!(slack.received_requests().await.unwrap().len() as i64, deliveries::DRAIN_BATCH);
    let left: (i64, i64) = sqlx::query_as(
        "select count(*), coalesce(sum(attempts), 0) from notification_deliveries
         where round like 'batch%' and status = 'pending'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, (1, 0), "the one past the batch waits for the next drain, unattempted");
}

#[sqlx::test]
async fn a_personal_channel_of_someone_who_left_the_project_is_not_sent(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (left, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&slack).await;
    let personal = insert_channel(
        &pool,
        None,
        Some(left),
        "phone",
        ChannelConfig::Slack { webhook_url: format!("{}/services/x", slack.uri()) },
    )
    .await;
    let incident = open_test_incident(&pool, project, user).await;
    sqlx::query("delete from project_members where user_id = $1").bind(left).execute(&pool).await.unwrap();
    // The owner is found from the channel even when the row names no user.
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: personal, user_id: None }).await;
    run_incident_ticks(&state).await;
    assert_eq!(delivery(&pool, incident).await.0, "cancelled");
}

/// Polls `ready` every 50 ms until it holds or `limit` passes; whether it held.
async fn wait_until<F, Fut>(limit: Duration, mut ready: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let started = Instant::now();
    while started.elapsed() < limit {
        if ready().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn sending_to(pool: &sqlx::PgPool, channel: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from notification_deliveries where channel_id = $1 and status = 'sending'")
        .bind(channel)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A receiver that hangs holds its own send slots until its timeout, never
/// the queue: with as many of its sends in flight as its cap allows, the
/// worker's next drain still sends to another channel.
#[sqlx::test]
async fn a_hanging_receiver_does_not_hold_up_another_channel(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (port, _silent) = common::silent_server().await;
    let config = ChannelConfig::Slack { webhook_url: format!("http://127.0.0.1:{port}/services/x") };
    let hung = insert_channel(&pool, Some(project), None, "hung", config).await;
    let fast = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&fast).await;
    let config = ChannelConfig::Slack { webhook_url: format!("{}/services/y", fast.uri()) };
    let other = insert_channel(&pool, Some(project), None, "other", config).await;
    let incident = open_test_incident(&pool, project, user).await;
    for n in 0..deliveries::CHANNEL_PER_MINUTE {
        queue(&pool, incident, &format!("hung{n}"), Target::Channel { channel_id: hung, user_id: None }).await;
    }
    // The worker loop: one kick after another, never two at once.
    let worker = tokio::spawn({
        let state = state.clone();
        async move {
            loop {
                run_incident_ticks(&state).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    });
    let all_in_flight =
        wait_until(Duration::from_secs(5), || async { sending_to(&pool, hung).await == deliveries::CHANNEL_PER_MINUTE })
            .await;
    assert!(all_in_flight, "the hanging channel's deliveries were claimed");

    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: other, user_id: None }).await;
    let queued = Instant::now();
    let sent = wait_until(OUTBOUND_TIMEOUT * 2, || async { !fast.received_requests().await.unwrap().is_empty() }).await;
    let waited = queued.elapsed();
    worker.abort();
    assert!(sent && waited < OUTBOUND_TIMEOUT, "the other channel waited {waited:?} (sent: {sent})");
    assert_eq!(sending_to(&pool, hung).await, deliveries::CHANNEL_PER_MINUTE, "still hanging meanwhile");
}

/// A page still in flight when its incident is resolved is followed by its
/// resolved notice once it lands, like the pages the resolve saw sent.
#[sqlx::test]
async fn a_page_in_flight_at_resolve_time_gets_its_resolved_notice(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(2)))
        .mount(&slow)
        .await;
    let config = ChannelConfig::Slack { webhook_url: format!("{}/services/x", slow.uri()) };
    let channel = insert_channel(&pool, Some(project), None, "room", config).await;
    let incident = open_test_incident(&pool, project, user).await;
    // A step notification of the incident's current generation (0).
    queue(&pool, incident, "g0p0r0s0", Target::Channel { channel_id: channel, user_id: None }).await;
    let kick = tokio::spawn({
        let state = state.clone();
        async move { run_incident_ticks(&state).await }
    });
    let requested = wait_until(Duration::from_secs(5), || async { !slow.received_requests().await.unwrap().is_empty() }).await;
    assert!(requested, "the page is on its way");
    let mut conn = pool.acquire().await.unwrap();
    assert!(lifecycle::resolve(&mut conn, incident, None, Utc::now()).await.unwrap());
    kick.await.unwrap();

    let notices = || async {
        let rows: Vec<(String, String)> = sqlx::query_as(
            "select round, status from notification_deliveries where incident_id = $1 and event = 'resolved'",
        )
        .bind(incident)
        .fetch_all(&pool)
        .await
        .unwrap();
        rows
    };
    assert_eq!(notices().await, [("resolved:g0".to_string(), "pending".to_string())]);
    run_incident_ticks(&state).await;
    assert_eq!(notices().await, [("resolved:g0".to_string(), "sent".to_string())]);
    assert_eq!(slow.received_requests().await.unwrap().len(), 2);
}

/// A drain claims a row only for a free send slot: with every one of the
/// instance's slots held by a hanging send, the next due row is not claimed
/// (no attempt spent, no lease taken) until a slot frees up.
#[sqlx::test]
async fn a_row_past_the_free_send_slots_is_not_claimed(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (port, _silent) = common::silent_server().await;
    let incident = open_test_incident(&pool, project, user).await;
    // Two hanging channels, so neither reaches its per-minute cap first.
    let cap = deliveries::CHANNEL_PER_MINUTE as usize;
    let per_channel = [cap, deliveries::MAX_SENDS_IN_FLIGHT - cap];
    for (name, rows) in ["a", "b"].into_iter().zip(per_channel) {
        let config = ChannelConfig::Slack { webhook_url: format!("http://127.0.0.1:{port}/services/{name}") };
        let channel = insert_channel(&pool, Some(project), None, name, config).await;
        for n in 0..rows {
            queue(&pool, incident, &format!("hung-{name}{n}"), Target::Channel { channel_id: channel, user_id: None }).await;
        }
    }
    sqlx::query("update notification_deliveries set next_attempt_at = now() - interval '1 hour' where round like 'hung-%'")
        .execute(&pool)
        .await
        .unwrap();
    let fast = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&fast).await;
    let config = ChannelConfig::Slack { webhook_url: format!("{}/services/c", fast.uri()) };
    let next = insert_channel(&pool, Some(project), None, "c", config).await;
    queue(&pool, incident, "p0r0s0", Target::Channel { channel_id: next, user_id: None }).await;

    run_incident_ticks(&state).await;
    let in_flight: i64 = sqlx::query_scalar(
        "select count(*) from notification_deliveries where round like 'hung-%' and status = 'sending'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(in_flight, deliveries::MAX_SENDS_IN_FLIGHT as i64, "every slot is held");
    let (status, attempts, _) = delivery(&pool, incident).await;
    assert_eq!((status.as_str(), attempts), ("pending", 0), "the one past the slots waits, unattempted");
    assert!(fast.received_requests().await.unwrap().is_empty());
}
