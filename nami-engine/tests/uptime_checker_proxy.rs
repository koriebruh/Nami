#![allow(unsafe_code)] // `std::env::set_var` is `unsafe` in edition 2024; this binary owns its process.

//! Proxy environment variables must never reroute uptime checks.
//!
//! This test has its own binary because it changes process-wide environment
//! variables. Alone in its process, it can do that before any other thread
//! exists, and no other test builds a client while the variables are set.
mod common;

use common::{check_context, proxy::FakeProxy, unsaved_monitor, with_headers, with_proxy};
use nami_engine::{
    uptime::checker::{CheckStatus, perform_check},
    uptime::monitor::model::Monitor,
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn http_monitor(url: String) -> Monitor {
    Monitor {
        url: Some(url),
        ..unsaved_monitor("http")
    }
}

/// One monitor for every client the checker keeps: redirects off, redirects on,
/// and redirects on for a monitor that carries secrets (same origin only), each
/// with certificates verified or not.
fn one_monitor_per_client(url: &str) -> Vec<(String, Monitor)> {
    let mut monitors = Vec::new();
    for verify_ssl in [true, false] {
        for (label, follow_redirects, secrets) in [
            ("redirects off", false, false),
            ("redirects on", true, false),
            ("redirects on, with secrets", true, true),
        ] {
            let monitor = Monitor {
                follow_redirects,
                verify_ssl,
                ..http_monitor(url.to_string())
            };
            let monitor = if secrets {
                with_headers(monitor, &[("X-Api-Key", "proxy-test-key")])
            } else {
                monitor
            };
            monitors.push((format!("{label}, verify_ssl={verify_ssl}"), monitor));
        }
    }
    monitors
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

        // Every client the checker keeps must go around the proxy variables,
        // not just the one plain checks use.
        let ctx = check_context();
        let http = format!("{}/", server.uri());
        // The mock server speaks plain http, so a direct https check fails in
        // the TLS handshake. Through the dead proxy it would fail earlier,
        // with "connection refused".
        let https = format!("https://127.0.0.1:{}/", server.address().port());
        for ((label, direct), (_, tls)) in one_monitor_per_client(&http)
            .into_iter()
            .zip(one_monitor_per_client(&https))
        {
            let outcome = perform_check(&ctx, &direct).await;
            assert_eq!(
                outcome.status,
                CheckStatus::Up,
                "{label}: {:?}",
                outcome.error
            );
            assert_eq!(outcome.status_code, Some(200), "{label}");

            let outcome = perform_check(&ctx, &tls).await;
            assert_eq!(outcome.status, CheckStatus::Down, "{label}");
            let error = outcome.error.expect("a failed check carries an error");
            assert!(error.starts_with("connection failed: "), "{label}: {error}");
        }

        // Per-check clients (a cookie jar) go around the variables too.
        let jar = Monitor {
            remember_cookies: true,
            ..http_monitor(http.clone())
        };
        let outcome = perform_check(&ctx, &jar).await;
        assert_eq!(outcome.status, CheckStatus::Up, "jar: {:?}", outcome.error);

        // A monitor's own proxy wins over the variables, which stay ignored:
        // the check goes to the explicit proxy, not the dead one.
        let explicit =
            FakeProxy::start("HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
                .await;
        for (label, monitor) in [
            (
                "plain",
                http_monitor("http://target.test/via-proxy".to_string()),
            ),
            (
                "with a jar and secrets",
                with_headers(
                    Monitor {
                        remember_cookies: true,
                        ..http_monitor("http://target.test/via-proxy".to_string())
                    },
                    &[("X-Api-Key", "proxy-test-key")],
                ),
            ),
        ] {
            let monitor = with_proxy(
                monitor,
                &format!("http://127.0.0.1:{}", explicit.port),
                Some(("nami", "pr0xy")),
            );
            let outcome = perform_check(&ctx, &monitor).await;
            assert_eq!(
                outcome.status,
                CheckStatus::Up,
                "{label}: {:?}",
                outcome.error
            );
        }
        let seen = explicit.requests();
        assert_eq!(seen.len(), 2);
        assert!(
            seen.iter()
                .all(|r| r.request_line == "GET http://target.test/via-proxy HTTP/1.1")
        );
    });
}
