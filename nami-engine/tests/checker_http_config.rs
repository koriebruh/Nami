// HTTP request configuration in the checker. The worker tests at the end
// require `docker compose up -d postgres mailpit`.
mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::http::StatusCode;
use common::{
    add_member, call, check_context, seed_http_monitor, seed_project, seed_user, settled_mail,
    test_cipher, tls, unique_email, unsaved_monitor, user_with_role, with_basic_auth, with_headers,
};
use nami_engine::{
    core::crypto::{Cipher, SecretString},
    core::state::AppState,
    models::{
        monitor::Monitor,
        monitor_request::{RequestHeader, seal_headers, seal_password},
    },
    uptime::{
        checker::{CONFIG_DECRYPT_ERROR, CheckContext, CheckStatus, MAX_REDIRECTS, perform_check},
        worker::run_due_checks,
    },
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string, header, method, path},
};

const API_KEY: &str = "key-1f2e3d";
const PASSWORD: &str = "pass-9a8b7c";
/// base64("monitor:pass-9a8b7c")
const BASIC: &str = "Basic bW9uaXRvcjpwYXNzLTlhOGI3Yw==";

fn http_monitor(url: String) -> Monitor {
    Monitor {
        url: Some(url),
        ..unsaved_monitor("http")
    }
}

fn keyword_monitor(url: String, keyword: &str) -> Monitor {
    Monitor {
        url: Some(url),
        keyword: Some(keyword.to_string()),
        ..unsaved_monitor("keyword")
    }
}

/// A monitor carrying an API-key header, basic auth and a JSON body.
fn configured_monitor(url: String) -> Monitor {
    let monitor = with_basic_auth(
        with_headers(
            http_monitor(url),
            &[("X-Api-Key", API_KEY), ("Content-Type", "application/json")],
        ),
        "monitor",
        PASSWORD,
    );
    Monitor {
        http_method: "POST".to_string(),
        request_body: Some("{\"probe\":true}".to_string()),
        ..monitor
    }
}

/// The raw bytes of one header of a request the mock server received.
fn received_header(request: &wiremock::Request, name: &str) -> Option<Vec<u8>> {
    request
        .headers
        .get(name)
        .map(|value| value.as_bytes().to_vec())
}

#[tokio::test]
async fn method_body_custom_headers_and_basic_auth_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("x-api-key", API_KEY))
        .and(header("content-type", "application/json"))
        .and(header("authorization", BASIC))
        .and(body_string("{\"probe\":true}"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let outcome = perform_check(
        &check_context(),
        &configured_monitor(format!("{}/orders", server.uri())),
    )
    .await;

    // Anything that did not match every matcher gets wiremock's 404.
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(201));
}

#[tokio::test]
async fn each_configured_method_is_used() {
    for verb in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS", "HEAD"] {
        let server = MockServer::start().await;
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let monitor = Monitor {
            http_method: verb.to_string(),
            ..http_monitor(format!("{}/", server.uri()))
        };
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Up,
            "{verb}: {:?}",
            outcome.error
        );
    }
}

/// The body is the monitor's own for POST, PUT and PATCH and never goes out
/// with another method, whatever is stored.
#[tokio::test]
async fn a_body_goes_out_with_post_put_and_patch_only() {
    for (verb, sent) in [
        ("POST", true),
        ("PUT", true),
        ("PATCH", true),
        ("GET", false),
        ("DELETE", false),
        ("OPTIONS", false),
    ] {
        let server = MockServer::start().await;
        Mock::given(method(verb))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;
        let monitor = Monitor {
            http_method: verb.to_string(),
            request_body: Some("probe-body".to_string()),
            ..http_monitor(format!("{}/", server.uri()))
        };
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(
            outcome.status,
            CheckStatus::Up,
            "{verb}: {:?}",
            outcome.error
        );
        let requests = server.received_requests().await.unwrap();
        let expected: &[u8] = if sent { b"probe-body" } else { b"" };
        assert_eq!(requests[0].body, expected, "{verb}");
    }
}

