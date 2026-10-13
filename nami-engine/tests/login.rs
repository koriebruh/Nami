use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::state::AppState;
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState::for_test(pool);
    nami_engine::router::with_fallback_connect_info(nami_engine::router::build_router(state))
        .layer(nami_engine::router::configure_session_layer(
            tower_sessions::MemoryStore::default(),
            false,
        ))
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

/// Fix 1 (final-fix-wave): login must stay blocked until the email is
/// verified, per the spec's explicit "login is blocked until the email is
/// verified." Deliberately does NOT seed `email_verified_at` (NULL), unlike
/// every other fixture in this file/repo — proves the new check actually
/// fires, distinct from the generic invalid-credentials 401 (this is a 400
/// since the credentials themselves are correct).
#[sqlx::test]
async fn login_rejects_unverified_email(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, null)")
        .bind("unverified@example.com")
        .bind(&password_hash)
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
                .header("x-forwarded-for", "10.0.0.1")
                .body(Body::from(json!({"email": "unverified@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    // Distinguishable from the generic 401 invalid-credentials response: the
    // password here IS correct, so a flat 401 would be misleading/unactionable.
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["error"], "please verify your email before logging in");
}

/// Companion to the rejection test above: once verified, the same user with
/// the same password can log in successfully.
#[sqlx::test]
async fn login_succeeds_after_email_is_verified(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at) values ($1, $2, null) returning id",
    )
    .bind("verifyme@example.com")
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());

    let blocked = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.2")
                .body(Body::from(json!({"email": "verifyme@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::BAD_REQUEST);

    sqlx::query("update users set email_verified_at = now() where id = $1")
        .bind(user_id.0)
        .execute(&pool)
        .await
        .unwrap();

    let allowed = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .header("x-forwarded-for", "10.0.0.2")
                .body(Body::from(json!({"email": "verifyme@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
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
