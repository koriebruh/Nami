use axum::{body::{self, Body}, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use serde_json::{json, Value};

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    axum::Router::new()
        .merge(routes::auth::router())
        .merge(routes::auth::rate_limited_router())
        .with_state(state)
}

#[sqlx::test]
async fn register_creates_unverified_user(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let body = json!({ "email": "new@example.com", "password": "password123" }).to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let user: (uuid::Uuid, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("select id, email_verified_at from users where email = $1")
            .bind("new@example.com")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(user.1.is_none());
}

#[sqlx::test]
async fn registering_twice_does_not_duplicate_or_change_response(pool: sqlx::PgPool) {
    // Requires `docker compose up -d mailpit` to be running first.
    reqwest::Client::new()
        .delete("http://localhost:8025/api/v1/messages")
        .send()
        .await
        .unwrap();

    let app1 = test_app(pool.clone());
    let app2 = test_app(pool.clone());
    let body = json!({ "email": "dup@example.com", "password": "password123" }).to_string();

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    let r1 = app1.oneshot(make_request()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);
    let r1_body = body::to_bytes(r1.into_body(), usize::MAX).await.unwrap();

    let r2 = app2.oneshot(make_request()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::OK);
    let r2_body = body::to_bytes(r2.into_body(), usize::MAX).await.unwrap();

    // Anti-enumeration: an attacker must not be able to distinguish a fresh
    // registration from a duplicate by the response body.
    assert_eq!(r1_body, r2_body);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("dup@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    // Exactly one verification email sent: the second (duplicate) registration
    // must not trigger a second send. Filter by recipient (rather than raw
    // inbox count) so this stays correct even when other tests in this file
    // run concurrently and send mail to other addresses.
    let mailpit_body: Value = reqwest::get("http://localhost:8025/api/v1/messages")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let messages = mailpit_body["messages"].as_array().unwrap();
    let dup_messages: Vec<_> = messages
        .iter()
        .filter(|m| m["To"][0]["Address"] == "dup@example.com")
        .collect();
    assert_eq!(dup_messages.len(), 1);
}

#[sqlx::test]
async fn verify_email_rejects_invalid_token(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/auth/verify-email?token=not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
