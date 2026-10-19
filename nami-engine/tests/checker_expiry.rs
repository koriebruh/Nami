// `ssl_expiry` (local TLS servers with certificates from a test CA) and
// `domain_expiry` (wiremock standing in for rdap.org) checks.
mod common;

use chrono::{DateTime, Utc};
use common::{check_context, tls, unsaved_monitor};
use nami_engine::{
    models::monitor::Monitor,
    uptime::{
        checker::{CheckContext, CheckOutcome, CheckStatus, perform_check},
        tls::client_config,
    },
};
use serde_json::json;
use time::{Duration, OffsetDateTime};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{headers, method, path},
};

/// The date the error message shows for an instant `days` (+1 h) from now.
fn date_in(days: i64) -> String {
    (Utc::now() + chrono::Duration::days(days) + chrono::Duration::hours(1))
        .format("%Y-%m-%d")
        .to_string()
}

/// A TLS server whose `localhost` certificate (from `ca`) expires `days`
/// days and one hour from now. Returns its port.
async fn tls_server(ca: &tls::TestCa, days: i64) -> u16 {
    let not_after = OffsetDateTime::now_utc() + Duration::days(days) + Duration::hours(1);
    tls::serve_https_ok(ca.leaf(not_after)).await
}

async fn check_ssl(ca: &tls::TestCa, host: &str, port: u16, threshold: i32) -> CheckOutcome {
    let ctx = CheckContext {
        tls: client_config(ca.roots()),
        ..check_context()
    };
    let monitor = Monitor {
        host: Some(host.to_string()),
        port: Some(i32::from(port)),
        expiry_threshold_days: Some(threshold),
        ..unsaved_monitor("ssl_expiry")
    };
    perform_check(&ctx, &monitor).await
}

#[tokio::test]
async fn a_certificate_far_from_expiry_is_up() {
    let ca = tls::TestCa::new();
    let port = tls_server(&ca, 60).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert!(outcome.response_time_ms.is_some());
}

#[tokio::test]
async fn a_certificate_inside_the_threshold_is_down_and_says_how_long_is_left() {
    let ca = tls::TestCa::new();
    let port = tls_server(&ca, 5).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error,
        Some(format!("certificate expires in 5 days ({})", date_in(5)))
    );
}

#[tokio::test]
async fn an_expired_certificate_is_down_and_says_since_when() {
    let ca = tls::TestCa::new();
    let not_after = OffsetDateTime::now_utc() - Duration::days(3) - Duration::hours(1);
    let port = tls::serve_https_ok(ca.leaf(not_after)).await;
    let expired_on =
        (Utc::now() - chrono::Duration::days(3) - chrono::Duration::hours(1)).format("%Y-%m-%d");
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error,
        Some(format!("certificate expired 3 days ago ({expired_on})"))
    );
}

