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
            "RDAP response has no expiration date",
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
    // Padding past the 1 MiB cap makes the JSON unparseable: down, not a hang or OOM.
    let rdap =
        rdap_answering(ResponseTemplate::new(200).set_body_string("x".repeat(3 * 1024 * 1024)))
            .await;
    let outcome = check_domain(&rdap, 30).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("RDAP response is not valid JSON")
    );
}
