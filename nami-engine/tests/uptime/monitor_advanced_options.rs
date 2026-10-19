// Advanced request options of http/keyword monitors (NAMI-UP-23): IP version,
// a per-check cookie jar and a proxy whose password is sealed, write-only and
// follows the proxy's origin like every other stored secret. Requires
// `docker compose up -d postgres`.
use crate::common;

use axum::{Router, http::HeaderValue, http::StatusCode};
use common::{LogCapture, call, seed_project, test_cipher, user_with_role};
use nami_engine::uptime::monitor::request::open_proxy_password;
use serde_json::{Value, json};
use uuid::Uuid;

const PROXY_SECRET: &str = "s3cr3t-proxy-pw";
const HEADER_SECRET: &str = "ADMIN-SECRET-KEY-123";
const PASSWORD_SECRET: &str = "ADMIN-PASSWORD-456";
const PROXY_REFUSAL: &str =
    "changing the proxy requires entering the header values and passwords again, or clearing them";

struct Setup {
    app: Router,
    owner: HeaderValue,
    member: HeaderValue,
    viewer: HeaderValue,
    collection: String,
}

async fn setup(pool: &sqlx::PgPool) -> Setup {
    let app = common::test_app(pool.clone());
    let project = seed_project(pool).await;
    let (_, _, owner) = user_with_role(&app, pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, pool, project, Some("viewer")).await;
    Setup {
        app,
        owner,
        member,
        viewer,
        collection: format!("/projects/{project}/monitors"),
    }
}

impl Setup {
    async fn create(&self, who: &HeaderValue, body: Value) -> (StatusCode, Value) {
        call(&self.app, "POST", &self.collection, Some(who), Some(body)).await
    }

    async fn patch(&self, who: &HeaderValue, id: Uuid, body: Value) -> (StatusCode, Value) {
        let item = format!("{}/{id}", self.collection);
        call(&self.app, "PATCH", &item, Some(who), Some(body)).await
    }

    async fn get(&self, who: &HeaderValue, id: Uuid) -> (StatusCode, Value) {
        let item = format!("{}/{id}", self.collection);
        call(&self.app, "GET", &item, Some(who), None).await
    }

    /// A member-created http monitor with a proxy and a proxy password.
    async fn proxied(&self) -> Uuid {
        let (status, body) = self
            .create(
                &self.member,
                json!({
                    "name": "Proxied",
                    "check_type": "http",
                    "url": "https://example.com/health",
                    "proxy_url": "http://proxy.internal:3128",
                    "proxy_username": "nami",
                    "proxy_password": PROXY_SECRET,
                    "ip_version": "any",
                    "remember_cookies": true,
                }),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        id_of(&body)
    }
}

fn id_of(body: &Value) -> Uuid {
    Uuid::parse_str(body["id"].as_str().unwrap()).unwrap()
}

/// Every key of a JSON object, recursively.
fn keys(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                out.push(key.clone());
                keys(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|inner| keys(inner, out)),
        _ => {}
    }
}

