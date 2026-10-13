use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, auth, config::Config};
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
async fn wrong_totp_code_never_creates_working_session(pool: sqlx::PgPool) {
    let password_hash = auth::password::hash_password("password123").unwrap();
    let secret = auth::totp::generate_secret();
    sqlx::query(
        "insert into users (email, password_hash, email_verified_at, totp_secret, totp_enabled)
         values ($1, $2, now(), $3, true)",
    )
    .bind("2fa@example.com")
    .bind(&password_hash)
    .bind(&secret)
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
                .body(Body::from(json!({"email": "2fa@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let verify_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/totp/verify")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie.clone())
                .body(Body::from(json!({"code": "000000"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verify_res.status(), StatusCode::UNAUTHORIZED);

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
