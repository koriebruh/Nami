use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;

#[sqlx::test]
async fn health_returns_ok(pool: sqlx::PgPool) {
    let mailer = nami_engine::email::Mailer::new("localhost", 1025);
    let state = nami_engine::state::AppState { db: pool, mailer };
    let app = nami_engine::routes::health::router().with_state(state);

    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