#[tokio::test]
async fn untrusted_and_mismatched_certificates_are_down() {
    let ca = tls::TestCa::new();
    let untrusted = tls::serve_https_ok(tls::self_signed()).await;
    let outcome = check_ssl(&ca, "localhost", untrusted, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("certificate is not trusted (unknown issuer)")
    );

    // A valid certificate for `localhost`, reached as 127.0.0.1.
    let port = tls_server(&ca, 60).await;
    let outcome = check_ssl(&ca, "127.0.0.1", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert!(
        outcome
            .error
            .as_deref()
            .unwrap()
            .contains("not valid for name"),
        "{:?}",
        outcome.error
    );
}

#[tokio::test]
async fn nothing_listening_is_down() {
    let ca = tls::TestCa::new();
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let outcome = check_ssl(&ca, "localhost", closed, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some("connection refused"));
}

fn rdap_body(expires: Option<DateTime<Utc>>) -> serde_json::Value {
    let mut events =
        vec![json!({ "eventAction": "registration", "eventDate": "1995-08-14T04:00:00Z" })];
    if let Some(expires) = expires {
        events.push(json!({ "eventAction": "expiration", "eventDate": expires.to_rfc3339() }));
    }
    json!({ "objectClassName": "domain", "ldhName": "EXAMPLE.COM", "events": events })
}

async fn check_domain(rdap: &MockServer, threshold: i32) -> CheckOutcome {
    let ctx = CheckContext {
        rdap_base_url: rdap.uri(),
        ..check_context()
    };
    let monitor = Monitor {
        host: Some("example.com".to_string()),
        expiry_threshold_days: Some(threshold),
        ..unsaved_monitor("domain_expiry")
    };
    perform_check(&ctx, &monitor).await
}

async fn rdap_answering(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/domain/example.com"))
        // wiremock splits header values on commas.
        .and(headers(
            "accept",
            vec!["application/rdap+json", "application/json"],
        ))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn a_domain_far_from_expiry_is_up() {
    let expires = Utc::now() + chrono::Duration::days(400);
    let rdap =
        rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(expires)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn a_domain_inside_the_threshold_or_expired_is_down() {
    let soon = Utc::now() + chrono::Duration::days(10) + chrono::Duration::hours(1);
    let rdap =
        rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(soon)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error,
        Some(format!("domain expires in 10 days ({})", date_in(10)))
    );

    let gone = Utc::now() - chrono::Duration::days(2) - chrono::Duration::hours(1);
    let rdap =
        rdap_answering(ResponseTemplate::new(200).set_body_json(rdap_body(Some(gone)))).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error,
        Some(format!(
            "domain expired 2 days ago ({})",
            gone.format("%Y-%m-%d")
        ))
    );
}

#[tokio::test]
async fn the_rdap_redirect_to_the_registry_is_followed() {
    let rdap = MockServer::start().await;
    Mock::given(path("/domain/example.com"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!("{}/registry/domain/example.com", rdap.uri()),
        ))
        .mount(&rdap)
        .await;
    let expires = Utc::now() + chrono::Duration::days(400);
    Mock::given(path("/registry/domain/example.com"))
        .respond_with(ResponseTemplate::new(200).set_body_json(rdap_body(Some(expires))))
        .mount(&rdap)
        .await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

#[tokio::test]
async fn rdap_failures_and_answers_without_an_expiration_are_down() {
    let cases = [
        (
            ResponseTemplate::new(200).set_body_json(rdap_body(None)),
            "RDAP response has no expiration event",
        ),
        (ResponseTemplate::new(404), "RDAP lookup failed: HTTP 404"),
        (
            ResponseTemplate::new(200).set_body_string("<html>"),
            "RDAP response is not valid JSON",
        ),
    ];
    for (template, error) in cases {
        let rdap = rdap_answering(template).await;
        let outcome = check_domain(&rdap, 30).await;
        assert_eq!(outcome.status, CheckStatus::Down);
        assert_eq!(outcome.error.as_deref(), Some(error));
    }
}

#[tokio::test]
async fn ssl_checks_refuse_forbidden_addresses_without_connecting() {
    let ca = tls::TestCa::new();
    for host in ["169.254.169.254", "fd00:ec2::254", "::ffff:169.254.169.254"] {
        let outcome = check_ssl(&ca, host, 443, 14).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{host}");
        assert_eq!(
            outcome.error.as_deref(),
            Some("destination not allowed"),
            "{host}"
        );
    }
}

#[tokio::test]
async fn rdap_redirects_to_forbidden_addresses_are_refused() {
    let rdap = MockServer::start().await;
    Mock::given(path("/domain/example.com"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", "http://169.254.169.254/latest/meta-data/"),
        )
        .mount(&rdap)
        .await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("RDAP lookup failed: destination not allowed")
    );
}

#[tokio::test]
async fn an_rdap_base_url_that_is_unusable_or_forbidden_is_down() {
    for (base, error) in [
        ("http://169.254.169.254", "destination not allowed"),
        (
            "ftp://rdap.example",
            "RDAP_BASE_URL is not a valid http(s) URL",
        ),
        ("not a url", "RDAP_BASE_URL is not a valid http(s) URL"),
    ] {
        let ctx = CheckContext {
            rdap_base_url: base.to_string(),
            ..check_context()
        };
        let monitor = Monitor {
            host: Some("example.com".to_string()),
            expiry_threshold_days: Some(30),
            ..unsaved_monitor("domain_expiry")
        };
        let outcome = perform_check(&ctx, &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{base}");
        assert_eq!(outcome.error.as_deref(), Some(error), "{base}");
    }
}

#[tokio::test]
async fn a_huge_rdap_answer_is_read_only_up_to_the_cap() {
    // Valid JSON whose far-future expiration event lies past the 1 MiB cap:
    // were the body read whole it would parse and the domain would be up.
    let expires = Utc::now() + chrono::Duration::days(400);
    let body = format!(
        r#"{{"pad":"{}","events":[{{"eventAction":"expiration","eventDate":"{}"}}]}}"#,
        "x".repeat(3 * 1024 * 1024),
        expires.to_rfc3339()
    );
    assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_string(body)).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("RDAP response is not valid JSON")
    );
}

#[tokio::test]
async fn a_certificate_that_is_not_valid_yet_says_since_when() {
    let ca = tls::TestCa::new();
    let not_before = OffsetDateTime::now_utc() + Duration::days(2) + Duration::hours(1);
    let cert = ca.leaf_between(not_before, not_before + Duration::days(90));
    let port = tls::serve_https_ok(cert).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error,
        Some(format!("certificate not valid until {}", date_in(2)))
    );
}

