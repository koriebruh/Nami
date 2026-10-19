// `ping` and `dns` checks. Ping needs unprivileged ICMP sockets (macOS: always;
// Linux: the test runner's group inside net.ipv4.ping_group_range). DNS checks
// run against an in-test stub nameserver: no internet access needed.
use crate::common;

use std::{
    net::SocketAddr,
    str::FromStr,
    time::{Duration, Instant},
};

use common::{check_context, unsaved_monitor};
use hickory_resolver::proto::{
    op::{Message, ResponseCode},
    rr::{Name, RData, Record, RecordType, rdata},
};
use nami_engine::{
    uptime::checker::{CheckOutcome, CheckStatus, MAX_ERROR_CHARS, perform_check},
    uptime::monitor::model::Monitor,
};

const NOT_ALLOWED: &str = "destination not allowed";

fn ping_monitor(host: &str) -> Monitor {
    Monitor {
        host: Some(host.to_string()),
        ..unsaved_monitor("ping")
    }
}

/// Asserts that `outcome` is a refusal: down, the generic message, and no
/// status code or response time, since nothing was contacted.
fn assert_refused(outcome: &CheckOutcome, label: &str) {
    assert_eq!(outcome.status, CheckStatus::Down, "{label}");
    assert_eq!(outcome.error.as_deref(), Some(NOT_ALLOWED), "{label}");
    assert_eq!(outcome.status_code, None, "{label}");
    assert_eq!(outcome.response_time_ms, None, "{label}");
}

#[tokio::test]
async fn pinging_localhost_is_up_with_a_round_trip_time() {
    let outcome = perform_check(&check_context(), &ping_monitor("127.0.0.1")).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
    assert_eq!(outcome.status_code, None);
}

#[tokio::test]
async fn pinging_a_name_that_resolves_to_loopback_is_up() {
    // Loopback and private networks stay allowed: monitoring internal hosts
    // is what a self-hosted monitor is for.
    let outcome = perform_check(&check_context(), &ping_monitor("localhost")).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn pinging_an_unroutable_address_is_down_within_the_timeout() {
    // 192.0.2.0/24 is TEST-NET-1 (RFC 5737): nothing ever answers.
    let monitor = Monitor {
        timeout_seconds: 1,
        ..ping_monitor("192.0.2.1")
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &monitor).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(outcome.error.is_some());
    assert_eq!(outcome.status_code, None);
    assert_eq!(outcome.response_time_ms, None, "no reply, no round trip");
}

#[tokio::test]
async fn pinging_a_name_that_does_not_resolve_is_down() {
    // .invalid never resolves (RFC 2606).
    let outcome = perform_check(&check_context(), &ping_monitor("no-such-host.invalid")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .starts_with("could not resolve no-such-host.invalid"),
        "{:?}",
        outcome.error
    );
}

#[tokio::test]
async fn a_ping_monitor_without_a_host_is_down() {
    let outcome = perform_check(&check_context(), &unsaved_monitor("ping")).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("monitor has no host"));
}

/// Link-local addresses (where cloud metadata services answer), "this
/// network" and the unspecified address are refused when the monitor names
/// them, in IPv4, IPv6 or IPv4-mapped form. In-memory monitors skip the API's
/// validation: the check itself is what is under test.
#[tokio::test]
async fn ping_refuses_link_local_metadata_and_this_network_addresses() {
    for host in [
        "169.254.169.254",
        "169.254.0.1",
        "fe80::1",
        "fd00:ec2::254",
        "::ffff:169.254.169.254",
        "::ffff:a9fe:a9fe",
        "0.0.0.0",
        "0.1.2.3",
        "::",
        "::ffff:0.0.0.0",
        "64:ff9b::a9fe:a9fe",
    ] {
        let outcome = perform_check(&check_context(), &ping_monitor(host)).await;
        assert_refused(&outcome, host);
    }
}

/// The system resolver reads these as the metadata address or "this
/// network" (decimal, hex, octal and short forms): a name is judged by what
/// it resolves to, not by how it is spelled.
#[tokio::test]
async fn ping_refuses_a_name_that_resolves_to_a_refused_address() {
    for host in [
        "2852039166",
        "0xa9fea9fe",
        "0251.0376.0251.0376",
        "169.254.43518",
        "0",
    ] {
        let outcome = perform_check(&check_context(), &ping_monitor(host)).await;
        assert_refused(&outcome, host);
    }
}