#[tokio::test]
async fn a_stored_method_the_checker_does_not_know_is_down_without_a_request() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    for stored in ["BREW", "get", "CONNECT", "TRACE", ""] {
        let monitor = Monitor {
            http_method: stored.to_string(),
            ..http_monitor(format!("{}/", server.uri()))
        };
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{stored:?}");
        assert_eq!(
            outcome.error,
            Some(format!("unsupported http method {stored:?}"))
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// A keyword check reads the body, so a stored HEAD is sent as GET; every
/// other stored method, with its body and headers, goes out as it is.
#[tokio::test]
async fn keyword_monitors_send_the_stored_method_body_and_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/status"))
        .and(header("x-api-key", API_KEY))
        .and(body_string("what is the status?"))
        .respond_with(ResponseTemplate::new(200).set_body_string("status: all systems go"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .and(header("x-api-key", API_KEY))
        .respond_with(ResponseTemplate::new(200).set_body_string("status: only on GET"))
        .mount(&server)
        .await;
    let url = format!("{}/status", server.uri());

    let post = Monitor {
        http_method: "POST".to_string(),
        request_body: Some("what is the status?".to_string()),
        ..with_headers(
            keyword_monitor(url.clone(), "systems go"),
            &[("X-Api-Key", API_KEY)],
        )
    };
    let outcome = perform_check(&check_context(), &post).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);

    // Stored as HEAD (the body is needed): sent as GET, headers included.
    let head = Monitor {
        http_method: "HEAD".to_string(),
        ..with_headers(
            keyword_monitor(url, "only on GET"),
            &[("X-Api-Key", API_KEY)],
        )
    };
    let outcome = perform_check(&check_context(), &head).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

/// Validation accepts any text without control characters as a header value,
/// so a value with accents or emoji has to reach the target as the UTF-8 it is
/// rather than failing every check.
#[tokio::test]
async fn header_values_with_non_ascii_text_are_sent_as_utf_8() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let monitor = with_headers(
        http_monitor(format!("{}/", server.uri())),
        &[("X-Note", "caf\u{e9} \u{2615}\ttab")],
    );

    let outcome = perform_check(&check_context(), &monitor).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(
        received_header(&requests[0], "x-note"),
        Some("caf\u{e9} \u{2615}\ttab".as_bytes().to_vec())
    );
}

#[tokio::test]
async fn a_custom_user_agent_replaces_the_default_one() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let url = format!("{}/", server.uri());

    perform_check(&check_context(), &http_monitor(url.clone())).await;
    let custom = with_headers(http_monitor(url), &[("User-Agent", "my-probe/7")]);
    perform_check(&check_context(), &custom).await;

    let requests = server.received_requests().await.unwrap();
    let agent =
        |index: usize| String::from_utf8(received_header(&requests[index], "user-agent").unwrap());
    assert!(agent(0).unwrap().starts_with("Nami-Uptime/"));
    assert_eq!(agent(1).unwrap(), "my-probe/7");
}

async fn redirect(server: &MockServer, from: &str, to: String) {
    Mock::given(path(from))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", to))
        .mount(server)
        .await;
}

#[tokio::test]
async fn with_follow_redirects_off_the_redirect_itself_is_the_result() {
    let server = MockServer::start().await;
    redirect(&server, "/old", format!("{}/new", server.uri())).await;
    let monitor = Monitor {
        follow_redirects: false,
        ..http_monitor(format!("{}/old", server.uri()))
    };

    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(
        outcome.status,
        CheckStatus::Up,
        "302 is inside the default 200-399 range"
    );
    assert_eq!(outcome.status_code, Some(302));

    let strict = Monitor {
        expected_status_max: 299,
        ..monitor
    };
    let outcome = perform_check(&check_context(), &strict).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(
        outcome.error.as_deref(),
        Some("expected status 200-299, got 302")
    );
}

/// Not following is not the same as failing: a monitor with secrets and
/// `follow_redirects` off reports the redirect like any other monitor.
#[tokio::test]
async fn a_monitor_with_secrets_that_does_not_follow_redirects_reports_the_redirect() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    redirect(
        &target,
        "/start",
        format!("http://localhost:{}/landing", elsewhere.address().port()),
    )
    .await;
    let monitor = Monitor {
        follow_redirects: false,
        ..with_headers(
            http_monitor(format!("{}/start", target.uri())),
            &[("X-Api-Key", API_KEY)],
        )
    };

    let outcome = perform_check(&check_context(), &monitor).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(302));
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
}