async fn sealed_proxy_password(pool: &sqlx::PgPool, id: Uuid) -> Option<Vec<u8>> {
    sqlx::query_scalar("select proxy_password_enc from monitors where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

type Row = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    String,
);

/// Everything a refused PATCH must leave as it was.
async fn row(pool: &sqlx::PgPool, id: Uuid) -> Row {
    sqlx::query_as(
        "select url, proxy_url, proxy_username, proxy_password_enc, request_headers_enc,
                auth_password_enc, ip_version
         from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn monitor_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn the_proxy_password_is_stored_sealed_and_never_returned(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let (status, body) = s
        .create(
            &s.member,
            json!({
                "name": "Proxied",
                "check_type": "http",
                "url": "https://example.com/health",
                "proxy_url": "http://proxy.internal:3128",
                "proxy_username": "nami",
                "proxy_password": PROXY_SECRET,
                "ip_version": "any",
                "remember_cookies": true,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["has_proxy_password"], true);
    assert_eq!(body["proxy_username"], "nami");
    assert_eq!(body["proxy_url"], "http://proxy.internal:3128");
    assert_eq!(body["remember_cookies"], true);
    assert_eq!(body["ip_version"], "any");
    let mut names = Vec::new();
    keys(&body, &mut names);
    let password_keys: Vec<&String> = names
        .iter()
        .filter(|k| k.contains("proxy_password"))
        .collect();
    assert_eq!(password_keys, ["has_proxy_password"], "{body}");
    assert!(!body.to_string().contains(PROXY_SECRET));

    let id = id_of(&body);
    let sealed = sealed_proxy_password(&pool, id).await.unwrap();
    assert!(
        !sealed
            .windows(PROXY_SECRET.len())
            .any(|w| w == PROXY_SECRET.as_bytes())
    );
    assert_eq!(
        open_proxy_password(&test_cipher(), id, &sealed)
            .unwrap()
            .expose(),
        PROXY_SECRET
    );

    let (status, member_view) = s.get(&s.member, id).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!member_view.to_string().contains(PROXY_SECRET));
    assert_eq!(member_view["proxy_username"], "nami");
    assert_eq!(member_view["redacted"], false);

    let (status, viewer_view) = s.get(&s.viewer, id).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!viewer_view.to_string().contains(PROXY_SECRET));
    assert!(!viewer_view.to_string().contains("\"nami\""));
    assert_eq!(viewer_view["proxy_username"], "***");
    assert_eq!(viewer_view["redacted"], true);
    assert_eq!(viewer_view["has_proxy_password"], true);
    assert_eq!(viewer_view["proxy_url"], "http://proxy.internal:3128");

    // The list says the same.
    let (_, list) = call(&s.app, "GET", &s.collection, Some(&s.viewer), None).await;
    assert!(!list.to_string().contains(PROXY_SECRET));
    assert!(!list.to_string().contains("\"nami\""));
}

#[sqlx::test]
async fn ip_version_and_cookies_are_stored_and_returned(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let (status, body) = s
        .create(
            &s.member,
            json!({
                "name": "v6", "check_type": "tcp", "host": "db.internal", "port": 5432,
                "ip_version": "ipv6",
            }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["ip_version"], "ipv6");
    assert_eq!(body["remember_cookies"], false);
    assert_eq!(body["proxy_url"], Value::Null);
    assert_eq!(body["has_proxy_password"], false);
    let id = id_of(&body);
    let (status, body) = s
        .patch(&s.member, id, json!({ "ip_version": "ipv4" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ip_version"], "ipv4");
    // dns does not use it: reset, not refused.
    let (status, body) = s
        .patch(
            &s.member,
            id,
            json!({ "check_type": "dns", "host": "example.com" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ip_version"], "any");
}

#[sqlx::test]
async fn patch_keeps_omitted_proxy_password_replaces_given_and_null_clears(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let id = s.proxied().await;
    let original = sealed_proxy_password(&pool, id).await.unwrap();

    let (status, body) = s
        .patch(
            &s.member,
            id,
            json!({ "name": "renamed", "group_id": null, "paused": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        sealed_proxy_password(&pool, id).await.unwrap(),
        original,
        "kept byte for byte"
    );

    let (status, body) = s
        .patch(&s.member, id, json!({ "proxy_password": "new" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let replaced = sealed_proxy_password(&pool, id).await.unwrap();
    assert_ne!(replaced, original);
    assert_eq!(
        open_proxy_password(&test_cipher(), id, &replaced)
            .unwrap()
            .expose(),
        "new"
    );
    assert!(!body.to_string().contains("\"new\""));

    let (status, body) = s
        .patch(&s.member, id, json!({ "proxy_password": null }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(sealed_proxy_password(&pool, id).await, None);
    assert_eq!(body["has_proxy_password"], false);
    assert_eq!(body["proxy_username"], "nami");

    let (status, body) = s
        .patch(&s.member, id, json!({ "proxy_password": "again" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = s.patch(&s.member, id, json!({ "proxy_url": null })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["proxy_url"], Value::Null);
    assert_eq!(body["proxy_username"], Value::Null);
    assert_eq!(body["has_proxy_password"], false);
    let (_, proxy_url, proxy_username, sealed, ..) = row(&pool, id).await;
    assert_eq!((proxy_url, proxy_username, sealed), (None, None, None));
}

/// An owner-created monitor with a header, basic auth and a proxy with a
/// password, which a member then tries to repoint.
async fn owner_secrets(s: &Setup, url: &str, proxy: Option<&str>) -> Uuid {
    let mut body = json!({
        "name": "Owned",
        "check_type": "http",
        "url": url,
        "request_headers": [{ "name": "X-Api-Key", "value": HEADER_SECRET }],
        "auth_username": "svc",
        "auth_password": PASSWORD_SECRET,
    });
    if let Some(proxy) = proxy {
        body["proxy_url"] = json!(proxy);
        body["proxy_username"] = json!("nami");
        body["proxy_password"] = json!(PROXY_SECRET);
    }
    let (status, created) = s.create(&s.owner, body).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    id_of(&created)
}

fn assert_no_secret(response: &Value) {
    let text = response.to_string();
    for secret in [PROXY_SECRET, HEADER_SECRET, PASSWORD_SECRET] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
}

#[sqlx::test]
async fn changing_the_proxy_requires_every_secret_again(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let id = owner_secrets(
        &s,
        "http://127.0.0.1:9/health",
        Some("http://127.0.0.1:3128"),
    )
    .await;
    let before = row(&pool, id).await;

    for body in [
        // Port.
        json!({ "proxy_url": "http://127.0.0.1:3129" }),
        // Scheme.
        json!({ "proxy_url": "https://127.0.0.1:3128" }),
        // Host.
        json!({ "proxy_url": "http://localhost:3128" }),
        // The proxy password replaced, the target's secrets kept.
        json!({ "proxy_url": "http://127.0.0.1:3129", "proxy_password": "mine" }),
        // Headers too, the basic-auth password kept.
        json!({ "proxy_url": "http://127.0.0.1:3129", "proxy_password": "mine",
            "request_headers": null }),
    ] {
        let (status, response) = s.patch(&s.member, id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(response["error"], PROXY_REFUSAL, "{body}");
        assert_no_secret(&response);
        assert!(!response.to_string().contains("X-Api-Key"));
        assert_eq!(row(&pool, id).await, before, "{body}: row unchanged");
    }

    // Every secret replaced or cleared in the same request.
    let (status, response) = s
        .patch(
            &s.member,
            id,
            json!({
                "proxy_url": "http://127.0.0.1:3129",
                "proxy_password": "mine",
                "request_headers": [{ "name": "X-Api-Key", "value": "mine" }],
                "auth_password": null,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["proxy_url"], "http://127.0.0.1:3129");
    assert_no_secret(&response);
    let after = row(&pool, id).await;
    assert_ne!(after.3, before.3, "proxy password replaced");
    assert_eq!(after.5, None, "basic-auth password cleared");

    // Removing the proxy is not moving it.
    let id = owner_secrets(
        &s,
        "http://127.0.0.1:9/health",
        Some("http://127.0.0.1:3128"),
    )
    .await;
    let (status, response) = s.patch(&s.member, id, json!({ "proxy_url": null })).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_no_secret(&response);

    // Adding a proxy counts.
    let unproxied = owner_secrets(&s, "http://127.0.0.1:9/health", None).await;
    let before = row(&pool, unproxied).await;
    let (status, response) = s
        .patch(
            &s.member,
            unproxied,
            json!({ "proxy_url": "http://127.0.0.1:3128" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(response["error"], PROXY_REFUSAL);
    assert_eq!(row(&pool, unproxied).await, before);

    // The target's origin alone: the proxy password stays, the target's
    // secrets follow the fix-wave rule.
    let id = owner_secrets(
        &s,
        "http://127.0.0.1:9/health",
        Some("http://127.0.0.1:3128"),
    )
    .await;
    let sealed = sealed_proxy_password(&pool, id).await;
    let (status, response) = s
        .patch(
            &s.member,
            id,
            json!({ "url": "http://127.0.0.1:10/health" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_ne!(response["error"], PROXY_REFUSAL);
    let (status, response) = s
        .patch(
            &s.member,
            id,
            json!({
                "url": "http://127.0.0.1:10/health",
                "request_headers": null,
                "auth_password": null,
            }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["has_proxy_password"], true);
    assert_eq!(
        sealed_proxy_password(&pool, id).await,
        sealed,
        "byte for byte"
    );
    assert_no_secret(&response);
}

#[sqlx::test]
async fn invalid_proxy_settings_are_400_and_write_nothing(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let base = json!({ "name": "Bad", "check_type": "http", "url": "https://example.com/" });
    let cases = [
        (
            json!({ "proxy_url": "http://user:pw@proxy:3128" }),
            "proxy_url must not contain credentials; use proxy_username and proxy_password",
        ),
        (
            json!({ "proxy_url": "socks5://proxy:1080" }),
            "proxy_url must be an http:// or https:// URL",
        ),
        (
            json!({ "proxy_url": "http://proxy" }),
            "proxy_url must include a port",
        ),
        (
            json!({ "proxy_url": "http://169.254.169.254:80" }),
            "proxy_url must not point at a link-local, metadata or unspecified address",
        ),
        (
            json!({ "proxy_username": "nami" }),
            "proxy_username requires proxy_url",
        ),
        (
            json!({ "proxy_url": "http://proxy:3128", "proxy_password": PROXY_SECRET }),
            "proxy_password requires proxy_username",
        ),
        (
            json!({ "proxy_url": "http://proxy:3128", "ip_version": "ipv4" }),
            "ip_version cannot be combined with a proxy: the proxy resolves the target",
        ),
        (
            json!({ "ip_version": "v6" }),
            "ip_version must be any, ipv4 or ipv6",
        ),
    ];
    let count = monitor_count(&pool).await;
    for (extra, message) in &cases {
        let mut body = base.clone();
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (status, response) = s.create(&s.member, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(response["error"], *message, "{body}");
        assert_no_secret(&response);
    }
    assert_eq!(monitor_count(&pool).await, count, "nothing created");

    let id = s.proxied().await;
    let before = row(&pool, id).await;
    for (extra, message) in &cases {
        let mut body = extra.clone();
        // On the proxied monitor, "username without url" means removing the
        // url, and "password without username" removing the username.
        if body.get("proxy_username").is_some() && body.get("proxy_url").is_none() {
            body["proxy_url"] = Value::Null;
        }
        if body.get("proxy_password").is_some() {
            body = json!({ "proxy_username": null, "proxy_password": PROXY_SECRET });
        }
        let (status, response) = s.patch(&s.member, id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(response["error"], *message, "{body}");
        assert_no_secret(&response);
        assert_eq!(row(&pool, id).await, before, "{body}: row unchanged");
    }
}

/// Removing the proxy while sending a new proxy password in the same PATCH
/// is a password without a username: 400, and nothing changes (the URL, the
/// username and the stored password all stay).
#[sqlx::test]
async fn removing_the_proxy_with_a_new_proxy_password_is_400_and_changes_nothing(
    pool: sqlx::PgPool,
) {
    let s = setup(&pool).await;
    let id = s.proxied().await;
    let before = row(&pool, id).await;
    for body in [
        json!({ "proxy_url": null, "proxy_password": "x" }),
        json!({ "proxy_url": "", "proxy_password": "x" }),
    ] {
        let (status, response) = s.patch(&s.member, id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(
            response["error"], "proxy_password requires proxy_username",
            "{body}"
        );
        assert_no_secret(&response);
        assert_eq!(row(&pool, id).await, before, "{body}: row unchanged");
    }
    let (_, view) = s.get(&s.member, id).await;
    assert_eq!(view["proxy_url"], "http://proxy.internal:3128");
    assert_eq!(view["has_proxy_password"], true);
}

#[sqlx::test]
async fn viewers_cannot_set_a_proxy(pool: sqlx::PgPool) {
    let s = setup(&pool).await;
    let id = s.proxied().await;
    let (status, _) = s.patch(&s.viewer, id, json!({ "proxy_url": null })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn proxy_secrets_never_reach_a_log_line(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let s = setup(&pool).await;
    let id = s.proxied().await;
    let (status, _) = s
        .patch(
            &s.member,
            id,
            json!({ "proxy_password": "an0ther-proxy-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = s
        .patch(
            &s.member,
            id,
            json!({ "proxy_password": "x".repeat(1100) + "-long-proxy-pw" }),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    s.get(&s.member, id).await;
    s.get(&s.viewer, id).await;
    let lines = log.lines();
    assert!(!lines.is_empty(), "the capture saw the requests");
    for line in &lines {
        for secret in [PROXY_SECRET, "an0ther-proxy-pw", "-long-proxy-pw"] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
    }
}
