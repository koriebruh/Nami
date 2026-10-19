mod common;

use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use common::{check_context, unsaved_monitor as monitor};
use nami_engine::{
    uptime::checker::{CheckStatus, MAX_BODY_BYTES, MAX_ERROR_CHARS, perform_check},
    uptime::monitor::model::Monitor,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

fn http_monitor(url: String) -> Monitor {
    Monitor {
        url: Some(url),
        ..monitor("http")
    }
}

fn keyword_monitor(url: String, keyword: &str, mode: &str) -> Monitor {
    Monitor {
        url: Some(url),
        keyword: Some(keyword.to_string()),
        keyword_mode: mode.to_string(),
        ..monitor("keyword")
    }
}

async fn server_returning(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

/// A local port with nothing listening on it.
fn closed_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

/// A local server for behaviour wiremock cannot produce (a reset, a body that
/// never ends, connections that stay open). `handle` gets every accepted
/// connection. Returns the port and the number of connections accepted so far.
async fn raw_server<H, F>(handle: H) -> (u16, Arc<AtomicUsize>)
where
    H: Fn(tokio::net::TcpStream) -> F + Send + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(handle(stream));
        }
    });
    (port, accepted)
}

async fn read_request(stream: &mut tokio::net::TcpStream) {
    let mut buf = [0u8; 4096];
    let _ = stream.read(&mut buf).await;
}

/// Makes closing the stream send a TCP RST instead of a FIN. `set_linger` is
/// deprecated because a non-zero linger blocks the thread on drop; a zero
/// linger is the only way to force an RST from tokio's API.
#[allow(deprecated)]
fn reset_on_close(stream: &tokio::net::TcpStream) {
    stream.set_linger(Some(Duration::ZERO)).unwrap();
}

#[tokio::test]
async fn status_inside_the_expected_range_is_up() {
    let server = server_returning(ResponseTemplate::new(204)).await;
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("{}/", server.uri())),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Up);
    assert_eq!(outcome.status_code, Some(204));
    assert!(outcome.response_time_ms.is_some());
    assert_eq!(outcome.error, None);
}

#[tokio::test]
async fn status_outside_the_expected_range_is_down_with_the_reason() {
    let server = server_returning(ResponseTemplate::new(503)).await;
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("{}/", server.uri())),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.status_code, Some(503));
    assert_eq!(
        outcome.error.as_deref(),
        Some("expected status 200-399, got 503")
    );
}

#[tokio::test]
async fn a_custom_expected_range_is_honoured() {
    let server = server_returning(ResponseTemplate::new(204)).await;
    let only_200 = Monitor {
        expected_status_min: 200,
        expected_status_max: 200,
        ..http_monitor(format!("{}/", server.uri()))
    };
    let outcome = perform_check(&check_context(), &only_200).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("expected status 200-200, got 204")
    );
}