/// A chain /{prefix}/0 -> /{prefix}/1 -> ... -> /{prefix}/{hops} answering 200.
async fn redirect_chain(server: &MockServer, prefix: &str, hops: usize) {
    for i in 0..hops {
        redirect(
            server,
            &format!("/{prefix}/{i}"),
            format!("{}/{prefix}/{}", server.uri(), i + 1),
        )
        .await;
    }
    Mock::given(path(format!("/{prefix}/{hops}")))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// The limit holds for monitors with secrets too: they follow redirects by a
/// policy of their own.
#[tokio::test]
async fn at_most_ten_redirects_are_followed() {
    assert_eq!(MAX_REDIRECTS, 10);
    let server = MockServer::start().await;
    redirect_chain(&server, "ten", 10).await;
    redirect_chain(&server, "eleven", 11).await;

    for secrets in [false, true] {
        let monitor = |url: String| {
            let monitor = http_monitor(url);
            if secrets {
                with_headers(monitor, &[("X-Api-Key", API_KEY)])
            } else {
                monitor
            }
        };

        let ten = perform_check(
            &check_context(),
            &monitor(format!("{}/ten/0", server.uri())),
        )
        .await;
        assert_eq!(
            ten.status,
            CheckStatus::Up,
            "secrets={secrets}: {:?}",
            ten.error
        );

        let eleven = perform_check(
            &check_context(),
            &monitor(format!("{}/eleven/0", server.uri())),
        )
        .await;
        assert_eq!(eleven.status, CheckStatus::Down, "secrets={secrets}");
        assert_eq!(
            eleven.error.as_deref(),
            Some("too many redirects"),
            "secrets={secrets}"
        );
    }
}

/// Security: reqwest would forward a custom header such as an API key to
/// wherever a redirect points, and drops `Authorization` and `Cookie` only on
/// the hop that crosses to another host (see
/// `credentials_in_the_url_never_reach_another_origin`); monitors with custom
/// headers or basic auth only follow redirects that stay on the monitored
/// origin (scheme, host and port).
#[tokio::test]
async fn secrets_never_follow_a_redirect_to_another_origin() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    let port = elsewhere.address().port();
    let start = format!("{}/start", target.uri());

    // (where the redirect points, what the error says it is)
    let destinations = [
        // Same machine, different host name: 127.0.0.1 -> localhost.
        (
            format!("http://localhost:{port}/landing"),
            "so the redirect to another host (localhost) was not followed".to_string(),
        ),
        // Same host name, different port.
        (
            format!("http://127.0.0.1:{port}/landing"),
            format!("so the redirect to another origin (http://127.0.0.1:{port}) was not followed"),
        ),
    ];
    for (location, expected) in destinations {
        redirect(&target, "/start", location.clone()).await;
        let monitors = [
            (
                "custom header",
                with_headers(http_monitor(start.clone()), &[("X-Api-Key", API_KEY)]),
            ),
            (
                "basic auth",
                with_basic_auth(http_monitor(start.clone()), "monitor", PASSWORD),
            ),
            (
                "authorization header",
                with_headers(
                    http_monitor(start.clone()),
                    &[("Authorization", "Bearer xyz")],
                ),
            ),
            ("both", configured_monitor(start.clone())),
        ];
        for (label, monitor) in monitors {
            let outcome = perform_check(&check_context(), &monitor).await;

            assert_eq!(outcome.status, CheckStatus::Down, "{label}: {location}");
            let error = outcome.error.unwrap();
            assert!(error.contains(&expected), "{label}: {error}");
            assert!(
                error.starts_with(
                    "headers, credentials and request bodies are only sent to the monitored origin"
                ),
                "the reason comes first: {error}"
            );
            assert!(
                !error.contains(API_KEY) && !error.contains(PASSWORD),
                "{error}"
            );
            assert!(
                !error.contains("landing"),
                "the target's path is not echoed: {error}"
            );
        }
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "no request, so no secret, ever reached {location}"
        );
        target.reset().await;
    }
}

