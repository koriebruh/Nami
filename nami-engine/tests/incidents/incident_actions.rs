// Acknowledge/resolve links. Requires `docker compose up -d postgres mailpit`.
use crate::common;

use axum::http::StatusCode;
use chrono::Utc;
use common::{call, open_test_incident, run_incident_ticks, seed_project, settled_mail, test_app, user_with_role};
use nami_engine::{
    core::state::AppState,
    incidents::action::{
        self as incident_action,
        routes::BURST,
        token::{self as action_token, Action, Claims},
    },
};
use serde_json::json;
use uuid::Uuid;

fn token(state: &AppState, incident: Uuid, user: Uuid, action: Action) -> String {
    action_token::sign(&state.cipher, &Claims { incident_id: incident, action, user_id: user,
        expires_at: Utc::now() + action_token::TOKEN_TTL })
}

#[sqlx::test]
#[ignore = "needs NAMI-INC-9 (escalation emails) and the deliveries.rs link wiring of NAMI-INC-11"]
async fn emails_carry_links_that_acknowledge_without_a_session_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    run_incident_ticks(&state).await;
    let mail = settled_mail(&alice_email, 1).await;
    let ack_line = mail[0].1.lines().find(|l| l.starts_with("Acknowledge: ")).expect("ack link").to_string();
    let link = ack_line.trim_start_matches("Acknowledge: ");
    assert!(link.starts_with(&format!("{}/incidents/action?token=", state.config.ui_base_url.trim_end_matches('/'))));
    let token = link.split("token=").nth(1).unwrap();

    let (status, inspected) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!((inspected["action"].as_str(), inspected["incident"]["status"].as_str()), (Some("acknowledge"), Some("open")));
    let status_after_inspect: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(status_after_inspect, "open", "inspect never mutates");

    let (status, done) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!((status, done["status"].as_str()), (StatusCode::OK, Some("acknowledged")), "{done}");
    let by: Option<Uuid> = sqlx::query_scalar("select acknowledged_by from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(by, Some(alice));
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::GONE);
}

