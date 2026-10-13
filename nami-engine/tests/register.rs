use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
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

    let r2 = app2.oneshot(make_request()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("dup@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
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