/// reqwest drops `Authorization`, `Cookie` and the like only on the hop that
/// crosses to another host, from that hop's own copy of the request headers;
/// the next hop starts again from the original ones. Credentials in the
/// monitored URL (`http://user:pass@host/`, which reqwest turns into an
/// `Authorization` header) therefore came back one hop later: host A to host
/// B/1 to host B/2 sent them to B. A monitor with credentials in its URL never
/// leaves its origin either.
#[tokio::test]
async fn credentials_in_the_url_never_reach_another_origin() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    let port = elsewhere.address().port();
    // Two hops on the other host: the second one is where the header came back.
    redirect(&elsewhere, "/1", format!("http://localhost:{port}/2")).await;
    Mock::given(path("/2"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    redirect(&target, "/start", format!("http://localhost:{port}/1")).await;

    for userinfo in ["user:pass-9a8b7c", "user", ":pass-9a8b7c"] {
        let url = format!(
            "{}/start",
            target
                .uri()
                .replacen("http://", &format!("http://{userinfo}@"), 1)
        );

        let outcome = perform_check(&check_context(), &http_monitor(url)).await;

        for request in elsewhere.received_requests().await.unwrap() {
            assert_eq!(
                received_header(&request, "authorization"),
                None,
                "{userinfo}: {} got the credentials",
                request.url
            );
        }
        assert_eq!(
            outcome.status,
            CheckStatus::Down,
            "{userinfo}: {:?}",
            outcome.error
        );
        let error = outcome.error.unwrap();
        assert!(
            error.contains("another host (localhost)"),
            "{userinfo}: {error}"
        );
        assert!(!error.contains("pass-9a8b7c"), "{error}");
    }
    assert!(
        elsewhere.received_requests().await.unwrap().is_empty(),
        "no request reached the other host"
    );
}

/// Credentials in the URL still work where they belong: every hop that stays
/// on the monitored origin carries them.
#[tokio::test]
async fn credentials_in_the_url_ride_along_on_redirects_that_stay_on_the_origin() {
    let server = MockServer::start().await;
    redirect(&server, "/start", format!("{}/landing", server.uri())).await;
    Mock::given(method("GET"))
        .and(path("/landing"))
        .and(header("authorization", "Basic dXNlcjpwYXNzLTlhOGI3Yw=="))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let url = format!(
        "{}/start",
        server
            .uri()
            .replacen("http://", "http://user:pass-9a8b7c@", 1)
    );

    let outcome = perform_check(&check_context(), &http_monitor(url)).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(200));
}

/// The same redirects are followed by every monitor without secrets, and
/// nothing about the monitored URL travels along in a Referer header.
#[tokio::test]
async fn plain_monitors_still_follow_redirects_to_another_origin_without_a_referer() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    redirect(
        &target,
        "/start",
        format!("http://localhost:{}/landing", elsewhere.address().port()),
    )
    .await;

    let outcome = perform_check(
        &check_context(),
        &http_monitor(format!("{}/start?token=url-secret", target.uri())),
    )
    .await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let landed = elsewhere.received_requests().await.unwrap();
    assert_eq!(landed.len(), 1);
    assert_eq!(received_header(&landed[0], "referer"), None);
}

/// Staying on the monitored origin is fine: every hop carries the headers.
#[tokio::test]
async fn secrets_ride_along_on_redirects_that_stay_on_the_origin() {
    let server = MockServer::start().await;
    redirect(&server, "/start", format!("{}/landing", server.uri())).await;
    Mock::given(method("GET"))
        .and(path("/landing"))
        .and(header("x-api-key", API_KEY))
        .and(header("authorization", BASIC))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let monitor = with_basic_auth(
        with_headers(
            http_monitor(format!("{}/start", server.uri())),
            &[("X-Api-Key", API_KEY)],
        ),
        "monitor",
        PASSWORD,
    );

    let outcome = perform_check(&check_context(), &monitor).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    assert_eq!(outcome.status_code, Some(200));
}

/// A redirect to an address no check may contact is refused with the generic
/// message by every redirecting client, the one for monitors with secrets
/// included, even though it also leaves the origin.
#[tokio::test]
async fn a_redirect_to_a_refused_address_is_refused_for_every_monitor() {
    for location in [
        "http://169.254.169.254/latest/meta-data/",
        "http://[fd00:ec2::254]/",
        "http://0.0.0.0:9/",
    ] {
        let server = MockServer::start().await;
        redirect(&server, "/start", location.to_string()).await;
        let start = format!("{}/start", server.uri());
        let monitors = [
            ("plain", http_monitor(start.clone())),
            (
                "with secrets",
                with_headers(http_monitor(start.clone()), &[("X-Api-Key", API_KEY)]),
            ),
            (
                "with basic auth",
                with_basic_auth(http_monitor(start), "monitor", PASSWORD),
            ),
        ];
        for (label, monitor) in monitors {
            let outcome = perform_check(&check_context(), &monitor).await;
            assert_eq!(outcome.status, CheckStatus::Down, "{label}: {location}");
            assert_eq!(
                outcome.error.as_deref(),
                Some("destination not allowed"),
                "{label}: {location}"
            );
        }
    }
}

