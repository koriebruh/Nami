use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
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
