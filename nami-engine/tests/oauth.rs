use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, config::Config};
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path};

fn test_config(mock_server_uri: &str) -> Config {
    let mut config = Config::from_env_for_test();
    config.google_auth_url = format!("{mock_server_uri}/authorize");
    config.google_token_url = format!("{mock_server_uri}/token");
    config.google_userinfo_url = format!("{mock_server_uri}/userinfo");
    config.github_auth_url = format!("{mock_server_uri}/authorize");
    config.github_token_url = format!("{mock_server_uri}/token");
    config.github_userinfo_url = format!("{mock_server_uri}/userinfo");
    config
}

fn test_app(pool: sqlx::PgPool, config: Config) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config };
    nami_engine::router::with_fallback_connect_info(nami_engine::router::build_router(state))
        .layer(nami_engine::router::configure_session_layer(
            tower_sessions::MemoryStore::default(),
            false,
        ))
}

/// Drives `/auth/oauth/{provider}/start` then `/callback` through the real
/// router (parsing the real CSRF `state` out of the real redirect), and
/// returns (status, session cookie) so callers can chain further requests
/// (e.g. `/auth/me`) on the same session.
async fn run_oauth_flow(
    app: axum::Router,
    provider: &str,
) -> (StatusCode, axum::http::HeaderValue) {
    let start_res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/auth/oauth/{provider}/start"))
                .body(Body::empty())
                .unwrap(),
        )
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
                .uri(format!("/auth/oauth/{provider}/callback?code=fake-code&state={real_state}"))
                .header(header::COOKIE, session_cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = callback_res.status();
    // `oauth_callback` calls `session.cycle_id()` on success, which rotates
    // the session ID and issues a NEW `Set-Cookie`. Callers that need to make
    // further authenticated requests (e.g. `/auth/me`) must use that
    // post-callback cookie, not the pre-callback one from `/start` — reusing
    // the old one would hit a session the server has already rotated away
    // from and get 401. Fall back to the pre-callback cookie when the
    // callback didn't set one (e.g. it returned an error before cycling).
    let final_cookie = callback_res
        .headers()
        .get(header::SET_COOKIE)
        .cloned()
        .unwrap_or(session_cookie);

    (status, final_cookie)
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
    // Google's real userinfo shape: subject identifier is `sub`, not `id`.
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sub": "google-123", "email": "oauth@example.com"
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let app = test_app(pool.clone(), config);

    let (status, session_cookie) = run_oauth_flow(app.clone(), "google").await;
    assert_eq!(status, StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("oauth@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    // Prove the session is actually authenticated, not just that the
    // callback returned 200 and a user row exists.
    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::OK);
}

#[sqlx::test]
async fn oauth_callback_creates_and_logs_in_github_user(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-token", "token_type": "bearer"
        })))
        .mount(&mock_server)
        .await;
    // GitHub's real userinfo shape: `id` is a JSON integer, `email` present.
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 12345678, "email": "github-oauth@example.com"
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let app = test_app(pool.clone(), config);

    let (status, session_cookie) = run_oauth_flow(app.clone(), "github").await;
    assert_eq!(status, StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("github-oauth@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::OK);
}

#[sqlx::test]
async fn oauth_callback_rejects_github_user_with_no_public_email(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-token", "token_type": "bearer"
        })))
        .mount(&mock_server)
        .await;
    // GitHub: `email` can be null when the user has no public email set.
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 99999999, "email": serde_json::Value::Null
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let app = test_app(pool.clone(), config);

    let (status, _session_cookie) = run_oauth_flow(app, "github").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let count: (i64,) = sqlx::query_as("select count(*) from users")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

#[sqlx::test]
async fn oauth_callback_rejects_mismatched_csrf_state(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    // No mocks mounted for /token or /userinfo: if the CSRF check were
    // bypassed, the handler would attempt to call them and the test would
    // fail with a connection/mock-mismatch error rather than a clean 400,
    // making a silently-skipped check obvious either way.

    let config = test_config(&mock_server.uri());
    let app = test_app(pool.clone(), config);

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
async fn oauth_callback_conflicts_with_existing_password_account_same_email(pool: sqlx::PgPool) {
    // Pre-existing password-based account with the email the OAuth provider
    // will report for a *different* provider_user_id.
    sqlx::query("insert into users (email, password_hash) values ($1, $2)")
        .bind("shared@example.com")
        .bind("not-a-real-hash")
        .execute(&pool)
        .await
        .unwrap();

    let mock_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-token", "token_type": "bearer"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "sub": "google-999", "email": "shared@example.com"
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let app = test_app(pool.clone(), config);

    let (status, _session_cookie) = run_oauth_flow(app, "google").await;
    assert_eq!(status, StatusCode::CONFLICT);

    // No partial insert: exactly the one original password-based row remains.
    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("shared@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    let oauth_identity_count: (i64,) =
        sqlx::query_as("select count(*) from oauth_identities where provider_user_id = $1")
            .bind("google-999")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(oauth_identity_count.0, 0);
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

/// Companion to `register.rs`'s `first_registered_user_becomes_global_admin`,
/// for the OAuth-first-login path: a deployment whose very first user signs in
/// via Google/GitHub rather than email/password must still get a global admin,
/// or `POST /projects` is 403 for everyone forever with no in-app recovery.
#[sqlx::test]
async fn first_oauth_user_becomes_global_admin(pool: sqlx::PgPool) {
    let user = nami_engine::models::user::find_or_create_oauth_user(
        &pool,
        "google",
        "google-first",
        "firstoauth@example.com",
    )
    .await
    .unwrap();

    assert_eq!(user.global_role, "admin");
}

/// And the bootstrap applies to the FIRST user only — including when the
/// first user arrived via the password path and the second via OAuth.
#[sqlx::test]
async fn second_oauth_user_stays_member(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let first = nami_engine::models::user::insert_user(&pool, "pw-first@example.com", &hash)
        .await
        .unwrap();
    assert_eq!(first.global_role, "admin");

    let second = nami_engine::models::user::find_or_create_oauth_user(
        &pool,
        "google",
        "google-second",
        "secondoauth@example.com",
    )
    .await
    .unwrap();
    assert_eq!(second.global_role, "member");
}