/// `verify_ssl` decides whether a certificate no root store trusts is
/// accepted, for every client the checker keeps (redirects off, on, on with
/// secrets).
#[tokio::test]
async fn verify_ssl_decides_whether_a_self_signed_certificate_is_accepted() {
    let port = tls::serve_https_ok(tls::self_signed()).await;
    let url = format!("https://localhost:{port}/");

    for (label, follow_redirects, secrets) in [
        ("redirects off", false, false),
        ("redirects on", true, false),
        ("redirects on, with secrets", true, true),
    ] {
        let monitor = |verify_ssl: bool| {
            let monitor = Monitor {
                follow_redirects,
                verify_ssl,
                ..http_monitor(url.clone())
            };
            if secrets {
                with_headers(monitor, &[("X-Api-Key", API_KEY)])
            } else {
                monitor
            }
        };

        let verified = perform_check(&check_context(), &monitor(true)).await;
        assert_eq!(verified.status, CheckStatus::Down, "{label}");
        let error = verified.error.as_deref().unwrap();
        assert!(error.contains("certificate"), "{label}: {error}");

        let unverified = perform_check(&check_context(), &monitor(false)).await;
        assert_eq!(
            unverified.status,
            CheckStatus::Up,
            "{label}: {:?}",
            unverified.error
        );
        assert_eq!(unverified.status_code, Some(200), "{label}");
    }
}

/// A failing request says why, and never repeats a secret.
#[tokio::test]
async fn errors_never_contain_the_secrets() {
    let refused = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let unauthorized = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&unauthorized)
        .await;
    let looping = MockServer::start().await;
    redirect(&looping, "/", format!("{}/", looping.uri())).await;
    let tls_port = tls::serve_https_ok(tls::self_signed()).await;

    for (label, url) in [
        ("connection refused", format!("http://127.0.0.1:{refused}/")),
        ("a 401", format!("{}/", unauthorized.uri())),
        ("a redirect loop", format!("{}/", looping.uri())),
        (
            "an untrusted certificate",
            format!("https://localhost:{tls_port}/"),
        ),
        (
            "a TLS handshake against a plain port",
            format!("https://127.0.0.1:{}/", unauthorized.address().port()),
        ),
    ] {
        let outcome = perform_check(&check_context(), &configured_monitor(url)).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{label}");
        let error = outcome.error.expect("a failed check carries an error");
        for secret in [API_KEY, PASSWORD, "bW9uaXRvcjpwYXNz", "probe"] {
            assert!(!error.contains(secret), "{label}: {error}");
        }
    }
}

fn sealed_headers_for(
    cipher: &Cipher,
    monitor_id: Uuid,
    headers: &[(&str, &str)],
) -> (Option<Vec<u8>>, Vec<String>) {
    let headers: Vec<RequestHeader> = headers
        .iter()
        .map(|(name, value)| RequestHeader {
            name: name.to_string(),
            value: SecretString::new(*value),
        })
        .collect();
    (
        Some(seal_headers(cipher, monitor_id, &headers)),
        headers.into_iter().map(|h| h.name).collect(),
    )
}