/// A UDP nameserver on 127.0.0.1 answering from a fixed zone:
/// `ok.test` has A/MX/TXT/CNAME/NS records (no AAAA), `evil.test` a TXT record
/// with control and direction characters, `broken.test` answers SERVFAIL,
/// `missing.test` NXDOMAIN, `silent.test` never answers and `garbage.test`
/// answers with bytes that are no DNS message.
async fn stub_nameserver() -> SocketAddr {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = socket.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((len, peer)) = socket.recv_from(&mut buf).await else {
                return;
            };
            let Ok(request) = Message::from_vec(&buf[..len]) else {
                continue;
            };
            let Some(query) = request.queries.first().cloned() else {
                continue;
            };
            let mut response = Message::response(request.metadata.id, request.metadata.op_code);
            response.metadata.recursion_desired = request.metadata.recursion_desired;
            response.metadata.recursion_available = true;
            response.add_query(query.clone());
            let name = query.name().clone();
            let answer = |data: RData| Record::from_rdata(name.clone(), 60, data);
            let target = |s: &str| Name::from_ascii(s).unwrap();
            match (name.to_ascii().as_str(), query.query_type()) {
                ("ok.test.", RecordType::A) => {
                    response.add_answer(answer(RData::A(rdata::A::from_str("10.0.0.7").unwrap())));
                }
                ("ok.test.", RecordType::MX) => {
                    response.add_answer(answer(RData::MX(rdata::MX::new(
                        10,
                        target("Mail.OK.test."),
                    ))));
                }
                ("ok.test.", RecordType::TXT) => {
                    let txt = rdata::TXT::new(vec![
                        "v=spf1 ".to_string(),
                        "include:_spf.ok.test -all".to_string(),
                    ]);
                    response.add_answer(answer(RData::TXT(txt)));
                }
                ("ok.test.", RecordType::CNAME) => {
                    response
                        .add_answer(answer(RData::CNAME(rdata::CNAME(target("edge.cdn.test.")))));
                }
                ("ok.test.", RecordType::NS) => {
                    response.add_answer(answer(RData::NS(rdata::NS(target("ns1.ok.test.")))));
                }
                ("evil.test.", RecordType::TXT) => {
                    let txt = rdata::TXT::new(vec!["line1\nline2\u{202e}rtl".to_string()]);
                    response.add_answer(answer(RData::TXT(txt)));
                }
                ("broken.test.", _) => response.metadata.response_code = ResponseCode::ServFail,
                ("missing.test.", _) => response.metadata.response_code = ResponseCode::NXDomain,
                ("silent.test.", _) => continue,
                ("garbage.test.", _) => {
                    let _ = socket.send_to(b"\x00\x01 not a dns message", peer).await;
                    continue;
                }
                _ => {}
            }
            let _ = socket.send_to(&response.to_vec().unwrap(), peer).await;
        }
    });
    addr
}

fn dns_monitor(
    nameserver: SocketAddr,
    host: &str,
    record_type: &str,
    expected: Option<&str>,
) -> Monitor {
    Monitor {
        host: Some(host.to_string()),
        dns_record_type: Some(record_type.to_string()),
        dns_nameserver: Some(nameserver.to_string()),
        expected_value: expected.map(str::to_string),
        ..unsaved_monitor("dns")
    }
}

#[tokio::test]
async fn a_resolving_record_is_up_and_an_expected_value_must_be_in_an_answer() {
    let ns = stub_nameserver().await;

    let any = perform_check(&check_context(), &dns_monitor(ns, "ok.test", "A", None)).await;
    assert_eq!(any.status, CheckStatus::Up, "{:?}", any.error);
    assert!(any.response_time_ms.is_some());
    assert_eq!(any.status_code, None);

    let matching = perform_check(
        &check_context(),
        &dns_monitor(ns, "ok.test", "A", Some("10.0.0.7")),
    )
    .await;
    assert_eq!(matching.status, CheckStatus::Up, "{:?}", matching.error);

    let wrong = perform_check(
        &check_context(),
        &dns_monitor(ns, "ok.test", "A", Some("10.0.0.8")),
    )
    .await;
    assert_eq!(wrong.status, CheckStatus::Down);
    assert_eq!(
        wrong.error.as_deref(),
        Some("no A answer contains \"10.0.0.8\" (got: 10.0.0.7)")
    );
    assert!(
        wrong.response_time_ms.is_some(),
        "the nameserver answered, so there is a response time"
    );
}

#[tokio::test]
async fn every_record_type_is_matched_on_its_readable_form_case_insensitively() {
    let ns = stub_nameserver().await;
    for (record_type, expected) in [
        ("MX", "10 mail.ok.test"),
        ("TXT", "include:_spf.ok.test"),
        ("CNAME", "EDGE.cdn.test"),
        ("NS", "ns1.ok.test"),
    ] {
        let outcome = perform_check(
            &check_context(),
            &dns_monitor(ns, "ok.test", record_type, Some(expected)),
        )
        .await;
        assert_eq!(
            outcome.status,
            CheckStatus::Up,
            "{record_type}: {:?}",
            outcome.error
        );
    }
}

#[tokio::test]
async fn nxdomain_servfail_and_empty_answers_are_down() {
    let ns = stub_nameserver().await;
    let cases = [
        ("missing.test", "A", "NXDOMAIN: missing.test does not exist"),
        ("broken.test", "A", "DNS server answered SERVFAIL"),
        ("ok.test", "AAAA", "no AAAA records found for ok.test"),
    ];
    for (host, record_type, error) in cases {
        let outcome =
            perform_check(&check_context(), &dns_monitor(ns, host, record_type, None)).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{host} {record_type}");
        assert_eq!(outcome.error.as_deref(), Some(error));
        assert_eq!(outcome.status_code, None, "{host} {record_type}");
        assert!(
            outcome.response_time_ms.is_some(),
            "{host} {record_type}: the nameserver answered"
        );
    }
}

