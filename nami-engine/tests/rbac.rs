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

/// Fix 3b (final-fix-wave): `insert_project` must seed its creator as a
/// `project_members` row at `admin` — previously a brand-new project had
/// ZERO members, even for its creator, so only a *global* admin (not a
/// *project* admin) could ever add the first member. Proves the creator ends
/// up with that row by calling `add_member` (a project-admin-only action) as
/// the SAME user who created the project and confirming it's not 403.
#[sqlx::test]
async fn project_creator_is_seeded_as_project_admin(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    let creator_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin') returning id",
    )
    .bind("creator@example.com")
    .bind(&hash)
    .fetch_one(&pool)
    .await
    .unwrap();
    let other_user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'member') returning id",
    )
    .bind("other@example.com")
    .bind(&hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "creator@example.com").await;

    let create_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie.clone())
                .body(Body::from(json!({"name": "Creator Project", "slug": "creator-project"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_res.status(), StatusCode::OK);
    let body = axum::body::to_bytes(create_res.into_body(), usize::MAX).await.unwrap();
    let project: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let project_id = project["id"].as_str().unwrap();

    // Direct DB check: the creator has a `project_members` row at `admin`.
    let membership: (String,) = sqlx::query_as(
        "select project_role from project_members where project_id = $1 and user_id = $2",
    )
    .bind(uuid::Uuid::parse_str(project_id).unwrap())
    .bind(creator_id.0)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(membership.0, "admin");

    // Behavioral check: the creator (relying only on the seeded
    // `project_members` row, not their global role, since global role here
    // is already `admin` and would bypass the membership check entirely —
    // the DB assertion above is what actually proves the row exists) can
    // perform a project-admin-only action: add another member.
    let add_member_res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/projects/{project_id}/members"))
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"user_id": other_user_id.0, "project_role": "member"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(add_member_res.status(), StatusCode::OK, "project creator must be able to add members, not get 403");
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