/// Security: tampered, wrongly keyed or transplanted secrets fail the check
/// with a generic error, and no request is sent.
#[tokio::test]
async fn undecryptable_secrets_fail_the_check_with_a_generic_error() {
    assert_eq!(
        CONFIG_DECRYPT_ERROR,
        "request configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)"
    );
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let url = format!("{}/orders", server.uri());
    let other_key = Cipher::new(&[3u8; 32]);
    let someone_else = Uuid::new_v4();

    let mut tampered_headers = configured_monitor(url.clone());
    tampered_headers.request_headers_enc.as_mut().unwrap()[20] ^= 0x01;
    let mut tampered_password = configured_monitor(url.clone());
    tampered_password.auth_password_enc.as_mut().unwrap()[20] ^= 0x01;
    let wrong_key_headers = {
        let monitor = configured_monitor(url.clone());
        let (enc, _) = sealed_headers_for(&other_key, monitor.id, &[("X-Api-Key", API_KEY)]);
        Monitor {
            request_headers_enc: enc,
            ..monitor
        }
    };
    let wrong_key_password = {
        let monitor = configured_monitor(url.clone());
        let sealed = seal_password(&other_key, monitor.id, &SecretString::new(PASSWORD));
        Monitor {
            auth_password_enc: Some(sealed),
            ..monitor
        }
    };
    // Blobs that are fine, but sealed for another monitor's row.
    let transplanted_headers = {
        let monitor = configured_monitor(url.clone());
        let (enc, _) = sealed_headers_for(&test_cipher(), someone_else, &[("X-Api-Key", API_KEY)]);
        Monitor {
            request_headers_enc: enc,
            ..monitor
        }
    };
    let transplanted_password = {
        let monitor = configured_monitor(url.clone());
        let sealed = seal_password(&test_cipher(), someone_else, &SecretString::new(PASSWORD));
        Monitor {
            auth_password_enc: Some(sealed),
            ..monitor
        }
    };
    // Not a secret of the right kind at all.
    let garbage = Monitor {
        request_headers_enc: Some(b"not a sealed value".to_vec()),
        auth_password_enc: Some(Vec::new()),
        ..configured_monitor(url)
    };

    for (label, monitor) in [
        ("tampered headers", tampered_headers),
        ("tampered password", tampered_password),
        ("headers under another key", wrong_key_headers),
        ("password under another key", wrong_key_password),
        ("headers of another monitor", transplanted_headers),
        ("password of another monitor", transplanted_password),
        ("garbage", garbage),
    ] {
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{label}");
        assert_eq!(
            outcome.error.as_deref(),
            Some(CONFIG_DECRYPT_ERROR),
            "{label}"
        );
        assert_eq!(outcome.status_code, None, "{label}");
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Sealed headers that are not valid HTTP (validation never lets them be
/// stored) fail the check without a request, and without repeating the value.
#[tokio::test]
async fn sealed_headers_that_are_not_valid_http_fail_the_check_without_echoing_them() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    for (name, value) in [
        ("bad name", API_KEY),
        ("X-Api-Key", "line\nbreak-secret"),
        ("X-Api-Key", "\u{7f}del-secret"),
        // The client owns these (validation refuses them): a sealed blob that
        // has them anyway is not sent.
        ("Host", "evil.test"),
        ("host", "evil.test"),
        ("Content-Length", "0"),
        ("CONTENT-LENGTH", "0"),
        ("Transfer-Encoding", "chunked"),
        ("Connection", "upgrade"),
        ("cOnNeCtIoN", "close"),
    ] {
        let base = http_monitor(format!("{}/", server.uri()));
        let (enc, names) = sealed_headers_for(&test_cipher(), base.id, &[(name, value)]);
        let monitor = Monitor {
            request_headers_enc: enc,
            request_header_names: names,
            ..base
        };

        let outcome = perform_check(&check_context(), &monitor).await;

        assert_eq!(outcome.status, CheckStatus::Down, "{name:?} {value:?}");
        assert_eq!(
            outcome.error.as_deref(),
            Some("invalid request header configuration")
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Names that only look like the reserved ones are ordinary headers.
#[tokio::test]
async fn headers_that_merely_resemble_the_reserved_ones_are_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let names = [
        "Hosts",
        "X-Host",
        "Content-Type",
        "Content-Lengths",
        "Connection-Id",
        "Keep-Alive-Timeout",
        "X-Upgrade",
    ];
    let headers: Vec<(&str, &str)> = names.iter().map(|name| (*name, "v")).collect();
    let monitor = with_headers(http_monitor(format!("{}/", server.uri())), &headers);

    let outcome = perform_check(&check_context(), &monitor).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let requests = server.received_requests().await.unwrap();
    for name in names {
        assert_eq!(
            received_header(&requests[0], name),
            Some(b"v".to_vec()),
            "{name}"
        );
    }
}

#[tokio::test]
async fn basic_auth_without_a_password_sends_an_empty_one() {
    let server = MockServer::start().await;
    // base64("monitor:")
    Mock::given(method("GET"))
        .and(header("authorization", "Basic bW9uaXRvcjo="))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let monitor = Monitor {
        auth_username: Some("monitor".to_string()),
        ..http_monitor(format!("{}/", server.uri()))
    };

    let outcome = perform_check(&check_context(), &monitor).await;

    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}

/// The key that opens a monitor's secrets is the context's, which a test (or
/// another kind of check) can swap with struct update syntax.
#[tokio::test]
async fn the_cipher_of_the_context_is_the_one_that_opens_the_secrets() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;
    let monitor = configured_monitor(format!("{}/orders", server.uri()));

    let right_key = perform_check(&check_context(), &monitor).await;
    assert_eq!(right_key.status, CheckStatus::Up, "{:?}", right_key.error);

    let other_key = CheckContext {
        cipher: Cipher::new(&[9u8; 32]),
        ..check_context()
    };
    let outcome = perform_check(&other_key, &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.error.as_deref(), Some(CONFIG_DECRYPT_ERROR));
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "only the first check sent a request"
    );
}

/// Security: a monitor whose secrets cannot be decrypted goes down with the
/// generic error, the pass still checks every other monitor, and the alert
/// email carries no secret.
#[sqlx::test]
async fn the_worker_survives_an_undecryptable_monitor_and_its_alert_has_no_secret(
    pool: sqlx::PgPool,
) {
    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let recipient = unique_email("oncall");
    let user = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, user, "viewer").await;
    let healthy_server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&healthy_server)
        .await;
    let broken = Uuid::new_v4();
    let (mut sealed, names) = sealed_headers_for(&test_cipher(), broken, &[("X-Api-Key", API_KEY)]);
    sealed.as_mut().unwrap()[15] ^= 0x01;
    sqlx::query(
        "insert into monitors (id, project_id, name, check_type, url, confirmation_threshold, request_headers_enc, request_header_names)
         values ($1, $2, 'Broken secrets', 'http', 'http://127.0.0.1:9/', 1, $3, $4)",
    )
    .bind(broken)
    .bind(project)
    .bind(sealed)
    .bind(names)
    .execute(&pool)
    .await
    .unwrap();
    let healthy = seed_http_monitor(&pool, project, "Healthy", &healthy_server.uri(), 1).await;

    assert_eq!(
        run_due_checks(&state).await.unwrap(),
        2,
        "both monitors were checked"
    );

    let status = |id| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("select current_status from monitors where id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(status(broken).await, "down");
    assert_eq!(status(healthy).await, "up");
    let cause: Option<String> =
        sqlx::query_scalar("select cause from incidents where monitor_id = $1")
            .bind(broken)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cause.as_deref(), Some(CONFIG_DECRYPT_ERROR));
    let mail = settled_mail(&recipient, 1).await;
    assert_eq!(mail.len(), 1, "{mail:?}");
    assert!(mail[0].1.contains(CONFIG_DECRYPT_ERROR), "{}", mail[0].1);
    assert!(!mail[0].1.contains(API_KEY) && !mail[0].0.contains(API_KEY));
}

