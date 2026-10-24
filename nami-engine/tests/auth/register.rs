use axum::{body::{self, Body}, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::core::state::AppState;
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState::for_test(pool);
    nami_engine::core::router::with_fallback_connect_info(nami_engine::core::router::build_router(state))
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
    // Requires `docker compose up -d mailpit` to be running first. A fresh
    // address per run instead of emptying the inbox, which would race the
    // other tests that read their mail.
    let email = format!("dup-{}@example.com", uuid::Uuid::new_v4());

    let app1 = test_app(pool.clone());
    let app2 = test_app(pool.clone());
    let body = json!({ "email": email, "password": "password123" }).to_string();

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
        .bind(&email)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);

    // Exactly one verification email sent: the second (duplicate) registration
    // must not trigger a second send.
    assert_eq!(crate::common::settled_mail(&email, 1).await.len(), 1);
}

/// Fix 3a (final-fix-wave): the first user ever registered on a fresh
/// deployment becomes global admin — otherwise there's no way to ever create
/// the first project (`POST /projects` requires global admin, and
/// `global_role` otherwise defaults to `member`). `#[sqlx::test]` gives each
/// test a fresh, empty database, so registering exactly one user here IS the
/// "first user on a fresh deployment" case.
#[sqlx::test]
async fn first_registered_user_becomes_global_admin(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "email": "first@example.com", "password": "password123" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let role: (String,) = sqlx::query_as("select global_role from users where email = $1")
        .bind("first@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(role.0, "admin");
}

/// Companion to the above: a SECOND registered user must still get the
/// ordinary `member` default — the bootstrap only applies to the very first
/// user on an empty deployment, not every registration thereafter.
#[sqlx::test]
async fn second_registered_user_stays_member(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());

    let make_request = |email: &str| {
        Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "email": email, "password": "password123" }).to_string()))
            .unwrap()
    };

    let first = app.clone().oneshot(make_request("firstuser@example.com")).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app.oneshot(make_request("seconduser@example.com")).await.unwrap();
    assert_eq!(second.status(), StatusCode::OK);

    let first_role: (String,) = sqlx::query_as("select global_role from users where email = $1")
        .bind("firstuser@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(first_role.0, "admin");

    let second_role: (String,) = sqlx::query_as("select global_role from users where email = $1")
        .bind("seconduser@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(second_role.0, "member");
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

/// The verification email carries an absolute nami-ui link (the UI's
/// `/verify-email` page, not a bare engine path), and its token verifies the
/// account.
#[sqlx::test]
async fn the_verification_email_links_to_the_ui_verify_page(pool: sqlx::PgPool) {
    // Requires `docker compose up -d mailpit`. A fresh address per run, since
    // Mailpit keeps earlier runs' mail.
    let email = format!("verify-{}@example.com", uuid::Uuid::new_v4());
    let state = AppState::for_test(pool.clone());
    let prefix = format!("{}/verify-email?token=", state.config.ui_base_url.trim_end_matches('/'));
    let app = nami_engine::core::router::with_fallback_connect_info(
        nami_engine::core::router::build_router(state),
    );

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "email": email, "password": "password123" }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mail = crate::common::settled_mail(&email, 1).await;
    assert_eq!(mail.len(), 1);
    let (_, text) = &mail[0];
    let start = text.find(&prefix).expect("the email holds the absolute UI link");
    let token: String = text[start + prefix.len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    assert!(!token.is_empty());

    let verify = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/verify-email?token={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verify.status(), StatusCode::OK);
    let verified: (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("select email_verified_at from users where email = $1")
            .bind(&email)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(verified.0.is_some());
}
