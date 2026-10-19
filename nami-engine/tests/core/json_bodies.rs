// Requires `docker compose up -d postgres`.
//
// The API accepts state-changing requests (POST, PUT, PATCH) only with a body
// declared as JSON. Browsers send forms (`application/x-www-form-urlencoded`,
// `multipart/form-data`, `text/plain`) across sites without asking first, so
// no form on another site can drive the API with a visitor's session, not
// even an action that takes no body (rotating a ping token, logging out).
use crate::common;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use common::{call, login, seed_project, seed_user, test_app, unique_email, user_with_role};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

/// Sends `method uri` with `content_type` (none if `None`) and `body`.
async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&HeaderValue>,
    content_type: Option<&str>,
    body: &str,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE, cookie);
    }
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// What a cross-site form (or a bare request) can send, with the body it
/// would carry.
const NOT_JSON: [(Option<&str>, &str); 5] = [
    (None, ""),
    (Some("application/x-www-form-urlencoded"), "a=b"),
    (Some("multipart/form-data; boundary=x"), "--x--"),
    (Some("text/plain"), "{}"),
    (Some("application/jsonp"), "{}"),
];

async fn seed_heartbeat(pool: &sqlx::PgPool, project: Uuid) -> (Uuid, String) {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let id = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds) values ($1, 'job', $2, 60)
         returning id",
    )
    .bind(project)
    .bind(&token)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, token)
}

async fn token_of(pool: &sqlx::PgPool, heartbeat: Uuid) -> String {
    sqlx::query_scalar("select token from heartbeats where id = $1")
        .bind(heartbeat)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Rotating a ping token takes no body, which is exactly what a cross-site
/// form can send: without a JSON body it is refused with 415 and the token
/// stays; with one it rotates.
#[sqlx::test]
async fn rotate_token_needs_a_json_body(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (id, token) = seed_heartbeat(&pool, project).await;
    let rotate = format!("/projects/{project}/heartbeats/{id}/rotate-token");

    for (content_type, body) in NOT_JSON {
        let (status, error) = send(&app, "POST", &rotate, Some(&member), content_type, body).await;
        assert_eq!(
            status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{content_type:?}"
        );
        assert_eq!(
            error,
            json!({ "error": "send a JSON body (Content-Type: application/json)" })
        );
        assert_eq!(token_of(&pool, id).await, token, "{content_type:?} rotated");
    }

    for content_type in [
        "application/json",
        "application/json; charset=utf-8",
        "Application/JSON",
    ] {
        let before = token_of(&pool, id).await;
        let (status, body) = send(
            &app,
            "POST",
            &rotate,
            Some(&member),
            Some(content_type),
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{content_type}: {body}");
        assert_ne!(token_of(&pool, id).await, before, "{content_type}");
    }
}

/// Every state-changing route of the API is covered, with and without a
/// session: logging out, starting 2FA setup, logging in, creating and editing.
/// A refused logout leaves the session working.
#[sqlx::test]
async fn every_state_changing_route_refuses_bodies_that_are_not_json(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let admin_email = unique_email("root");
    seed_user(&pool, &admin_email, "admin").await;
    let admin = login(&app, &admin_email).await;
    let (monitor_status, monitor) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&admin),
        Some(json!({ "name": "API", "check_type": "http", "url": "https://example.com/" })),
    )
    .await;
    assert_eq!(monitor_status, StatusCode::CREATED);
    let monitor_uri = format!(
        "/projects/{project}/monitors/{}",
        monitor["id"].as_str().unwrap()
    );
    let (source_status, source) = call(
        &app,
        "POST",
        &format!("/projects/{project}/sources"),
        Some(&admin),
        Some(json!({ "name": "web" })),
    )
    .await;
    assert_eq!(source_status, StatusCode::CREATED, "{source}");
    let source_uri = format!(
        "/projects/{project}/sources/{}",
        source["id"].as_str().unwrap()
    );

    for (method, uri) in [
        ("POST", "/auth/logout".to_string()),
        ("POST", "/auth/totp/setup".to_string()),
        ("POST", "/auth/totp/enable".to_string()),
        ("POST", "/auth/login".to_string()),
        ("POST", "/auth/register".to_string()),
        ("POST", "/auth/password/forgot".to_string()),
        ("POST", "/projects".to_string()),
        ("POST", format!("/projects/{project}/members")),
        ("POST", format!("/projects/{project}/monitors")),
        ("PATCH", monitor_uri.clone()),
        ("POST", format!("/projects/{project}/heartbeats")),
        ("PUT", monitor_uri.clone()),
        ("POST", format!("/projects/{project}/sources")),
        ("PATCH", source_uri.clone()),
        ("POST", format!("{source_uri}/rotate")),
    ] {
        for cookie in [Some(&admin), None] {
            for (content_type, body) in NOT_JSON {
                let (status, _) = send(&app, method, &uri, cookie, content_type, body).await;
                assert_eq!(
                    status,
                    StatusCode::UNSUPPORTED_MEDIA_TYPE,
                    "{method} {uri} with {content_type:?}, session: {}",
                    cookie.is_some()
                );
            }
        }
    }

    let (status, me) = call(&app, "GET", "/auth/me", Some(&admin), None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the refused logouts ended nothing: {me}"
    );
    let (status, _) = send(
        &app,
        "POST",
        "/auth/logout",
        Some(&admin),
        Some("application/json"),
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "GET", "/auth/me", Some(&admin), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Reads and deletes carry no body and need none: a browser cannot send a
/// DELETE across sites without asking first.
#[sqlx::test]
async fn reads_and_deletes_need_no_body(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (id, _) = seed_heartbeat(&pool, project).await;
    let item = format!("/projects/{project}/heartbeats/{id}");

    let (status, _) = send(&app, "GET", &item, Some(&admin), None, "").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = send(&app, "DELETE", &item, Some(&admin), None, "").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// The rule is the API's: cron jobs ping the public listener with whatever
/// `curl -d` sends.
#[sqlx::test]
async fn pings_on_the_public_listener_take_any_body(pool: sqlx::PgPool) {
    let public = common::test_public_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, token) = seed_heartbeat(&pool, project).await;
    let ping = format!("/api/v1/heartbeat/{token}");

    for (content_type, body) in NOT_JSON {
        let (status, _) = send(&public, "POST", &ping, None, content_type, body).await;
        assert_eq!(status, StatusCode::OK, "{content_type:?}");
    }
}