/// How the handshakes of a TLS server went.
#[derive(Default)]
struct Handshakes {
    full: AtomicUsize,
    resumed: AtomicUsize,
}

/// An HTTPS server (self-signed certificate) answering `200` and counting how
/// each handshake went. Returns its port.
async fn serve_https_counting(handshakes: Arc<Handshakes>) -> u16 {
    let acceptor = tls::acceptor(tls::self_signed());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (acceptor, handshakes) = (acceptor.clone(), Arc::clone(&handshakes));
            tokio::spawn(async move {
                let Ok(mut connection) = acceptor.accept(stream).await else {
                    return;
                };
                let counter = match connection.get_ref().1.handshake_kind() {
                    Some(rustls::HandshakeKind::Resumed) => &handshakes.resumed,
                    _ => &handshakes.full,
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let mut request = [0u8; 4096];
                let _ = connection.read(&mut request).await;
                let _ = connection
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok",
                    )
                    .await;
                let _ = connection.shutdown().await;
            });
        }
    });
    port
}

/// Clients that outlive a pass would resume the TLS sessions of earlier checks,
/// skipping the verification of the certificate the server presents now: every
/// pass builds its own, so each of its checks starts with a full handshake.
#[sqlx::test]
async fn a_pass_never_resumes_the_tls_session_of_an_earlier_one(pool: sqlx::PgPool) {
    let handshakes = Arc::new(Handshakes::default());
    let port = serve_https_counting(Arc::clone(&handshakes)).await;
    let monitor = Monitor {
        verify_ssl: false,
        ..http_monitor(format!("https://localhost:{port}/"))
    };

    // The control: one context, used twice, does resume the session. Without
    // it, "no session was resumed" below could just mean none could be.
    let shared = check_context();
    for _ in 0..2 {
        let outcome = perform_check(&shared, &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    }
    assert_eq!(handshakes.resumed.load(Ordering::SeqCst), 1);
    handshakes.full.store(0, Ordering::SeqCst);
    handshakes.resumed.store(0, Ordering::SeqCst);

    let state = AppState::for_test(pool.clone());
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "TLS", &monitor.url.unwrap(), 1).await;
    sqlx::query("update monitors set verify_ssl = false where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    for _ in 0..3 {
        common::make_due(&pool, id).await;
        assert_eq!(run_due_checks(&state).await.unwrap(), 1);
    }

    assert_eq!(handshakes.full.load(Ordering::SeqCst), 3);
    assert_eq!(handshakes.resumed.load(Ordering::SeqCst), 0);
}

