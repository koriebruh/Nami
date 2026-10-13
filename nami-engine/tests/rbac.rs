use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState {
        db: pool,
        mailer: Mailer::new("localhost", 1025),
        config: nami_engine::config::Config::from_env_for_test(),
    };
    axum::Router::new()
        .merge(routes::auth::router())
        .merge(routes::auth::rate_limited_router())
        .merge(routes::projects::router())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .with_state(state)
}

async fn login_as(app: axum::Router, email: &str) -> axum::http::HeaderValue {
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": email, "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers().get(header::SET_COOKIE).unwrap().clone()
}

#[sqlx::test]
async fn viewer_cannot_create_project(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'viewer')")
        .bind("viewer@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "viewer@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"name": "Test", "slug": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn admin_can_create_project(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin')")
        .bind("admin@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "admin@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"name": "Test", "slug": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// Regression guard for the session-staleness check added to `CurrentUser`
// (requirement beyond the brief's literal text): a session authenticated
// before the user's password was last changed must not work against a
// `CurrentUser`-guarded route, even though it still has a valid `user_id` in
// the session store. Expect 401, not 200/403.
#[sqlx::test]
async fn stale_session_after_password_change_is_rejected(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin')")
        .bind("admin2@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "admin2@example.com").await;

    // Simulate a password change happening after this session was issued
    // (e.g. via a password reset) by bumping `password_changed_at` past
    // `auth_at`, without going through the reset flow itself.
    sqlx::query("update users set password_changed_at = now() + interval '1 minute' where email = $1")
        .bind("admin2@example.com")
        .execute(&pool)
        .await
        .unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"name": "Test", "slug": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn member_without_project_row_gets_forbidden(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'member')")
        .bind("member@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    let project_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into projects (name, slug) values ('Test', 'test') returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "member@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/{}", project_id.0))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

// Positive-coverage counterpart to `member_without_project_row_gets_forbidden`:
// proves a user WITH a project_members row can actually get in. Without this,
// a regression that made `require_project_role` always deny non-global-admins
// would still pass every existing test (they all assert denial).
#[sqlx::test]
async fn member_with_viewer_project_row_can_access_project(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'member') returning id",
    )
    .bind("viewer-member@example.com")
    .bind(&hash)
    .fetch_one(&pool)
    .await
    .unwrap();
    let project_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into projects (name, slug) values ('Test3', 'test3') returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into project_members (project_id, user_id, project_role) values ($1, $2, 'viewer')",
    )
    .bind(project_id.0)
    .bind(user_id.0)
    .execute(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "viewer-member@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/{}", project_id.0))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[sqlx::test]
async fn global_admin_bypasses_project_membership(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin')")
        .bind("admin2@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    let project_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into projects (name, slug) values ('Test2', 'test2') returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "admin2@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/{}", project_id.0))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
