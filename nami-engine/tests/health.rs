use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use tower::ServiceExt;

#[sqlx::test]
async fn health_returns_ok(pool: sqlx::PgPool) {
    let state = nami_engine::core::state::AppState::for_test(pool);
    let app = nami_engine::core::router::build_router(state);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
