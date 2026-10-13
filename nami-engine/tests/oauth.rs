use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path};

fn test_config(mock_server_uri: &str) -> Config {
    let mut config = Config::from_env_for_test();
    config.google_auth_url = format!("{mock_server_uri}/authorize");
    config.google_token_url = format!("{mock_server_uri}/token");
    config.google_userinfo_url = format!("{mock_server_uri}/userinfo");
    config
}

#[sqlx::test]
async fn oauth_callback_creates_and_logs_in_user(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-token", "token_type": "bearer"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "google-123", "email": "oauth@example.com"
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let state = AppState { db: pool.clone(), mailer: Mailer::new("localhost", 1025), config };
    let app = axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .with_state(state);

    let start_res = app
        .clone()
        .oneshot(Request::builder().uri("/auth/oauth/google/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let session_cookie = start_res.headers().get(header::SET_COOKIE).unwrap().clone();
    let location = start_res.headers().get(header::LOCATION).unwrap().to_str().unwrap().to_string();
    let real_state = url::Url::parse(&location)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();

    let callback_res = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/oauth/google/callback?code=fake-code&state={real_state}"))
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(callback_res.status(), StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("oauth@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}

#[sqlx::test]
async fn oauth_callback_rejects_mismatched_csrf_state(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    // No mocks mounted for /token or /userinfo: if the CSRF check were
    // bypassed, the handler would attempt to call them and the test would
    // fail with a connection/mock-mismatch error rather than a clean 400,
    // making a silently-skipped check obvious either way.

    let config = test_config(&mock_server.uri());
    let state = AppState { db: pool.clone(), mailer: Mailer::new("localhost", 1025), config };
    let app = axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .with_state(state);

    let start_res = app
        .clone()
        .oneshot(Request::builder().uri("/auth/oauth/google/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let session_cookie = start_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let callback_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/oauth/google/callback?code=fake-code&state=not-the-real-state")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(callback_res.status(), StatusCode::BAD_REQUEST);

    let count: (i64,) = sqlx::query_as("select count(*) from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[sqlx::test]
async fn find_or_create_oauth_user_does_not_duplicate_on_repeat_login(pool: sqlx::PgPool) {
    let first = nami_engine::models::user::find_or_create_oauth_user(
        &pool,
        "google",
        "google-456",
        "repeat@example.com",
    )
    .await
    .unwrap();

    let second = nami_engine::models::user::find_or_create_oauth_user(
        &pool,
        "google",
        "google-456",
        "repeat@example.com",
    )
    .await
    .unwrap();

    assert_eq!(first.id, second.id);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("repeat@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}
