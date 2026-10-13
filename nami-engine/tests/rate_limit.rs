use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, config::Config};
use serde_json::json;

fn build_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    // Same governor config/error_handler and session-layer settings as
    // main.rs, defined once in `nami_engine::router` and shared here (see
    // Fix 4 in the final hardening round). `SmartIpKeyExtractor` checks
    // `X-Forwarded-For` / `X-Real-IP` / `Forwarded` before falling back to
    // `ConnectInfo`, which `oneshot()` never supplies — these tests drive
    // the key via an explicit `X-Forwarded-For` header on each request,
    // which takes precedence over `with_fallback_connect_info`'s fallback.
    nami_engine::router::with_fallback_connect_info(nami_engine::router::build_router(state)).layer(
        nami_engine::router::configure_session_layer(tower_sessions::MemoryStore::default(), false),
    )
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

/// Fix 2 (final-fix-wave): `/auth/totp/verify` was previously completely
/// unrated, letting an attacker who already has a stolen password brute-force
/// the 6-digit TOTP code. Proves it now sits behind the same governor as
/// login/register.
#[sqlx::test]
async fn sixth_rapid_totp_verify_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let app = build_app(pool);

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/totp/verify")
            .header("content-type", "application/json")
            .header("x-forwarded-for", "2.2.2.2")
            .body(Body::from(json!({"code": "000000"}).to_string()))
            .unwrap()
    };

    let mut statuses = Vec::with_capacity(6);
    for _ in 0..6 {
        let response = app.clone().oneshot(make_request()).await.unwrap();
        statuses.push(response.status());
    }

    assert_eq!(
        &statuses[0..5],
        // No `pending_user_id` in session for any of these requests, so each
        // fails auth normally (401) rather than being rate-limited.
        &[StatusCode::UNAUTHORIZED; 5],
        "first 5 requests should fail auth normally, not be rate-limited: {statuses:?}"
    );
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS, "6th request should be rate-limited: {statuses:?}");
}

/// Fix 2: `/auth/password/forgot` was previously unrated and could be used to
/// mail-bomb any address. Proves it now sits behind the same governor.
#[sqlx::test]
async fn sixth_rapid_forgot_password_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let app = build_app(pool);

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/password/forgot")
            .header("content-type", "application/json")
            .header("x-forwarded-for", "3.3.3.3")
            .body(Body::from(json!({"email": "nobody@example.com"}).to_string()))
            .unwrap()
    };

    let mut statuses = Vec::with_capacity(6);
    for _ in 0..6 {
        let response = app.clone().oneshot(make_request()).await.unwrap();
        statuses.push(response.status());
    }

    assert_eq!(
        &statuses[0..5],
        &[StatusCode::OK; 5],
        "first 5 requests should succeed with the generic response, not be rate-limited: {statuses:?}"
    );
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS, "6th request should be rate-limited: {statuses:?}");
}

/// Fix 2: `/auth/password/reset` was previously unrated, letting an attacker
/// brute-force a reset token unlimited. Proves it now sits behind the same
/// governor.
#[sqlx::test]
async fn sixth_rapid_reset_password_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let app = build_app(pool);

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/password/reset")
            .header("content-type", "application/json")
            .header("x-forwarded-for", "4.4.4.4")
            .body(Body::from(json!({"token": "not-a-real-token", "new_password": "whatever123"}).to_string()))
            .unwrap()
    };

    let mut statuses = Vec::with_capacity(6);
    for _ in 0..6 {
        let response = app.clone().oneshot(make_request()).await.unwrap();
        statuses.push(response.status());
    }

    assert_eq!(
        &statuses[0..5],
        &[StatusCode::BAD_REQUEST; 5],
        "first 5 requests should fail normally (invalid token), not be rate-limited: {statuses:?}"
    );
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS, "6th request should be rate-limited: {statuses:?}");
}