/// The whole path: a monitor created through the API (secrets sealed for its
/// row), claimed from the database by the worker, opened by the checker and
/// sent to the target as configured.
#[sqlx::test]
async fn the_worker_sends_what_the_api_stored(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let app = common::app(state.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/orders"))
        .and(header("x-api-key", API_KEY))
        .and(header("content-type", "application/json"))
        .and(header("authorization", BASIC))
        .and(body_string("{\"probe\":true}"))
        .respond_with(ResponseTemplate::new(201))
        .expect(1)
        .mount(&server)
        .await;

    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(json!({
            "name": "Orders API",
            "check_type": "http",
            "url": format!("{}/orders", server.uri()),
            "http_method": "POST",
            "request_body": "{\"probe\":true}",
            "request_headers": [
                { "name": "X-Api-Key", "value": API_KEY },
                { "name": "Content-Type", "value": "application/json" }
            ],
            "auth_username": "monitor",
            "auth_password": PASSWORD
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let id = Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    common::make_due(&pool, id).await;

    assert_eq!(run_due_checks(&state).await.unwrap(), 1);

    let current: String = sqlx::query_scalar("select current_status from monitors where id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        current, "up",
        "the target answers 201 only to the exact request"
    );
}

/// Security (Phase 2 final review): a 307/308 re-sends the request body, and
/// a body often carries a token. A body-carrying request only follows
/// redirects that stay on the monitored origin, like headers and credentials.
/// The live repro: POST with a JSON token, 307 to another host of the same
/// machine, which received the whole body.
#[tokio::test]
async fn a_request_body_never_follows_a_redirect_to_another_origin() {
    const BODY: &str = "{\"token\":\"BODY-SECRET-789\"}";
    for status in [307, 308, 303, 302] {
        let target = MockServer::start().await;
        let elsewhere = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(ResponseTemplate::new(200))
            .mount(&elsewhere)
            .await;
        let port = elsewhere.address().port();
        Mock::given(path("/submit"))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("Location", format!("http://localhost:{port}/collect")),
            )
            .mount(&target)
            .await;
        let monitor = Monitor {
            http_method: "POST".to_string(),
            request_body: Some(BODY.to_string()),
            ..http_monitor(format!("{}/submit", target.uri()))
        };

        let outcome = perform_check(&check_context(), &monitor).await;

        assert_eq!(outcome.status, CheckStatus::Down, "{status}");
        let error = outcome.error.unwrap();
        assert_eq!(
            error,
            "headers, credentials and request bodies are only sent to the monitored origin, so the redirect to another host (localhost) was not followed",
            "{status}"
        );
        assert!(!error.contains("BODY-SECRET"));
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "{status}: nothing reached the other origin"
        );
    }
}

/// The same body still follows a redirect on its own origin, and a body
/// stored with a method that sends none (a GET left over from an edit made
/// around the API) changes nothing about where a redirect may go.
#[tokio::test]
async fn a_request_body_rides_along_on_redirects_that_stay_on_the_origin() {
    let server = MockServer::start().await;
    Mock::given(path("/submit"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("Location", format!("{}/landing", server.uri())),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/landing"))
        .and(body_string("{\"probe\":true}"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let monitor = Monitor {
        http_method: "POST".to_string(),
        request_body: Some("{\"probe\":true}".to_string()),
        ..http_monitor(format!("{}/submit", server.uri()))
    };
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);

    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    redirect(
        &target,
        "/start",
        format!("http://localhost:{}/landing", elsewhere.address().port()),
    )
    .await;
    let get_with_stale_body = Monitor {
        request_body: Some("{\"unused\":true}".to_string()),
        ..http_monitor(format!("{}/start", target.uri()))
    };
    let outcome = perform_check(&check_context(), &get_with_stale_body).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
}
