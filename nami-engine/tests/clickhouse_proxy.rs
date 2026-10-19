//! Proxy environment variables must never reroute ClickHouse requests: they
//! carry the ClickHouse credentials (and every query parameter), which have no
//! business passing through a proxy the engine's environment happens to name.
//!
//! This test has its own binary because it changes process-wide environment
//! variables. Alone in its process, it can do that before any other thread
//! exists, and no other test builds a client while the variables are set.

use nami_engine::core::clickhouse::ClickHouse;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

#[test]
fn proxy_environment_variables_never_reroute_clickhouse_requests() {
    // Nothing listens on this port: a client that honours the proxy variables
    // fails to connect instead of reaching ClickHouse.
    let dead_proxy = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port())
    };
    // SAFETY: this is the only test in the binary, and it changes the
    // environment before it starts the runtime, so no thread of this process
    // that reads the environment exists yet.
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
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{\"n\":1}\n"))
            .mount(&server)
            .await;
        let ch = ClickHouse::new(&server.uri(), "nami", "s3cret", "nami_guard_probe");

        ch.execute("SELECT 1")
            .await
            .expect("reached ClickHouse directly");
        let rows: Vec<serde_json::Value> = ch
            .query_rows("SELECT 1 AS n", &[])
            .await
            .expect("reached ClickHouse directly");

        assert_eq!(rows, vec![serde_json::json!({ "n": 1 })]);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    });
}
