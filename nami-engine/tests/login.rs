use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

#[sqlx::test]
async fn login_then_me_succeeds(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("user@example.com")
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
                .body(Body::from(json!({"email": "user@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);

    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::OK);
}

#[sqlx::test]
async fn login_rejects_oauth_only_account(pool: sqlx::PgPool) {
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, null, now())")
        .bind("oauth-only@example.com")
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "oauth-only@example.com", "password": "anything"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn logout_invalidates_the_cookie(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("logout@example.com")
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
                .body(Body::from(json!({"email": "logout@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);

    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let logout_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/logout")
                .header(header::COOKIE, cookie.clone())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(logout_res.status(), StatusCode::OK);

    // Replay the OLD (now-stale) cookie against /auth/me: logout must have
    // invalidated the server-side session, so this must be rejected.
    let me_res = app
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
}