#[tokio::test]
async fn redirects_are_followed_to_the_final_status() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/old"))
        .respond_with(
            ResponseTemplate::new(301).insert_header("Location", format!("{}/new", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/new"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("{}/old", server.uri())),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Up);
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn head_monitors_send_head_requests() {
    let server = MockServer::start().await;
    // Only HEAD is mocked: a GET would get wiremock's default 404.
    Mock::given(method("HEAD"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let head = Monitor {
        http_method: "HEAD".to_string(),
        ..http_monitor(format!("{}/", server.uri()))
    };
    let outcome = perform_check(&check_context(), &head).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn keyword_present_is_up_and_absent_is_down() {
    let server =
        server_returning(ResponseTemplate::new(200).set_body_string("status: all systems go"))
            .await;
    let url = format!("{}/", server.uri());

    let present = perform_check(
        &check_context(),
        &keyword_monitor(url.clone(), "systems go", "contains"),
    )
    .await;
    assert_eq!(present.status, CheckStatus::Up, "{:?}", present.error);

    let absent = perform_check(
        &check_context(),
        &keyword_monitor(url, "maintenance", "contains"),
    )
    .await;
    assert_eq!(absent.status, CheckStatus::Down);
    assert_eq!(
        absent.error.as_deref(),
        Some("keyword \"maintenance\" not found")
    );
    assert_eq!(absent.status_code, Some(200));
}

#[tokio::test]
async fn keyword_matching_is_case_sensitive() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("Status: OK")).await;
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), "ok", "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("keyword \"ok\" not found"));
}

#[tokio::test]
async fn not_contains_mode_inverts_the_keyword_check() {
    let server =
        server_returning(ResponseTemplate::new(200).set_body_string("database error")).await;
    let url = format!("{}/", server.uri());

    let found = perform_check(
        &check_context(),
        &keyword_monitor(url.clone(), "error", "not_contains"),
    )
    .await;
    assert_eq!(found.status, CheckStatus::Down);
    assert_eq!(found.error.as_deref(), Some("keyword \"error\" found"));

    let missing = perform_check(
        &check_context(),
        &keyword_monitor(url, "panic", "not_contains"),
    )
    .await;
    assert_eq!(missing.status, CheckStatus::Up, "{:?}", missing.error);
}

#[tokio::test]
async fn a_keyword_beyond_the_body_cap_is_not_found() {
    let body = format!("{}NEEDLE", "a".repeat(MAX_BODY_BYTES));
    let server = server_returning(ResponseTemplate::new(200).set_body_string(body)).await;
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    // Down because the keyword was not found, not because the read failed.
    assert_eq!(
        outcome.error.as_deref(),
        Some("keyword \"NEEDLE\" not found")
    );
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn a_keyword_ending_exactly_at_the_body_cap_is_found() {
    let padding = "a".repeat(MAX_BODY_BYTES - "NEEDLE".len());
    let body = format!("{padding}NEEDLE");
    assert_eq!(body.len(), MAX_BODY_BYTES);
    let server = server_returning(ResponseTemplate::new(200).set_body_string(body)).await;
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn a_keyword_ending_one_byte_past_the_body_cap_is_not_found() {
    let padding = "a".repeat(MAX_BODY_BYTES - "NEEDLE".len() + 1);
    let body = format!("{padding}NEEDLE");
    assert_eq!(body.len(), MAX_BODY_BYTES + 1);
    let server = server_returning(ResponseTemplate::new(200).set_body_string(body)).await;
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), "NEEDLE", "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("keyword \"NEEDLE\" not found")
    );
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn an_endless_body_is_cut_at_the_cap_instead_of_being_read_forever() {
    let (port, _) = raw_server(|mut stream| async move {
        read_request(&mut stream).await;
        // No content-length and no chunking: the body runs until the
        // connection closes, and this server never closes it. The write
        // fails once the client drops the connection.
        let head = b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n";
        if stream.write_all(head).await.is_err() {
            return;
        }
        let block = vec![b'a'; 64 * 1024];
        while stream.write_all(&block).await.is_ok() {}
    })
    .await;
    let endless = Monitor {
        timeout_seconds: 5,
        ..keyword_monitor(format!("http://127.0.0.1:{port}/"), "needle", "contains")
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &endless).await;
    // Reading stops at the cap: the check ends in milliseconds, long before
    // the 5 s deadline, and with "not found" rather than a timeout.
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("keyword \"needle\" not found")
    );
    assert_eq!(outcome.status_code, Some(200));
}

#[tokio::test]
async fn keyword_monitors_send_get_when_head_is_configured() {
    // Only GET is mocked: a HEAD would get wiremock's default 404, and a
    // HEAD response has no body to search anyway.
    let server =
        server_returning(ResponseTemplate::new(200).set_body_string("database error")).await;
    let url = format!("{}/", server.uri());

    let present = Monitor {
        http_method: "HEAD".to_string(),
        ..keyword_monitor(url.clone(), "database", "contains")
    };
    let outcome = perform_check(&check_context(), &present).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);

    // Without a body, not_contains would pass every page: it must see the body.
    let forbidden = Monitor {
        http_method: "HEAD".to_string(),
        ..keyword_monitor(url, "error", "not_contains")
    };
    let outcome = perform_check(&check_context(), &forbidden).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("keyword \"error\" found"));
}