#[tokio::test]
async fn rdap_dates_without_a_time_and_events_without_fields_are_understood() {
    let soon = (Utc::now() + chrono::Duration::days(10))
        .format("%Y-%m-%d")
        .to_string();
    let body = json!({ "events": [
        { "eventDate": "2000-01-01" },
        { "eventAction": "expiration" },
        { "eventAction": "expiration", "eventDate": "not a date" },
        { "eventAction": "expiration", "eventDate": soon },
    ]});
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_json(body)).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome
            .error
            .as_deref()
            .map(|e| e.starts_with("domain expires in ")),
        Some(true),
        "{:?}",
        outcome.error
    );

    let body = json!({ "events": [{ "eventAction": "expiration", "eventDate": "someday" }] });
    let rdap = rdap_answering(ResponseTemplate::new(200).set_body_json(body)).await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("RDAP expiration date is not valid")
    );
}

/// The boundary, end to end: a certificate with a little over N days left is
/// up, one with a little under is down.
#[tokio::test]
async fn the_threshold_boundary_holds_end_to_end() {
    let ca = tls::TestCa::new();
    let just_over = OffsetDateTime::now_utc() + Duration::days(14) + Duration::minutes(5);
    let port = tls::serve_https_ok(ca.leaf(just_over)).await;
    assert_eq!(
        check_ssl(&ca, "localhost", port, 14).await.status,
        CheckStatus::Up
    );
    let just_under = OffsetDateTime::now_utc() + Duration::days(14) - Duration::minutes(5);
    let port = tls::serve_https_ok(ca.leaf(just_under)).await;
    let outcome = check_ssl(&ca, "localhost", port, 14).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let date =
        (Utc::now() + chrono::Duration::days(14) - chrono::Duration::minutes(5)).format("%Y-%m-%d");
    assert_eq!(
        outcome.error,
        Some(format!("certificate expires in 13 days ({date})"))
    );
}

/// One config serves every check of a pass: it must never resume a session,
/// which would skip verifying the certificate the server presents now.
#[tokio::test]
async fn ssl_checks_never_resume_a_tls_session() {
    use std::sync::{Arc, atomic::Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn handshake(config: &Arc<rustls::ClientConfig>, port: u16) {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let name = rustls::pki_types::ServerName::try_from("localhost").unwrap();
        let mut stream = tokio_rustls::TlsConnector::from(config.clone())
            .connect(name, tcp)
            .await
            .unwrap();
        // Reading is what hands the session ticket to the client.
        stream.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response).await;
    }

    let ca = tls::TestCa::new();
    let handshakes = Arc::new(tls::Handshakes::default());
    let leaf = ca.leaf(OffsetDateTime::now_utc() + Duration::days(60));
    let port = tls::serve_https_counting(leaf, Arc::clone(&handshakes)).await;

    // The control: a default rustls client config does resume.
    let default = Arc::new(
        rustls::ClientConfig::builder_with_provider(tls::provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(ca.roots())
            .with_no_client_auth(),
    );
    handshake(&default, port).await;
    handshake(&default, port).await;
    assert_eq!(handshakes.resumed.load(Ordering::SeqCst), 1, "control");
    handshakes.full.store(0, Ordering::SeqCst);
    handshakes.resumed.store(0, Ordering::SeqCst);

    // The checker's own config, shared, never does.
    let shared = client_config(ca.roots());
    handshake(&shared, port).await;
    handshake(&shared, port).await;
    handshake(&shared, port).await;
    assert_eq!(handshakes.full.load(Ordering::SeqCst), 3);
    assert_eq!(handshakes.resumed.load(Ordering::SeqCst), 0);

    // And the probe, sharing one context, shakes hands in full every time.
    let ctx = CheckContext {
        tls: shared,
        ..check_context()
    };
    let monitor = Monitor {
        host: Some("localhost".to_string()),
        port: Some(i32::from(port)),
        expiry_threshold_days: Some(14),
        ..unsaved_monitor("ssl_expiry")
    };
    handshakes.full.store(0, Ordering::SeqCst);
    for _ in 0..2 {
        assert_eq!(perform_check(&ctx, &monitor).await.status, CheckStatus::Up);
    }
    // The probe hangs up as soon as it has the certificate: let the server
    // finish counting.
    for _ in 0..100 {
        if handshakes.full.load(Ordering::SeqCst) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(handshakes.full.load(Ordering::SeqCst), 2);
    assert_eq!(handshakes.resumed.load(Ordering::SeqCst), 0);
}