#[sqlx::test]
#[ignore = "needs NAMI-INC-9 (escalation emails) and the deliveries.rs link wiring of NAMI-INC-11"]
async fn viewers_get_no_links_and_their_tokens_do_nothing(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (viewer, viewer_email, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open_test_incident(&pool, project, member).await;
    run_incident_ticks(&state).await;
    assert!(!settled_mail(&viewer_email, 1).await[0].1.contains("Acknowledge:"));
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None,
        Some(json!({"token": token(&state, id, viewer, Action::Resolve)}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// The no-email half of the two tests above, until NAMI-INC-9 queues emails:
/// a link built as an email would carry it inspects without mutating,
/// acknowledges once as its user, and a replay is refused.
#[sqlx::test]
async fn a_link_acknowledges_without_a_session_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let links = action_token::links(&state.cipher, &state.config.ui_base_url, id, alice, Utc::now());
    let prefix = format!("{}/incidents/action?token=", state.config.ui_base_url.trim_end_matches('/'));
    let token = links.acknowledge.strip_prefix(&prefix).expect("a link under UI_BASE_URL");

    let (status, inspected) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::OK, "{inspected}");
    assert_eq!((inspected["action"].as_str(), inspected["incident"]["status"].as_str()), (Some("acknowledge"), Some("open")));
    assert_eq!(inspected["incident"]["id"], json!(id));
    assert_eq!(inspected["incident"]["project_id"], json!(project));
    assert_eq!(inspected["incident"]["title"], "test incident");
    assert_eq!(inspected["incident"]["severity"], "high");
    assert_eq!(inspected["user_email"], json!(alice_email));
    let (status, _) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::OK, "inspecting twice is fine");
    let status_after_inspect: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(status_after_inspect, "open", "inspect never mutates");

    let (status, done) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!((status, done["status"].as_str()), (StatusCode::OK, Some("acknowledged")), "{done}");
    let by: Option<Uuid> = sqlx::query_scalar("select acknowledged_by from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(by, Some(alice));
    let (status, body) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": token}))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::GONE, Some("this link was already used")));
    let (status, _) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": token}))).await;
    assert_eq!(status, StatusCode::GONE, "a used link says so when it is opened again");
    let stored: Vec<u8> = sqlx::query_scalar("select token_hash from incident_action_token_uses where incident_id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(stored, action_token::hash(token), "only the hash is kept");
}

#[sqlx::test]
async fn a_viewer_token_does_nothing(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (viewer, _, _) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open_test_incident(&pool, project, member).await;
    let t = token(&state, id, viewer, Action::Resolve);
    for path in ["/incident-actions/inspect", "/incident-actions/confirm"] {
        let (status, body) = call(&app, "POST", path, None, Some(json!({"token": t}))).await;
        assert_eq!(
            (status, body["error"].as_str()),
            (StatusCode::FORBIDDEN, Some("you no longer have access to this incident")),
            "{path}"
        );
    }
    let still: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(still, "open");
}

#[sqlx::test]
async fn a_global_admin_who_is_not_a_member_may_act(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let admin = common::seed_user(&pool, &common::unique_email("global-admin"), "admin").await;
    let id = open_test_incident(&pool, project, member).await;
    let (status, done) = call(&app, "POST", "/incident-actions/confirm", None,
        Some(json!({"token": token(&state, id, admin, Action::Resolve)}))).await;
    assert_eq!((status, done["status"].as_str()), (StatusCode::OK, Some("resolved")), "{done}");
    let by: Option<Uuid> = sqlx::query_scalar("select resolved_by from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(by, Some(admin));
}

#[sqlx::test]
async fn a_token_of_a_user_no_longer_in_the_project_is_refused(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Resolve);
    sqlx::query("delete from project_members where user_id = $1").bind(alice).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": t}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn a_password_change_revokes_outstanding_links(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Acknowledge);
    sqlx::query("update users set password_changed_at = now() + interval '1 second' where id = $1").bind(alice).execute(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": t}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn a_token_is_bound_to_its_incident_and_action(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let other = open_test_incident(&pool, project, alice).await;
    let ack = token(&state, id, alice, Action::Acknowledge);
    call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": ack}))).await;
    let statuses: Vec<String> = sqlx::query_scalar("select status from incidents where id = any($1) order by id = $2 desc")
        .bind(vec![id, other]).bind(id).fetch_all(&pool).await.unwrap();
    assert_eq!(statuses, ["acknowledged", "open"], "only its own incident changed");
    // A second, distinct link for the same action (the nonce makes every token distinct, R16).
    let late_ack = token(&state, id, alice, Action::Acknowledge);
    assert_ne!(late_ack, ack);
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": late_ack}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "already acknowledged");
    let (status, _) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": late_ack}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "a 409 does not consume the token");
    for bad in [json!({"token": "nope"}), json!({}), json!({"token": 5})] {
        let (status, _) = call(&app, "POST", "/incident-actions/inspect", None, Some(bad)).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[sqlx::test]
async fn a_get_never_acknowledges(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Acknowledge);
    let (status, _) = call(&app, "GET", &format!("/incident-actions/confirm?token={t}"), None, None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let still: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(still, "open");
}

#[sqlx::test]
async fn unknown_incidents_and_expired_or_forged_tokens_are_invalid(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let expired = action_token::sign(&state.cipher, &Claims { incident_id: id, action: Action::Acknowledge,
        user_id: alice, expires_at: Utc::now() - chrono::Duration::seconds(1) });
    let foreign = action_token::sign(&nami_engine::core::crypto::Cipher::new(b"another-encryption-key-32bytes!!"),
        &Claims { incident_id: id, action: Action::Acknowledge, user_id: alice, expires_at: Utc::now() + action_token::TOKEN_TTL });
    for t in [token(&state, Uuid::new_v4(), alice, Action::Acknowledge), token(&state, id, Uuid::new_v4(), Action::Acknowledge), expired, foreign] {
        for path in ["/incident-actions/inspect", "/incident-actions/confirm"] {
            let (status, body) = call(&app, "POST", path, None, Some(json!({"token": t}))).await;
            assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("this link is invalid or has expired")), "{path}");
        }
    }
    let still: String = sqlx::query_scalar("select status from incidents where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(still, "open");
}

/// Concurrent confirms of one token act once: one 200, the rest 410.
#[sqlx::test]
async fn concurrent_confirms_of_one_token_act_once(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let t = token(&state, id, alice, Action::Resolve);
    let calls = (0..5).map(|_| {
        let (app, t) = (app.clone(), t.clone());
        tokio::spawn(async move { call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": t}))).await.0 })
    });
    let mut statuses = Vec::new();
    for call in calls.collect::<Vec<_>>() {
        statuses.push(call.await.unwrap());
    }
    statuses.sort_by_key(|s| s.as_u16());
    assert_eq!(statuses, [StatusCode::OK, StatusCode::GONE, StatusCode::GONE, StatusCode::GONE, StatusCode::GONE]);
    let resolved: i64 = sqlx::query_scalar("select count(*) from incident_events where incident_id = $1 and kind = 'resolved'")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(resolved, 1);
}

#[sqlx::test]
async fn the_link_endpoints_allow_a_burst_per_client_address(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    for _ in 0..BURST {
        let (status, _) = call(&app, "POST", "/incident-actions/inspect", None, Some(json!({"token": "nope"}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, body) = call(&app, "POST", "/incident-actions/confirm", None, Some(json!({"token": "nope"}))).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("too many requests")));
}

#[sqlx::test]
async fn spent_tokens_are_pruned_long_after_they_expire(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, alice).await;
    let kept_for = chrono::Duration::from_std(incident_action::SPENT_KEPT_FOR).unwrap();
    for (byte, expires_at) in [
        (1u8, Utc::now() - kept_for - chrono::Duration::minutes(1)),
        (2u8, Utc::now() - kept_for + chrono::Duration::minutes(1)),
    ] {
        sqlx::query("insert into incident_action_token_uses (token_hash, incident_id, expires_at) values ($1, $2, $3)")
            .bind(vec![byte; 32]).bind(id).bind(expires_at).execute(&pool).await.unwrap();
    }
    assert_eq!(incident_action::prune_spent(&pool).await.unwrap(), 1);
    let left: Vec<Vec<u8>> = sqlx::query_scalar("select token_hash from incident_action_token_uses").fetch_all(&pool).await.unwrap();
    assert_eq!(left, [vec![2u8; 32]]);
}
