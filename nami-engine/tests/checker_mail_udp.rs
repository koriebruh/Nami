// `smtp`/`pop3`/`imap` banner checks against local TCP (and TLS) servers,
// and `udp` checks against local UDP responders.
mod common;

use std::time::{Duration, Instant};

use common::{check_context, tls, unsaved_monitor};
use nami_engine::{
    uptime::monitor::model::Monitor,
    uptime::{
        checker::tls::client_config,
        checker::{CheckContext, CheckOutcome, CheckStatus, perform_check},
    },
};
use tokio::{io::AsyncWriteExt, net::TcpListener};

/// A TCP server that writes `chunks` (pausing 50 ms between them) to every
/// connection, then keeps it open for 2 s. `None` = never write anything.
async fn banner_server(chunks: Option<Vec<&'static str>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let chunks = chunks.clone();
            tokio::spawn(async move {
                for chunk in chunks.unwrap_or_default() {
                    let _ = stream.write_all(chunk.as_bytes()).await;
                    let _ = stream.flush().await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            });
        }
    });
    port
}

fn mail_monitor(check_type: &str, port: u16) -> Monitor {
    Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(port)),
        ..unsaved_monitor(check_type)
    }
}

async fn check(monitor: &Monitor) -> CheckOutcome {
    perform_check(&check_context(), monitor).await
}

#[tokio::test]
async fn each_protocol_is_up_on_its_ready_greeting() {
    let cases = [
        ("smtp", "220 mail.test ESMTP Postfix\r\n"),
        (
            "smtp",
            "220-mail.test first line of a multi-line greeting\r\n",
        ),
        ("pop3", "+OK POP3 server ready\r\n"),
        ("imap", "* OK [CAPABILITY IMAP4rev1] ready\r\n"),
        ("imap", "* PREAUTH IMAP4rev1 logged in\r\n"),
    ];
    for (check_type, greeting) in cases {
        let port = banner_server(Some(vec![greeting])).await;
        let outcome = check(&mail_monitor(check_type, port)).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Up,
            "{check_type} {greeting:?}: {:?}",
            outcome.error
        );
        assert!(outcome.response_time_ms.is_some());
    }
}

#[tokio::test]
async fn a_greeting_split_across_writes_is_read_whole() {
    let port = banner_server(Some(vec!["2", "20 slow.test ESMTP\r\n"])).await;
    let outcome = check(&mail_monitor("smtp", port)).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn a_wrong_greeting_is_down_and_quoted_without_control_characters() {
    let cases = [
        (
            "smtp",
            "554 mail.test no service\r\n",
            "unexpected greeting: 554 mail.test no service",
        ),
        (
            "pop3",
            "-ERR too busy\r\n",
            "unexpected greeting: -ERR too busy",
        ),
        (
            "imap",
            "* BYE shutting down\r\n",
            "unexpected greeting: * BYE shutting down",
        ),
        ("smtp", "421 \u{7}bell\r\n", "unexpected greeting: 421 bell"),
        (
            "imap",
            "+OK this is pop3\r\n",
            "unexpected greeting: +OK this is pop3",
        ),
    ];
    for (check_type, greeting, error) in cases {
        let port = banner_server(Some(vec![greeting])).await;
        let outcome = check(&mail_monitor(check_type, port)).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Down,
            "{check_type} {greeting:?}"
        );
        assert_eq!(outcome.error.as_deref(), Some(error));
    }
}

#[tokio::test]
async fn a_silent_server_times_out_and_a_closing_one_is_down() {
    let silent = banner_server(None).await;
    let monitor = Monitor {
        timeout_seconds: 1,
        ..mail_monitor("smtp", silent)
    };
    let started = Instant::now();
    let outcome = check(&monitor).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let closing = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    });
    let outcome = check(&mail_monitor("pop3", closing)).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("connection closed before the server greeting")
    );

    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let outcome = check(&mail_monitor("imap", closed)).await;
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

#[tokio::test]
async fn use_tls_reads_the_greeting_over_implicit_tls() {
    let ca = tls::TestCa::new();
    let not_after = time::OffsetDateTime::now_utc() + time::Duration::days(90);
    let port = tls::serve_tls_banner(ca.leaf(not_after), "* OK IMAPS ready\r\n").await;
    let ctx = CheckContext {
        tls: client_config(ca.roots()),
        ..check_context()
    };
    let monitor = Monitor {
        host: Some("localhost".to_string()),
        port: Some(i32::from(port)),
        use_tls: true,
        ..unsaved_monitor("imap")
    };
    let outcome = perform_check(&ctx, &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);

    let untrusted = tls::serve_tls_banner(tls::self_signed(), "* OK IMAPS ready\r\n").await;
    let monitor = Monitor {
        port: Some(i32::from(untrusted)),
        ..monitor
    };
    let outcome = perform_check(&ctx, &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("certificate is not trusted (unknown issuer)")
    );
}

/// A UDP responder on 127.0.0.1 that answers every datagram with
/// `PONG <datagram>` (or never answers when `silent`).
async fn udp_responder(silent: bool) -> u16 {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        while let Ok((len, peer)) = socket.recv_from(&mut buf).await {
            if !silent {
                let reply = [b"PONG ".as_slice(), &buf[..len]].concat();
                let _ = socket.send_to(&reply, peer).await;
            }
        }
    });
    port
}

fn udp_monitor(port: u16, keyword: Option<&str>, mode: &str) -> Monitor {
    Monitor {
        host: Some("127.0.0.1".to_string()),
        port: Some(i32::from(port)),
        udp_payload: Some("ping\n".to_string()),
        keyword: keyword.map(str::to_string),
        keyword_mode: mode.to_string(),
        ..unsaved_monitor("udp")
    }
}