#[tokio::test]
async fn error_text_is_capped_however_it_is_produced() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("ok")).await;
    let long_keyword = "k".repeat(10_000);
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), &long_keyword, "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let error = outcome.error.expect("a failed check carries an error");
    assert_eq!(error.chars().count(), MAX_ERROR_CHARS + 1);
    assert!(error.starts_with("keyword \"kkkk"), "{error}");
    assert!(error.ends_with('…'), "{error}");
}

#[tokio::test]
async fn a_keyword_match_still_needs_an_expected_status() {
    let server =
        server_returning(ResponseTemplate::new(500).set_body_string("all systems go")).await;
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), "systems go", "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("expected status 200-399, got 500")
    );
}

#[tokio::test]
async fn a_slow_response_times_out_within_the_monitor_timeout() {
    let server =
        server_returning(ResponseTemplate::new(200).set_delay(Duration::from_secs(5))).await;
    let slow = Monitor {
        timeout_seconds: 1,
        ..http_monitor(format!("{}/", server.uri()))
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &slow).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
    assert_eq!(outcome.response_time_ms, None);
}

#[tokio::test]
async fn http_connection_refused_is_down() {
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("http://127.0.0.1:{}/", closed_port())),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn tcp_open_port_is_up() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let tcp = Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(port)),
        ..monitor("tcp")
    };
    let outcome = perform_check(&check_context(), &tcp).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
    drop(listener);
}

#[tokio::test]
async fn tcp_closed_port_is_down_with_connection_refused() {
    let tcp = Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(closed_port())),
        ..monitor("tcp")
    };
    let outcome = perform_check(&check_context(), &tcp).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

#[tokio::test]
async fn a_redirect_loop_is_down_with_too_many_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("Location", format!("{}/", server.uri())),
        )
        .mount(&server)
        .await;
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("{}/", server.uri())),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("too many redirects"));
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn a_reset_connection_is_down() {
    let (port, _) = raw_server(|mut stream| async move {
        read_request(&mut stream).await;
        reset_on_close(&stream);
    })
    .await;
    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("http://127.0.0.1:{port}/")),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection reset"));
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn a_failed_tls_handshake_is_down_with_the_cause() {
    // https against a plain-http port: the TCP connect succeeds, the TLS
    // handshake does not.
    let server = MockServer::start().await;
    let target = http_monitor(format!("https://127.0.0.1:{}/", server.address().port()));
    let outcome = perform_check(&check_context(), &target).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let error = outcome.error.expect("a failed check carries an error");
    assert!(error.starts_with("connection failed: "), "{error}");
}

#[tokio::test]
async fn a_body_that_never_finishes_times_out_within_the_monitor_timeout() {
    let (port, _) = raw_server(|mut stream| async move {
        read_request(&mut stream).await;
        // Promises a megabyte, sends five bytes, then goes silent.
        let _ = stream
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 1000000\r\n\r\nhello")
            .await;
        tokio::time::sleep(Duration::from_secs(10)).await;
    })
    .await;
    let stalled = Monitor {
        timeout_seconds: 1,
        ..keyword_monitor(format!("http://127.0.0.1:{port}/"), "needle", "contains")
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &stalled).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
    // The one deadline around the whole check fired, so nothing is reported
    // from the half-finished response: no status code, no response time.
    assert_eq!(outcome.status_code, None);
    assert_eq!(outcome.response_time_ms, None);
}

#[tokio::test]
async fn every_check_opens_a_fresh_connection() {
    let (port, accepted) = raw_server(|mut stream| async move {
        // Keep-alive: answer every request that arrives on this connection.
        let mut buf = [0u8; 4096];
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0
                || stream
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                    .await
                    .is_err()
            {
                break;
            }
        }
    })
    .await;
    let ctx = check_context();
    let target = http_monitor(format!("http://127.0.0.1:{port}/"));
    for _ in 0..2 {
        let outcome = perform_check(&ctx, &target).await;
        assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    }
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the second check reused a pooled connection"
    );
}

