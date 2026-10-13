use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use serde_json::json;

async fn login(app: &axum::Router, email: &str, password: &str) -> axum::http::HeaderValue {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": email, "password": password}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK, "login did not succeed");
    res.headers().get(header::SET_COOKIE).unwrap().clone()
}

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    axum::Router::new()
        .merge(routes::auth::router())
        .merge(routes::auth::rate_limited_router())
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

#[sqlx::test]
async fn expired_reset_token_is_rejected(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at) values ($1, $2, now()) returning id",
    )
    .bind("reset@example.com")
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let (raw_token, token_hash) = nami_engine::auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() - interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/reset")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token": raw_token, "new_password": "newpass123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn reset_invalidates_existing_session(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("reset2@example.com")
        .bind(&password_hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let login_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "reset2@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let user_id: (uuid::Uuid,) = sqlx::query_as("select id from users where email = $1")
        .bind("reset2@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (raw_token, token_hash) = nami_engine::auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/reset")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token": raw_token, "new_password": "brandnewpass"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let me_res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::UNAUTHORIZED);

    // Regression guard for Fix B: the staleness check must not lock out a
    // *legitimate* post-reset login. A fresh login with the new password
    // gets a fresh `auth_at` (set after `update_password` already bumped
    // `password_changed_at`), so it must still work and `/auth/me` must
    // return 200 for that new session.
    let new_cookie = login(&app, "reset2@example.com", "brandnewpass").await;
    let me_res2 = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, new_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res2.status(), StatusCode::OK);
}

#[sqlx::test]
async fn reset_token_cannot_be_reused(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at) values ($1, $2, now()) returning id",
    )
    .bind("reuse@example.com")
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let (raw_token, token_hash) = nami_engine::auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());

    let make_request = |password: &str| {
        Request::builder()
            .method("POST")
            .uri("/auth/password/reset")
            .header("content-type", "application/json")
            .body(Body::from(json!({"token": raw_token, "new_password": password}).to_string()))
            .unwrap()
    };

    let first = app.clone().oneshot(make_request("firstnewpass")).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = app.clone().oneshot(make_request("secondnewpass")).await.unwrap();
    assert_eq!(second.status(), StatusCode::BAD_REQUEST);

    // The password actually set must match the FIRST reset, not the second.
    let login_with_first = app.clone().oneshot(
        Request::builder()
            .method("POST")
            .uri("/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"email": "reuse@example.com", "password": "firstnewpass"}).to_string()))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(login_with_first.status(), StatusCode::OK);

    let login_with_second = app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(json!({"email": "reuse@example.com", "password": "secondnewpass"}).to_string()))
            .unwrap(),
    ).await.unwrap();
    assert_eq!(login_with_second.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn forgot_password_for_existing_user_returns_generic_message_and_creates_token(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at) values ($1, $2, now()) returning id",
    )
    .bind("forgot@example.com")
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/forgot")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "forgot@example.com"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["message"], "if this email exists, a reset link was sent");

    let count: (i64,) = sqlx::query_as(
        "select count(*) from password_reset_tokens where user_id = $1",
    )
    .bind(user_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count.0, 1);
}

#[sqlx::test]
async fn forgot_password_for_unknown_user_returns_same_generic_message(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/forgot")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "doesnotexist@example.com"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["message"], "if this email exists, a reset link was sent");
}

/// Regression guard for Fix B: `current_user_id` (used by `totp_setup`/
/// `totp_enable`) must apply the same staleness check as `me`, not just
/// read `user_id` off the session. Without the fix, an old session replayed
/// after a password reset can still manage 2FA.
#[sqlx::test]
async fn totp_setup_rejects_stale_session_after_reset(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("totpstale@example.com")
        .bind(&password_hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login(&app, "totpstale@example.com", "password123").await;

    let user_id: (uuid::Uuid,) = sqlx::query_as("select id from users where email = $1")
        .bind("totpstale@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (raw_token, token_hash) = nami_engine::auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/reset")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token": raw_token, "new_password": "brandnewpass2"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let setup_res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/totp/setup")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(setup_res.status(), StatusCode::UNAUTHORIZED);
}

/// Regression guard for Fix C: a mailer failure must not leak as a 400,
/// distinguishable from the 200 generic response for an already-registered
/// email — that would be a status-code oracle for account enumeration.
/// Port 1 is a reserved, always-unreachable TCP port, so the SMTP send
/// deterministically fails fast without needing a live server to misconfigure.
#[sqlx::test]
async fn forgot_password_returns_generic_200_even_if_mail_send_fails(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("mailfail@example.com")
        .bind(&password_hash)
        .execute(&pool)
        .await
        .unwrap();

    // Port 1 is a reserved/unreachable TCP port, so the SMTP send will fail.
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1), config: Config::from_env_for_test() };
    let app = axum::Router::new().merge(routes::auth::router()).with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/forgot")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "mailfail@example.com"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// Same regression guard as above, for `register`'s pre-existing (Task 6)
/// instance of the same bug pattern.
#[sqlx::test]
async fn register_returns_generic_200_even_if_mail_send_fails(pool: sqlx::PgPool) {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1), config: Config::from_env_for_test() };
    let app = axum::Router::new()
        .merge(routes::auth::router())
        .merge(routes::auth::rate_limited_router())
        .with_state(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "regmailfail@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
