// Notification channels: personal and project, sealed and write-only.
// Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, seed_project, test_app, test_cipher, user_with_role};
use nami_engine::incidents::channel::model::{
    self as channel, ChannelConfig, Inserted, MAX_NAME_CHARS, MAX_PERSONAL_CHANNELS,
    MAX_PROJECT_CHANNELS, Owner, config_context, signing_secret_context,
};
use serde_json::{Value, json};
use uuid::Uuid;

const SLACK_URL: &str = "https://hooks.slack.com/services/T000/B000/XXXXSECRETXXXX";

fn assert_no_secrets(body: &Value) {
    let text = body.to_string();
    for needle in ["XXXXSECRETXXXX", "config_enc", "signing_secret_enc", "bot_token", "webhook_url"] {
        assert!(!text.contains(needle), "{needle} leaked: {text}");
    }
}

#[sqlx::test]
async fn admins_create_project_channels_that_are_sealed_and_never_echoed(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let path = format!("/projects/{project}/notification-channels");
    let body = json!({"name": "ops room", "kind": "slack", "config": {"webhook_url": SLACK_URL}});

    let (status, _) = call(&app, "POST", &path, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!((created["kind"].as_str(), created["summary"].as_str()), (Some("slack"), Some("hooks.slack.com")));
    assert_eq!(created["signing_secret"], Value::Null, "only webhooks get a signing secret");
    assert_no_secrets(&created);

    let (status, listed) = call(&app, "GET", &path, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_no_secrets(&listed);

    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let sealed: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("SECRET"));
    let opened = test_cipher().decrypt(&config_context(id), &sealed).unwrap();
    let config: ChannelConfig = serde_json::from_slice(&opened).unwrap();
    assert_eq!(config, ChannelConfig::Slack { webhook_url: SLACK_URL.into() });
    assert!(test_cipher().decrypt(&config_context(Uuid::new_v4()), &sealed).is_err(), "bound to its row");
}

#[sqlx::test]
async fn webhook_channels_reveal_their_signing_secret_once(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    let (status, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "pager", "kind": "webhook", "config": {"url": "http://10.0.0.5:8080/in?key=abc"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let secret = created["signing_secret"].as_str().unwrap().to_string();
    assert_eq!(secret.len(), 43);
    assert_eq!(created["summary"], "http://10.0.0.5:8080");
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let sealed: Vec<u8> = sqlx::query_scalar("select signing_secret_enc from notification_channels where id = $1")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(test_cipher().decrypt(&signing_secret_context(id), &sealed).unwrap(), secret.as_bytes());
    let (_, listed) = call(&app, "GET", &path, Some(&admin), None).await;
    assert!(!listed.to_string().contains(&secret));
}

#[sqlx::test]
async fn destinations_are_validated_per_kind(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    for (kind, config) in [
        ("slack", json!({"webhook_url": "https://evil.test/services/x"})),
        ("discord", json!({"webhook_url": "https://discord.com.evil.test/api/webhooks/1/x"})),
        ("teams", json!({"webhook_url": "http://prod-00.westus.logic.azure.com/workflows/x"})),
        ("webhook", json!({"url": "http://169.254.169.254/latest/meta-data"})),
        ("webhook", json!({"url": "file:///etc/passwd"})),
        ("telegram", json!({"bot_token": "nope", "chat_id": "1"})),
        ("email", json!({"address": "not an address"})),
        ("sms", json!({"to": "+1"})),
    ] {
        let (status, body) = call(&app, "POST", &path, Some(&admin), Some(json!({"name": "x", "kind": kind, "config": config}))).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY, "{kind}: {status} {body}");
    }
}

#[sqlx::test]
async fn personal_channels_belong_to_their_user_only(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, alice) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, bob) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, created) = call(&app, "POST", "/account/notification-channels", Some(&alice),
        Some(json!({"name": "my phone", "kind": "telegram",
                    "config": {"bot_token": format!("123456:{}", "A".repeat(35)), "chat_id": "42"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["summary"], "Telegram chat 42");
    let id = created["id"].as_str().unwrap();

    let (_, mine) = call(&app, "GET", "/account/notification-channels", Some(&alice), None).await;
    assert_eq!(mine.as_array().unwrap().len(), 1);
    let (_, theirs) = call(&app, "GET", "/account/notification-channels", Some(&bob), None).await;
    assert!(theirs.as_array().unwrap().is_empty());
    let (status, _) = call(&app, "PATCH", &format!("/account/notification-channels/{id}"), Some(&bob), Some(json!({"enabled": false}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &format!("/account/notification-channels/{id}"), Some(&bob), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "POST", "/account/notification-channels", Some(&alice),
        Some(json!({"name": "mail", "kind": "email", "config": {"address": "a@b.test"}}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no personal email channels (R8)");
    for url in ["http://10.0.0.5:8080/in", "http://127.0.0.1:9/x", "https://[::1]/x"] {
        let (status, body) = call(&app, "POST", "/account/notification-channels", Some(&alice),
            Some(json!({"name": "internal", "kind": "webhook", "config": {"url": url}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "personal webhooks are public-only (R9): {url} {body}");
    }
}

#[sqlx::test]
async fn a_global_viewer_without_projects_manages_only_personal_channels(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let email = common::unique_email("gviewer");
    common::seed_user(&pool, &email, "viewer").await;
    let viewer = common::login(&app, &email).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}/notification-channels"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, "POST", "/account/notification-channels", Some(&viewer),
        Some(json!({"name": "hook", "kind": "webhook", "config": {"url": "https://hooks.example.org/in"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "any logged-in user owns personal channels");
}

#[sqlx::test]
async fn patch_keeps_omitted_config_and_replaces_given_config(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    let (_, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "room", "kind": "slack", "config": {"webhook_url": SLACK_URL}}))).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let before: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();

    let (status, renamed) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin), Some(json!({"name": "war room", "enabled": false}))).await;
    assert_eq!((status, renamed["name"].as_str(), renamed["enabled"].as_bool()), (StatusCode::OK, Some("war room"), Some(false)));
    let after: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(before, after, "a rename never touches the sealed config");

    let (status, _) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"kind": "discord", "config": {"webhook_url": "https://discord.com/api/webhooks/1/x"}}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "the kind cannot change");
    let (status, _) = call(&app, "PATCH", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"kind": "slack", "config": {"webhook_url": "https://hooks.slack.com/services/T/B/NEW"}}))).await;
    assert_eq!(status, StatusCode::OK);
    let replaced: Vec<u8> = sqlx::query_scalar("select config_enc from notification_channels where id = $1").bind(id).fetch_one(&pool).await.unwrap();
    assert_ne!(before, replaced);
}

#[sqlx::test]
async fn each_owner_holds_at_most_its_channel_limit(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (admin_id, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let slack = ChannelConfig::Slack { webhook_url: SLACK_URL.into() };
    for (owner, limit, path, message) in [
        (Owner::Project(project), MAX_PROJECT_CHANNELS, format!("/projects/{project}/notification-channels"),
         format!("channel limit reached ({MAX_PROJECT_CHANNELS} per project)")),
        (Owner::User(admin_id), MAX_PERSONAL_CHANNELS, "/account/notification-channels".to_string(),
         format!("channel limit reached ({MAX_PERSONAL_CHANNELS} per user)")),
    ] {
        for i in 0..limit - 1 {
            let inserted = channel::insert(&pool, &test_cipher(), owner, &format!("c{i}"), &slack, true, admin_id)
                .await.unwrap();
            assert!(matches!(inserted, Inserted::Created(_)));
        }
        let body = json!({"name": "last", "kind": "slack", "config": {"webhook_url": SLACK_URL}});
        let (status, created) = call(&app, "POST", &path, Some(&admin), Some(body.clone())).await;
        assert_eq!(status, StatusCode::CREATED, "the channel at the limit: {created}");
        let (status, refused) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
        assert_eq!((status, refused["error"].as_str()), (StatusCode::CONFLICT, Some(message.as_str())));
    }
}

#[sqlx::test]
async fn a_global_admin_gets_404_for_a_project_that_does_not_exist(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let email = common::unique_email("root");
    common::seed_user(&pool, &email, "admin").await;
    let admin = common::login(&app, &email).await;
    let (status, _) = call(&app, "POST", &format!("/projects/{}/notification-channels", Uuid::new_v4()), Some(&admin),
        Some(json!({"name": "room", "kind": "slack", "config": {"webhook_url": SLACK_URL}}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_channel_opens_for_sending_only_under_its_own_key(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let user = common::seed_user(&pool, &common::unique_email("owner"), "member").await;
    let config = ChannelConfig::Webhook { url: "http://10.0.0.5/in?key=SECRET".into() };
    let Inserted::Created(created) =
        channel::insert(&pool, &test_cipher(), Owner::Project(project), "hook", &config, true, user).await.unwrap()
    else { panic!("not created") };
    let id = created.channel.id;
    let opened = channel::open_for_send(&pool, &test_cipher(), id).await.unwrap().unwrap().unwrap();
    assert_eq!((opened.owner, opened.config.clone()), (Owner::Project(project), config));
    assert_eq!(opened.signing_secret.as_deref().map(Vec::as_slice), created.signing_secret.as_deref().map(str::as_bytes));
    assert!(!format!("{opened:?}").contains("SECRET"));

    let other_key = nami_engine::core::crypto::Cipher::new(&[7; 32]);
    assert!(channel::open_for_send(&pool, &other_key, id).await.unwrap().unwrap().is_err(), "the key changed");
    assert!(channel::open_for_send(&pool, &test_cipher(), Uuid::new_v4()).await.unwrap().is_none());
}

#[sqlx::test]
async fn channel_writes_check_role_and_owner(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (admin_id, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let slack = ChannelConfig::Slack { webhook_url: SLACK_URL.into() };
    let insert = |owner| {
        let (pool, slack) = (pool.clone(), slack.clone());
        async move {
            match channel::insert(&pool, &test_cipher(), owner, "room", &slack, true, admin_id).await.unwrap() {
                Inserted::Created(created) => created.channel.id,
                _ => panic!("not created"),
            }
        }
    };
    let room = insert(Owner::Project(project)).await;
    let foreign = insert(Owner::Project(other)).await;
    let path = format!("/projects/{project}/notification-channels");
    let rename = json!({"name": "renamed"});

    let (status, _) = call(&app, "PATCH", &format!("{path}/{room}"), Some(&member), Some(rename.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, "DELETE", &format!("{path}/{room}"), Some(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    for method in ["PATCH", "DELETE"] {
        let body = (method == "PATCH").then(|| rename.clone());
        let (status, _) = call(&app, method, &format!("{path}/{foreign}"), Some(&admin), body.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}: another project's channel");
        let (status, _) = call(&app, method, &format!("/account/notification-channels/{room}"), Some(&member), body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}: a project channel through the personal route");
    }
    let name: String = sqlx::query_scalar("select name from notification_channels where id = $1")
        .bind(room).fetch_one(&pool).await.unwrap();
    assert_eq!(name, "room");

    let (status, _) = call(&app, "DELETE", &format!("{path}/{room}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let left: i64 = sqlx::query_scalar("select count(*) from notification_channels where id = $1")
        .bind(room).fetch_one(&pool).await.unwrap();
    assert_eq!(left, 0);
    let (status, _) = call(&app, "DELETE", &format!("{path}/{room}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_name_is_refused_past_its_char_limit(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let path = format!("/projects/{project}/notification-channels");
    for (chars, expected) in [(MAX_NAME_CHARS, StatusCode::CREATED), (MAX_NAME_CHARS + 1, StatusCode::BAD_REQUEST)] {
        let body = json!({"name": "é".repeat(chars), "kind": "slack", "config": {"webhook_url": SLACK_URL}});
        let (status, body) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
        assert_eq!(status, expected, "{chars} chars: {body}");
    }
}

#[sqlx::test]
async fn a_personal_webhook_cannot_be_patched_to_an_internal_address(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, alice) = user_with_role(&app, &pool, project, Some("member")).await;
    let (status, created) = call(&app, "POST", "/account/notification-channels", Some(&alice),
        Some(json!({"name": "hook", "kind": "webhook", "config": {"url": "https://hooks.example.org/in"}}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let path = format!("/account/notification-channels/{}", created["id"].as_str().unwrap());
    for url in ["http://10.0.0.5/in", "http://[::ffff:127.0.0.1]/in", "http://[64:ff9b:1::a00:5]/in"] {
        let (status, body) = call(&app, "PATCH", &path, Some(&alice),
            Some(json!({"kind": "webhook", "config": {"url": url}}))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {body}");
    }
    let summary: String = sqlx::query_scalar("select summary from notification_channels where id = $1")
        .bind(created["id"].as_str().unwrap().parse::<Uuid>().unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(summary, "https://hooks.example.org");
}