#[tokio::test]
async fn a_monitor_that_cannot_be_checked_is_down_instead_of_panicking() {
    let local = || Some("127.0.0.1".to_string());
    // Never contacted: these monitors are rejected before any request is made.
    let unused_url = || Some(format!("http://127.0.0.1:{}/", closed_port()));
    let cases = [
        (monitor("http"), "monitor has no url"),
        (monitor("keyword"), "monitor has no url"),
        (
            Monitor {
                url: unused_url(),
                ..monitor("keyword")
            },
            "monitor has no keyword",
        ),
        (
            Monitor {
                url: unused_url(),
                keyword: Some(String::new()),
                ..monitor("keyword")
            },
            "monitor has no keyword",
        ),
        (monitor("tcp"), "monitor has no host/port"),
        (
            Monitor {
                host: local(),
                ..monitor("tcp")
            },
            "monitor has no host/port",
        ),
        (
            Monitor {
                port: Some(80),
                ..monitor("tcp")
            },
            "monitor has no host/port",
        ),
        (
            Monitor {
                host: local(),
                port: Some(70_000),
                ..monitor("tcp")
            },
            "invalid port 70000",
        ),
        (
            Monitor {
                host: local(),
                port: Some(-1),
                ..monitor("tcp")
            },
            "invalid port -1",
        ),
        (monitor("icmp"), "unsupported check type \"icmp\""),
    ];
    let ctx = check_context();
    for (case, expected) in cases {
        let outcome = perform_check(&ctx, &case).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{expected}");
        assert_eq!(outcome.error.as_deref(), Some(expected));
        assert_eq!(outcome.status_code, None, "{expected}");
        assert_eq!(outcome.response_time_ms, None, "{expected}");
    }
}

#[test]
fn check_status_serializes_as_the_clickhouse_enum_labels() {
    // `uptime_checks.status` is `Enum8('up' = 1, 'down' = 2)`.
    assert_eq!(serde_json::to_string(&CheckStatus::Up).unwrap(), "\"up\"");
    assert_eq!(
        serde_json::to_string(&CheckStatus::Down).unwrap(),
        "\"down\""
    );
    assert_eq!(
        serde_json::from_str::<CheckStatus>("\"up\"").unwrap(),
        CheckStatus::Up
    );
    assert_eq!(
        serde_json::from_str::<CheckStatus>("\"down\"").unwrap(),
        CheckStatus::Down
    );
}

// ---------------------------------------------------------------------------
// Final review: error text cleaned at the source, and destinations no check
// may contact (link-local and cloud metadata addresses, "this network").
// ---------------------------------------------------------------------------

/// Error text is one line that reads as written: control characters, the line
/// and paragraph separators and the bidirectional controls become spaces,
/// wherever they come from (here: the keyword, quoted in the error).
#[tokio::test]
async fn error_text_carries_no_line_breaks_or_direction_controls() {
    let server = server_returning(ResponseTemplate::new(200).set_body_string("ok")).await;
    let keyword = "a\u{2028}b\u{2029}c\u{202E}d\u{2066}e\u{200F}f\u{061C}g";
    let outcome = perform_check(
        &check_context(),
        &keyword_monitor(format!("{}/", server.uri()), keyword, "contains"),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("keyword \"a b c d e f g\" not found")
    );
}

const NOT_ALLOWED: &str = "destination not allowed";

/// Asserts that `outcome` is a refusal: down, the generic message, and no
/// status code or response time, since nothing was contacted.
fn assert_refused(outcome: &nami_engine::uptime::checker::CheckOutcome, label: &str) {
    assert_eq!(outcome.status, CheckStatus::Down, "{label}");
    assert_eq!(outcome.error.as_deref(), Some(NOT_ALLOWED), "{label}");
    assert_eq!(outcome.status_code, None, "{label}");
    assert_eq!(outcome.response_time_ms, None, "{label}");
}

