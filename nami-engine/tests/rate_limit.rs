use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::GlobalKeyExtractor, GovernorLayer};
use std::sync::Arc;
use serde_json::json;

#[sqlx::test]
async fn sixth_rapid_login_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    // See main.rs: GlobalKeyExtractor, not the crate's default
    // PeerIpKeyExtractor, which needs ConnectInfo that oneshot() doesn't supply.
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(GlobalKeyExtractor)
            .per_second(60)
            .burst_size(5)
            .finish()
            .unwrap(),
    );
    let app = routes::auth::rate_limited_router()
        .layer(GovernorLayer::new(governor_conf))
        .with_state(state);

    let mut last_status = StatusCode::OK;
    for _ in 0..6 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"email": "nobody@example.com", "password": "x"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        last_status = response.status();
    }

    assert_eq!(last_status, StatusCode::TOO_MANY_REQUESTS);
}
