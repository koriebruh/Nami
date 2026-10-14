//! Helpers for the log management tests.

use std::{net::SocketAddr, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Request, StatusCode},
};
use nami_engine::{
    clickhouse::ClickHouse,
    crypto::{Cipher, TEST_KEY},
    models::source::{self, SourceConfig},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

/// The cipher `AppState::for_test` builds, so a test can open what the app
/// sealed (and seal what the app must refuse).
pub fn test_cipher() -> Cipher {
    Cipher::new(&TEST_KEY)
}

/// A Postgres pool whose database cannot be reached: any query through it
/// fails once the connection attempt is refused (the API answers 500). A
/// request or a call that gets a different answer never queried the database.
pub fn unreachable_pool() -> sqlx::PgPool {
    sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(300))
        .connect_lazy("postgres://nami:nami@127.0.0.1:1/nami")
        .unwrap()
}

pub fn source_config(name: &str) -> SourceConfig {
    SourceConfig {
        name: name.to_string(),
        platform: "http".to_string(),
        retention_days: 30,
        daily_byte_quota: None,
    }
}

/// Inserts a source directly (bypassing the API); returns its id and its
/// plaintext ingest token.
pub async fn seed_source(pool: &sqlx::PgPool, project_id: Uuid, name: &str) -> (Uuid, String) {
    seed_source_with(pool, project_id, source_config(name)).await
}

pub async fn seed_source_with(
    pool: &sqlx::PgPool,
    project_id: Uuid,
    config: SourceConfig,
) -> (Uuid, String) {
    let created = source::insert_source(pool, &test_cipher(), project_id, &config)
        .await
        .unwrap();
    (created.source.id, created.token)
}

/// `POST /api/v1/logs` with `token` as a Bearer token (none when `None`)
/// and the extra request headers, to the public listener's router (see
/// `common::public_app`): ingest is served there and nowhere else. Returns
/// the status, the response headers and the JSON body (`Value::Null` when
/// not JSON).
pub async fn post_logs(
    public: &Router,
    token: Option<&str>,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method("POST").uri("/api/v1/logs");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = public
        .clone()
        .oneshot(builder.body(body.into()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `POST /api/v1/logs` with a JSON body; returns the status and JSON body.
pub async fn post_json(public: &Router, token: &str, body: Value) -> (StatusCode, Value) {
    let (status, _, json) = post_logs(
        public,
        Some(token),
        &[("content-type", "application/json")],
        body.to_string(),
    )
    .await;
    (status, json)
}

/// Every stored row of `project_id`, oldest first (`fields` as stored text).
pub async fn stored_rows(ch: &ClickHouse, project_id: Uuid) -> Vec<Value> {
    ch.query_rows(
        "SELECT toString(id) AS id, toString(source_id) AS source_id, dt, level, message, fields,
                trace_id, span_id, pattern, toString(pattern_hash) AS pattern_hash, retention_days
         FROM logs WHERE project_id = {project_id:UUID} ORDER BY dt, id",
        &[("project_id", project_id.to_string())],
    )
    .await
    .unwrap()
}

/// The messages of `rows`, sorted (for order-independent assertions).
pub fn messages(rows: &[Value]) -> Vec<String> {
    let mut messages: Vec<String> = rows
        .iter()
        .map(|row| row["message"].as_str().unwrap().to_string())
        .collect();
    messages.sort();
    messages
}

/// A router served on a loopback port, over real sockets, the way `main.rs`
/// serves the public listener. That is where a client can stall, trickle a
/// body or hang up, which a router called in process never sees. Stops when
/// dropped.
pub struct Served {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Served {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn serve(router: Router) -> Served {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    Served { addr, task }
}

/// The head of an ingest request that announces `content_length` bytes of
/// JSON.
pub fn ingest_head(token: &str, content_length: usize) -> String {
    format!(
        "POST /api/v1/logs HTTP/1.1\r\nHost: ingest\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {content_length}\r\n\r\n"
    )
}

/// A complete ingest request with `body`.
pub fn ingest_request(token: &str, body: &str) -> String {
    format!("{}{body}", ingest_head(token, body.len()))
}

/// What the server writes first, if it does within `wait`: the status line and
/// whatever came with it. `None` when nothing came, or the connection closed
/// without a word.
pub async fn first_response(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
    wait: Duration,
) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let mut buffer = [0u8; 1024];
    match tokio::time::timeout(wait, stream.read(&mut buffer)).await {
        Ok(Ok(read)) if read > 0 => Some(String::from_utf8_lossy(&buffer[..read]).into_owned()),
        _ => None,
    }
}

/// `POST /api/v1/logs` to `addr` over a real connection, with a JSON body:
/// the status, the headers and the JSON answer. Gives up after `wait`.
pub async fn post_to(
    addr: SocketAddr,
    token: &str,
    body: &Value,
    wait: Duration,
) -> Result<(StatusCode, HeaderMap, Value), reqwest::Error> {
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{addr}/api/v1/logs"))
        .bearer_auth(token)
        .json(body)
        .timeout(wait)
        .send()
        .await?;
    let status = response.status();
    let headers = response.headers().clone();
    let text = response.text().await?;
    Ok((
        status,
        headers,
        serde_json::from_str(&text).unwrap_or(Value::Null),
    ))
}

/// Polls `condition` until it holds, up to `wait`; panics with `what` after
/// that.
pub async fn eventually(what: &str, wait: Duration, mut condition: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + wait;
    while !condition() {
        assert!(
            std::time::Instant::now() < deadline,
            "never happened: {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
