// Write-only secrets (header values, the basic-auth password) never move to
// another origin without being entered again: a member who never saw them
// cannot retarget a monitor at a server of their own and collect them one
// check later. Requires `docker compose up -d postgres`.
mod common;

use axum::{Router, http::HeaderValue, http::StatusCode};
use common::{call, seed_project, user_with_role};
use nami_engine::{core::state::AppState, uptime::worker::run_due_checks};
use serde_json::{Value, json};
use uuid::Uuid;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

const HEADER_SECRET: &str = "ADMIN-SECRET-KEY-123";
const PASSWORD_SECRET: &str = "ADMIN-PASSWORD-456";
const REFUSAL: &str = "changing the url's scheme, host or port requires entering the header values and password again, or clearing them";

struct Setup {
    state: AppState,
    app: Router,
    member: HeaderValue,
    item: String,
    id: Uuid,
}

/// An owner-created monitor of `url` carrying an API-key header and basic
/// auth, and a project member who never saw either secret.
async fn setup(pool: &sqlx::PgPool, url: &str, verify_ssl: bool) -> Setup {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(pool).await;
    let (_, _, owner) = user_with_role(&app, pool, project, Some("admin")).await;
    let (_, _, member) = user_with_role(&app, pool, project, Some("member")).await;
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&owner),
        Some(json!({
            "name": "Health",
            "check_type": "http",
            "url": url,
            "request_headers": [{ "name": "X-Api-Key", "value": HEADER_SECRET }],
            "auth_username": "svc",
            "auth_password": PASSWORD_SECRET,
            "verify_ssl": verify_ssl,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    Setup {
        state,
        app,
        member,
        item: format!("/projects/{project}/monitors/{id}"),
        id,
    }
}

impl Setup {
    async fn patch(&self, body: Value) -> (StatusCode, Value) {
        call(
            &self.app,
            "PATCH",
            &self.item,
            Some(&self.member),
            Some(body),
        )
        .await
    }

    /// Runs the monitor's next check through the worker.
    async fn check_now(&self, pool: &sqlx::PgPool) {
        common::make_due(pool, self.id).await;
        assert_eq!(run_due_checks(&self.state).await.unwrap(), 1);
    }
}

async fn listener() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

fn header_text(request: &wiremock::Request, name: &str) -> Option<String> {
    request
        .headers
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
}

/// The live repro of the Phase 2 final review: owner's secrets, member PATCHes
/// only the url to a listener of their own.
#[sqlx::test]
async fn a_url_moved_to_another_origin_cannot_keep_the_secrets(pool: sqlx::PgPool) {
    let owned = listener().await;
    let stolen = listener().await;
    let s = setup(&pool, &format!("{}/health", owned.uri()), true).await;

    for body in [
        // Both kept.
        json!({ "url": format!("{}/steal", stolen.uri()) }),
        // Headers replaced, password kept.
        json!({
            "url": format!("{}/steal", stolen.uri()),
            "request_headers": [{ "name": "X-Api-Key", "value": "new" }],
        }),
        // Password replaced, headers kept.
        json!({
            "url": format!("{}/steal", stolen.uri()),
            "auth_password": "new",
        }),
        // Same port, another host name for the same machine.
        json!({ "url": owned.uri().replace("127.0.0.1", "localhost") }),
    ] {
        let (status, response) = s.patch(body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {response}");
        assert_eq!(response["error"], REFUSAL, "{body}");
        let text = response.to_string();
        assert!(!text.contains(HEADER_SECRET) && !text.contains("X-Api-Key"));
    }

    s.check_now(&pool).await;
    assert!(
        stolen.received_requests().await.unwrap().is_empty(),
        "no secret ever reached the member's listener"
    );
    let at_owner = owned.received_requests().await.unwrap();
    assert_eq!(at_owner.len(), 1, "the monitor still checks its own target");
    assert_eq!(
        header_text(&at_owner[0], "x-api-key").as_deref(),
        Some(HEADER_SECRET)
    );
}

#[sqlx::test]
async fn replacing_or_clearing_every_secret_lets_the_url_move(pool: sqlx::PgPool) {
    let owned = listener().await;
    let other = listener().await;
    let s = setup(&pool, &format!("{}/health", owned.uri()), true).await;

    let (status, response) = s
        .patch(json!({
            "url": format!("{}/mine", other.uri()),
            "request_headers": [{ "name": "X-Api-Key", "value": "member-key" }],
            "auth_password": "member-pass",
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    s.check_now(&pool).await;
    let received = other.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(
        header_text(&received[0], "x-api-key").as_deref(),
        Some("member-key")
    );
    // base64("svc:member-pass")
    assert_eq!(
        header_text(&received[0], "authorization").as_deref(),
        Some("Basic c3ZjOm1lbWJlci1wYXNz")
    );

    // Clearing works too, and moving a monitor without secrets is free.
    let (status, response) = s
        .patch(json!({
            "url": format!("{}/back", owned.uri()),
            "request_headers": null,
            "auth_password": null,
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["request_headers"], json!([]));
    assert_eq!(response["has_auth_password"], false);
    let (status, _) = s
        .patch(json!({ "url": format!("{}/anywhere", other.uri()) }))
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn a_path_or_query_change_on_the_same_origin_keeps_the_secrets(pool: sqlx::PgPool) {
    let owned = listener().await;
    let s = setup(&pool, &format!("{}/health", owned.uri()), true).await;

    let (status, response) = s
        .patch(json!({ "url": format!("{}/v2/health?deep=1", owned.uri()) }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["has_auth_password"], true);
    assert_eq!(
        response["request_headers"],
        json!([{ "name": "X-Api-Key", "value_set": true }])
    );
    s.check_now(&pool).await;
    let received = owned.received_requests().await.unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].url.path(), "/v2/health");
    assert_eq!(
        header_text(&received[0], "x-api-key").as_deref(),
        Some(HEADER_SECRET)
    );
}

#[sqlx::test]
async fn an_https_to_http_downgrade_on_the_same_host_is_another_origin(pool: sqlx::PgPool) {
    let s = setup(&pool, "https://example.com/health", true).await;
    let (status, response) = s.patch(json!({ "url": "http://example.com/health" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(response["error"], REFUSAL);
    // An explicit default port is the same origin.
    let (status, response) = s
        .patch(json!({ "url": "https://example.com:443/other" }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

#[sqlx::test]
async fn turning_certificate_checks_off_cannot_keep_the_secrets(pool: sqlx::PgPool) {
    let s = setup(&pool, "https://example.com/health", true).await;
    let (status, response) = s.patch(json!({ "verify_ssl": false })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(
        response["error"],
        "turning off certificate verification requires entering the header values and password again, or clearing them"
    );
    let (status, response) = s
        .patch(json!({
            "verify_ssl": false,
            "request_headers": [{ "name": "X-Api-Key", "value": "again" }],
            "auth_password": "again",
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    // Already off: other edits keep working.
    let (status, response) = s.patch(json!({ "name": "Renamed" })).await;
    assert_eq!(status, StatusCode::OK, "{response}");
}

#[sqlx::test]
async fn a_monitor_whose_type_changes_drops_its_secrets_without_a_refusal(pool: sqlx::PgPool) {
    let s = setup(&pool, "https://example.com/health", true).await;
    let (status, response) = s
        .patch(json!({ "check_type": "tcp", "host": "example.com", "port": 443 }))
        .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(response["has_auth_password"], false);
}

/// The scheme is case-insensitive: `HTTPS://` is https, and turning
/// certificate checks off on it is refused like on `https://`.
#[sqlx::test]
async fn an_uppercase_https_scheme_is_still_https(pool: sqlx::PgPool) {
    let s = setup(&pool, "https://example.com/health", true).await;
    let (status, response) = s.patch(json!({ "url": "HTTPS://example.com/other" })).await;
    assert_eq!(status, StatusCode::OK, "same origin: {response}");
    let (status, response) = s.patch(json!({ "verify_ssl": false })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert_eq!(
        response["error"],
        "turning off certificate verification requires entering the header values and password again, or clearing them"
    );
    let (status, response) = s
        .patch(json!({ "url": "HTTPS://example.com/x", "verify_ssl": false }))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
}
