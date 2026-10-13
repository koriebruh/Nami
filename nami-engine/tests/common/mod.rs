//! Helpers shared by the uptime-monitoring integration tests. Every test
//! binary compiles this module separately and uses a different subset of
//! it, hence the blanket `dead_code` allowance.
#![allow(dead_code)]

use std::sync::OnceLock;

use axum::{
    Router,
    body::Body,
    http::{HeaderValue, Request, StatusCode, header},
};
use nami_engine::state::AppState;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

pub const PASSWORD: &str = "password123";

/// argon2 is deliberately slow: hash the shared fixture password once per
/// test binary instead of once per seeded user.
fn password_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| nami_engine::auth::password::hash_password(PASSWORD).unwrap())
}

/// The full production router (same layers as `main.rs`) over `state`.
pub fn app(state: AppState) -> Router {
    nami_engine::router::with_fallback_connect_info(nami_engine::router::build_router(state)).layer(
        nami_engine::router::configure_session_layer(tower_sessions::MemoryStore::default(), false),
    )
}

pub fn test_app(pool: sqlx::PgPool) -> Router {
    app(AppState::for_test(pool))
}

/// Unique per call: Mailpit keeps messages across test runs and tests run
/// in parallel, so mail assertions always filter by their own recipient.
pub fn unique_email(prefix: &str) -> String {
    format!("{prefix}-{}@example.com", Uuid::new_v4().simple())
}

pub async fn seed_user(pool: &sqlx::PgPool, email: &str, global_role: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into users (email, password_hash, email_verified_at, global_role)
         values ($1, $2, now(), $3) returning id",
    )
    .bind(email)
    .bind(password_hash())
    .bind(global_role)
    .fetch_one(pool)
    .await
    .unwrap()
}

pub async fn seed_project(pool: &sqlx::PgPool) -> Uuid {
    let slug = format!("p-{}", Uuid::new_v4().simple());
    sqlx::query_scalar("insert into projects (name, slug) values ($1, $1) returning id")
        .bind(&slug)
        .fetch_one(pool)
        .await
        .unwrap()
}

pub async fn add_member(pool: &sqlx::PgPool, project_id: Uuid, user_id: Uuid, role: &str) {
    sqlx::query(
        "insert into project_members (project_id, user_id, project_role) values ($1, $2, $3)",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await
    .unwrap();
}

/// Logs in through the real `/auth/login` route and returns the session
/// cookie. Each call uses its own `X-Forwarded-For` address because the
/// login route is rate-limited per IP (burst of 5) and a test may log in
/// several users.
pub async fn login(app: &Router, email: &str) -> HeaderValue {
    let ip = Uuid::new_v4().as_bytes()[..3].to_vec();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .header(
                    "x-forwarded-for",
                    format!("10.{}.{}.{}", ip[0], ip[1], ip[2]),
                )
                .body(Body::from(
                    json!({ "email": email, "password": PASSWORD }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "login failed for {email}"
    );
    response.headers().get(header::SET_COOKIE).unwrap().clone()
}

/// A global-`member` user holding `role` on `project_id` (or no membership
/// at all when `role` is `None`), logged in. Returns (user id, email, cookie).
pub async fn user_with_role(
    app: &Router,
    pool: &sqlx::PgPool,
    project_id: Uuid,
    role: Option<&str>,
) -> (Uuid, String, HeaderValue) {
    let email = unique_email(role.unwrap_or("outsider"));
    let user_id = seed_user(pool, &email, "member").await;
    if let Some(role) = role {
        add_member(pool, project_id, user_id, role).await;
    }
    let cookie = login(app, &email).await;
    (user_id, email, cookie)
}

/// Sends one request through the router. Returns the status and the JSON
/// body (`Value::Null` when the body is empty or not JSON).
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: Option<&HeaderValue>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let request = match body {
        Some(json) => builder
            .header("content-type", "application/json")
            .body(Body::from(json.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Inserts an http monitor directly (bypassing the API).
pub async fn seed_http_monitor(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    name: &str,
    url: &str,
    confirmation_threshold: i32,
) -> Uuid {
    sqlx::query_scalar(
        "insert into monitors (project_id, name, check_type, url, confirmation_threshold, timeout_seconds)
         values ($1, $2, 'http', $3, $4, 5) returning id",
    )
    .bind(project_id)
    .bind(name)
    .bind(url)
    .bind(confirmation_threshold)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// AppState whose ClickHouse client is bound to a fresh per-test database.
/// Call `clickhouse.drop_database()` at the end of the test.
pub async fn state_with_clickhouse(
    pool: &sqlx::PgPool,
) -> (AppState, nami_engine::clickhouse::ClickHouse) {
    let clickhouse = nami_engine::clickhouse::ClickHouse::create_test_database()
        .await
        .expect("ClickHouse must be running: docker compose up -d clickhouse");
    (
        AppState {
            clickhouse: clickhouse.clone(),
            ..AppState::for_test(pool.clone())
        },
        clickhouse,
    )
}

pub const MAILPIT: &str = "http://localhost:8025";

/// Every message Mailpit holds for `recipient`, oldest first, as
/// (subject, plain-text body).
pub async fn mail_for(recipient: &str) -> Vec<(String, String)> {
    let search = url::Url::parse_with_params(
        &format!("{MAILPIT}/api/v1/search"),
        &[
            ("query", format!("to:\"{recipient}\"")),
            ("limit", "50".to_string()),
        ],
    )
    .unwrap();
    let found: Value = reqwest::get(search).await.unwrap().json().await.unwrap();
    let mut messages = Vec::new();
    // Mailpit lists newest first.
    for summary in found["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .rev()
    {
        let to_recipient = summary["To"]
            .as_array()
            .is_some_and(|to| to.iter().any(|address| address["Address"] == recipient));
        if !to_recipient {
            continue;
        }
        let id = summary["ID"].as_str().unwrap();
        let full: Value = reqwest::get(format!("{MAILPIT}/api/v1/message/{id}"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        messages.push((
            full["Subject"].as_str().unwrap_or_default().to_string(),
            full["Text"].as_str().unwrap_or_default().to_string(),
        ));
    }
    messages
}

/// Waits (up to 5 s) until `recipient` has at least `count` messages, then
/// waits 500 ms more and returns everything — so a duplicate that arrives
/// late still makes an exact-count assertion fail.
pub async fn settled_mail(recipient: &str, count: usize) -> Vec<(String, String)> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while mail_for(recipient).await.len() < count && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    mail_for(recipient).await
}

/// Makes the monitor due on the next `run_due_checks`.
pub async fn make_due(pool: &sqlx::PgPool, monitor_id: Uuid) {
    sqlx::query("update monitors set next_check_at = now() - interval '1 second' where id = $1")
        .bind(monitor_id)
        .execute(pool)
        .await
        .unwrap();
}