#[tokio::test]
async fn a_nameserver_that_never_answers_times_out() {
    let ns = stub_nameserver().await;
    let monitor = Monitor {
        timeout_seconds: 1,
        ..dns_monitor(ns, "silent.test", "A", None)
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &monitor).await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "took {:?}",
        started.elapsed()
    );
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("timeout after 1s"));
    assert_eq!(outcome.response_time_ms, None, "nothing answered");
}

#[tokio::test]
async fn a_nameserver_that_answers_garbage_is_down_not_a_panic() {
    let ns = stub_nameserver().await;
    let monitor = Monitor {
        timeout_seconds: 1,
        ..dns_monitor(ns, "garbage.test", "A", None)
    };
    let started = Instant::now();
    let outcome = perform_check(&check_context(), &monitor).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(outcome.error.is_some());
    assert_eq!(outcome.response_time_ms, None, "no usable answer");
}

#[tokio::test]
async fn a_name_that_cannot_be_encoded_is_down() {
    let ns = stub_nameserver().await;
    // A label longer than 63 bytes cannot be put in a DNS message.
    let host = format!("{}.test", "a".repeat(64));
    let outcome = perform_check(&check_context(), &dns_monitor(ns, &host, "A", None)).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .starts_with("DNS lookup failed"),
        "{:?}",
        outcome.error
    );
    assert_eq!(outcome.response_time_ms, None);
}

#[tokio::test]
async fn a_dns_monitor_that_cannot_be_checked_is_down_instead_of_panicking() {
    let ns = stub_nameserver().await;
    let cases = [
        (
            unsaved_monitor("dns"),
            "monitor has no host or record type".to_string(),
        ),
        (
            Monitor {
                host: Some("ok.test".to_string()),
                ..unsaved_monitor("dns")
            },
            "monitor has no host or record type".to_string(),
        ),
        (
            Monitor {
                dns_record_type: Some("A".to_string()),
                ..unsaved_monitor("dns")
            },
            "monitor has no host or record type".to_string(),
        ),
        (
            dns_monitor(ns, "ok.test", "SOA", None),
            "unsupported record type \"SOA\"".to_string(),
        ),
        (
            dns_monitor(ns, "ok.test", "ANY", None),
            "unsupported record type \"ANY\"".to_string(),
        ),
        (
            dns_monitor(ns, "ok.test", "a", None),
            "unsupported record type \"a\"".to_string(),
        ),
        (
            Monitor {
                dns_nameserver: Some("not-an-address".to_string()),
                ..dns_monitor(ns, "ok.test", "A", None)
            },
            "invalid nameserver \"not-an-address\"".to_string(),
        ),
    ];
    for (monitor, expected) in cases {
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{expected}");
        assert_eq!(outcome.error.as_deref(), Some(expected.as_str()));
        assert_eq!(outcome.status_code, None, "{expected}");
        assert_eq!(outcome.response_time_ms, None, "{expected}");
    }
}

/// Link-local and metadata addresses, "this network" and the unspecified
/// address are never asked, however they are written: the answer is a refusal
/// at once, not a timeout.
#[tokio::test]
async fn a_nameserver_in_a_refused_range_is_never_asked() {
    for nameserver in [
        "169.254.169.254:53",
        "169.254.169.253:53",
        "169.254.0.1:5353",
        "[fe80::1]:53",
        "[fd00:ec2::254]:53",
        "[::ffff:169.254.169.254]:53",
        "[::ffff:a9fe:a9fe]:53",
        "0.0.0.0:53",
        "0.1.2.3:53",
        "[::]:53",
        "[64:ff9b::a9fe:a9fe]:53",
    ] {
        let addr: SocketAddr = nameserver.parse().unwrap();
        let monitor = Monitor {
            timeout_seconds: 5,
            ..dns_monitor(addr, "ok.test", "A", None)
        };
        let started = Instant::now();
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_refused(&outcome, nameserver);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{nameserver} was not refused at once: {:?}",
            started.elapsed()
        );
    }
}

/// What a nameserver answers is untrusted text: it ends up in check history,
/// incident causes and alert emails, so control characters and direction
/// controls never survive into the error.
#[tokio::test]
async fn answer_text_in_an_error_carries_no_control_or_direction_characters() {
    let ns = stub_nameserver().await;
    let outcome = perform_check(
        &check_context(),
        &dns_monitor(ns, "evil.test", "TXT", Some("nope")),
    )
    .await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let error = outcome.error.unwrap();
    assert_eq!(
        error,
        "no TXT answer contains \"nope\" (got: line1line2 rtl)"
    );
    assert!(!error.contains(char::is_control) && !error.contains('\u{202e}'));
    assert!(error.chars().count() <= MAX_ERROR_CHARS + 1);
}