/// Link-local addresses (where cloud metadata services answer), "this
/// network" and the unspecified address are refused when the URL names them
/// directly, in IPv4, IPv6 or IPv4-mapped form, before any request is made.
#[tokio::test]
async fn literal_metadata_and_link_local_addresses_are_refused() {
    for url in [
        "http://169.254.169.254/latest/meta-data/",
        "http://169.254.0.1/",
        "http://[fe80::1]/",
        "http://[fd00:ec2::254]/latest/meta-data/",
        "http://[::ffff:169.254.169.254]/",
        "http://[::ffff:a9fe:a9fe]/",
        "http://0x0a9fea9fe/",
        "http://2852039166/",
        "http://0.0.0.0/",
        "http://0.1.2.3/",
        "http://[::]/",
    ] {
        let started = Instant::now();
        let outcome = perform_check(&check_context(), &http_monitor(url.to_string())).await;
        assert_refused(&outcome, url);
        assert!(started.elapsed() < Duration::from_secs(1), "{url}");
    }
}

/// `0.0.0.0` reaches this machine's own services, so a refusal is the only
/// thing standing between the check and a server that would answer: here one
/// that answers 200.
#[tokio::test]
async fn this_network_is_refused_even_where_a_server_would_answer() {
    let server = server_returning(ResponseTemplate::new(200)).await;
    let port = server.address().port();
    for url in [
        format!("http://0.0.0.0:{port}/"),
        format!("http://[::ffff:0.0.0.0]:{port}/"),
    ] {
        let outcome = perform_check(&check_context(), &http_monitor(url.clone())).await;
        assert_refused(&outcome, &url);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Every redirect hop is checked: a redirect to a metadata address (or to
/// this network, where a server would answer) is refused, not followed.
#[tokio::test]
async fn a_redirect_to_a_refused_address_is_not_followed() {
    let target = server_returning(ResponseTemplate::new(200)).await;
    let port = target.address().port();
    for location in [
        "http://169.254.169.254/latest/meta-data/iam/security-credentials/".to_string(),
        "http://[fd00:ec2::254]/".to_string(),
        format!("http://0.0.0.0:{port}/"),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", location.as_str()))
            .mount(&server)
            .await;
        let outcome = perform_check(
            &check_context(),
            &http_monitor(format!("{}/", server.uri())),
        )
        .await;
        assert_refused(&outcome, &location);
    }
    assert!(target.received_requests().await.unwrap().is_empty());
}

/// tcp checks resolve the host and refuse the same addresses, literal or
/// IPv4-mapped; on `0.0.0.0` a listener would otherwise accept.
#[tokio::test]
async fn tcp_checks_refuse_the_same_addresses() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = i32::from(listener.local_addr().unwrap().port());
    for (host, port) in [
        ("169.254.169.254", 80),
        ("fe80::1", 22),
        ("::ffff:169.254.169.254", 80),
        ("fd00:ec2::254", 80),
        ("0.0.0.0", port),
        ("::", port),
    ] {
        let tcp = Monitor {
            host: Some(host.to_string()),
            port: Some(port),
            ..monitor("tcp")
        };
        let started = Instant::now();
        let outcome = perform_check(&check_context(), &tcp).await;
        assert_refused(&outcome, host);
        assert!(started.elapsed() < Duration::from_secs(1), "{host}");
    }
    drop(listener);
}

/// Everything else stays allowed, loopback and names that resolve to it
/// included: internal services are legitimate targets.
#[tokio::test]
async fn loopback_by_name_or_address_is_still_checked() {
    let server = server_returning(ResponseTemplate::new(200)).await;
    let port = server.address().port();
    for url in [
        format!("http://127.0.0.1:{port}/"),
        format!("http://localhost:{port}/"),
    ] {
        let outcome = perform_check(&check_context(), &http_monitor(url.clone())).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Up,
            "{url}: {:?}",
            outcome.error
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tcp = Monitor {
        host: Some("localhost".to_string()),
        port: Some(i32::from(listener.local_addr().unwrap().port())),
        ..monitor("tcp")
    };
    let outcome = perform_check(&check_context(), &tcp).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}