#[tokio::test]
async fn a_udp_reply_is_up_and_the_keyword_is_matched_against_it() {
    let port = udp_responder(false).await;

    let any = check(&udp_monitor(port, None, "contains")).await;
    assert_eq!(any.status, CheckStatus::Up, "{:?}", any.error);
    assert!(any.response_time_ms.is_some());

    let echoed = check(&udp_monitor(port, Some("PONG ping"), "contains")).await;
    assert_eq!(
        echoed.status,
        CheckStatus::Up,
        "the payload was sent as given: {:?}",
        echoed.error
    );

    let missing = check(&udp_monitor(port, Some("NOPE"), "contains")).await;
    assert_eq!(missing.status, CheckStatus::Down);
    assert_eq!(
        missing.error.as_deref(),
        Some("keyword \"NOPE\" not found in the reply")
    );

    let unwanted = check(&udp_monitor(port, Some("PONG"), "not_contains")).await;
    assert_eq!(unwanted.status, CheckStatus::Down);
    assert_eq!(
        unwanted.error.as_deref(),
        Some("keyword \"PONG\" found in the reply")
    );
}

#[tokio::test]
async fn no_udp_reply_within_the_timeout_is_down() {
    let port = udp_responder(true).await;
    let monitor = Monitor {
        timeout_seconds: 1,
        ..udp_monitor(port, None, "contains")
    };
    let started = Instant::now();
    let outcome = check(&monitor).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
}

#[tokio::test]
async fn a_closed_udp_port_is_refused() {
    let closed = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let outcome = check(&udp_monitor(closed, None, "contains")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("connection refused (port unreachable)")
    );
}

const NOT_ALLOWED: &str = "destination not allowed";

#[tokio::test]
async fn mail_and_udp_checks_refuse_link_local_and_metadata_destinations() {
    for host in [
        "169.254.169.254",
        "fe80::1",
        "::ffff:169.254.169.254",
        "0.0.0.0",
        "2852039166",
    ] {
        for check_type in ["smtp", "pop3", "imap"] {
            let monitor = Monitor {
                host: Some(host.to_string()),
                port: Some(25),
                ..unsaved_monitor(check_type)
            };
            let outcome = check(&monitor).await;
            assert_eq!(
                outcome.error.as_deref(),
                Some(NOT_ALLOWED),
                "{check_type} {host}"
            );
            // A numeric spelling is no valid TLS server name: refused either way.
            if host != "2852039166" {
                let tls = Monitor {
                    use_tls: true,
                    ..monitor
                };
                let outcome = check(&tls).await;
                assert_eq!(
                    outcome.error.as_deref(),
                    Some(NOT_ALLOWED),
                    "{check_type} tls {host}"
                );
            }
        }
        let monitor = Monitor {
            host: Some(host.to_string()),
            ..udp_monitor(9, None, "contains")
        };
        let outcome = check(&monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "udp {host}");
        assert_eq!(outcome.error.as_deref(), Some(NOT_ALLOWED), "udp {host}");
        assert_eq!(outcome.response_time_ms, None, "udp {host}");
    }
}

#[tokio::test]
async fn an_endless_unterminated_greeting_is_cut_and_the_error_stays_short() {
    // 100 KiB without a newline, in chunks: the check must not wait for a
    // line break, and must quote only a short, cleaned prefix.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let chunk = [b'A'; 1024];
                for _ in 0..100 {
                    if stream.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            });
        }
    });
    let outcome = check(&mail_monitor("smtp", port)).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let error = outcome.error.unwrap();
    assert!(error.starts_with("unexpected greeting: AAAA"), "{error}");
    assert!(
        error.chars().count() < 150,
        "{} chars",
        error.chars().count()
    );
    assert!(error.ends_with('…'), "{error}");
}

#[tokio::test]
async fn a_greeting_cannot_smuggle_direction_controls_or_extra_lines_into_the_error() {
    let port = banner_server(Some(vec![
        "421 a\u{202e}evil\u{2028}b\x1b[31mred\r\n250 second line\r\n",
    ]))
    .await;
    let outcome = check(&mail_monitor("smtp", port)).await;
    let error = outcome.error.unwrap();
    assert!(!error.contains("second line"), "{error}");
    assert!(
        !error
            .chars()
            .any(nami_engine::uptime::checker::is_unsafe_in_text),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_status_word_must_stand_alone() {
    for (check_type, greeting) in [
        ("smtp", "2200 nope\r\n"),
        ("pop3", "+OKAY\r\n"),
        ("imap", "* OKAY\r\n"),
    ] {
        let port = banner_server(Some(vec![greeting])).await;
        let outcome = check(&mail_monitor(check_type, port)).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Down,
            "{check_type} {greeting:?}"
        );
    }
}

#[tokio::test]
async fn a_binary_safe_udp_payload_is_sent_verbatim() {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let seen = tokio::spawn(async move {
        let mut buf = [0u8; 2048];
        let (len, peer) = socket.recv_from(&mut buf).await.unwrap();
        let _ = socket.send_to(b"ok", peer).await;
        buf[..len].to_vec()
    });
    let monitor = Monitor {
        udp_payload: Some("\u{ff}\u{ff}status\r\n".to_string()),
        ..udp_monitor(port, None, "contains")
    };
    let outcome = check(&monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(seen.await.unwrap(), "\u{ff}\u{ff}status\r\n".as_bytes());
}
