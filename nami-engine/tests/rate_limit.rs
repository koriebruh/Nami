use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use tower_governor::{governor::GovernorConfigBuilder, key_extractor::SmartIpKeyExtractor, GovernorLayer};
use std::sync::Arc;
use serde_json::json;

fn build_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    // Same key extractor as main.rs: `SmartIpKeyExtractor` checks
    // `X-Forwarded-For` / `X-Real-IP` / `Forwarded` before falling back to
    // `ConnectInfo`, which `oneshot()` never supplies. Tests therefore drive
    // the key via an explicit `X-Forwarded-For` header on each request.
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(SmartIpKeyExtractor)
            .per_second(60)
            .burst_size(5)
            .finish()
            .unwrap(),
    );
    routes::auth::rate_limited_router()
        .layer(GovernorLayer::new(governor_conf))
        // `login` always extracts `Session` (even on a bad-credentials path,
        // which is what these tests exercise), so the router needs a session
        // layer or extraction itself 500s before auth logic ever runs —
        // mirrors the pattern in tests/login.rs's `test_app()`.
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

fn login_request(forwarded_for: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/auth/login")
        .header("content-type", "application/json")
        .header("x-forwarded-for", forwarded_for)
        .body(Body::from(json!({"email": "nobody@example.com", "password": "x"}).to_string()))
        .unwrap()
}

#[sqlx::test]
async fn sixth_rapid_login_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let app = build_app(pool);

    // Same simulated client (same X-Forwarded-For) for all 6 requests: the
    // first 5 should fail auth normally (401, bad credentials), and only the
    // 6th should be rejected by the rate limiter (429), pinning the exact
    // boundary instead of just asserting "something eventually 429s".
    let mut statuses = Vec::with_capacity(6);
    for _ in 0..6 {
        let response = app.clone().oneshot(login_request("1.1.1.1")).await.unwrap();
        statuses.push(response.status());
    }

    assert_eq!(
        &statuses[0..5],
        &[StatusCode::UNAUTHORIZED; 5],
        "first 5 requests from the same client should fail auth normally, not be rate-limited: {statuses:?}"
    );
    assert_eq!(
        statuses[5],
        StatusCode::TOO_MANY_REQUESTS,
        "6th request from the same client should be rate-limited: {statuses:?}"
    );
}

#[sqlx::test]
async fn different_client_ip_has_an_independent_rate_limit_bucket(pool: sqlx::PgPool) {
    let app = build_app(pool);

    // Exhaust the bucket for one simulated client ("1.2.3.4"): 5 requests.
    for _ in 0..5 {
        let response = app.clone().oneshot(login_request("1.2.3.4")).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    // A 6th request from a genuinely different client ("5.6.7.8") must NOT
    // be rate-limited: it should still fail auth normally (401), proving the
    // two "clients" have independent buckets keyed by `SmartIpKeyExtractor`
    // off `X-Forwarded-For`, not a single shared/global bucket. If the
    // extractor were `GlobalKeyExtractor` instead, this request would be the
    // process-wide 6th request overall and would come back 429, failing this
    // assertion — that's what makes this test prove per-IP (not global)
    // behavior, unlike the single-client test above.
    let response = app.clone().oneshot(login_request("5.6.7.8")).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "a different client IP must not be affected by another client's rate limit"
    );
}
