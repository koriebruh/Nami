//! Proxy environment variables must never reroute uptime checks.
//!
//! This test has its own binary because it changes process-wide environment
//! variables. Alone in its process, it can do that before any other thread
//! exists, and no other test builds a client while the variables are set.

use chrono::Utc;
use nami_engine::{
    models::monitor::Monitor,
    uptime::checker::{CheckStatus, build_http_client, perform_check},
};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn http_monitor(url: String) -> Monitor {
    Monitor {
        id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        name: "test".to_string(),
        check_type: "http".to_string(),
        url: Some(url),
        host: None,
        port: None,
        http_method: "GET".to_string(),
        keyword: None,
        keyword_mode: "contains".to_string(),
        expected_status_min: 200,
        expected_status_max: 399,
        interval_seconds: 60,
        timeout_seconds: 5,
        confirmation_threshold: 2,
        paused: false,
        current_status: "pending".to_string(),
        consecutive_failures: 0,
        last_checked_at: None,
        next_check_at: Utc::now(),
        created_at: Utc::now(),
        follow_redirects: true,
        verify_ssl: true,
        ..Monitor::default()
    }
}

#[test]
fn proxy_environment_variables_never_reroute_checks() {
    // Nothing listens on this port: a check that honours the proxy variables
    // fails with "connection refused" instead of reaching its target.
    let dead_proxy = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
    };
    // SAFETY: this is the only test in the binary, and it changes the
    // environment before it starts the runtime, so no other thread of this
    // process exists yet that could read the environment at the same time.
    unsafe {
        for name in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::set_var(name, &dead_proxy);
        }
        for name in ["NO_PROXY", "no_proxy"] {
            std::env::remove_var(name);
        }
    }

    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let outcome = perform_check(
            &build_http_client(),
            &http_monitor(format!("{}/", server.uri())),
        )
        .await;
        assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
        assert_eq!(outcome.status_code, Some(200));

        // The mock server speaks plain http, so a direct https check fails in
        // the TLS handshake. Through the dead proxy it would fail earlier,
        // with "connection refused".
        let https = format!("https://127.0.0.1:{}/", server.address().port());
        let outcome = perform_check(&build_http_client(), &http_monitor(https)).await;
        assert_eq!(outcome.status, CheckStatus::Down);
        let error = outcome.error.expect("a failed check carries an error");
        assert!(error.starts_with("connection failed: "), "{error}");
    });
}
