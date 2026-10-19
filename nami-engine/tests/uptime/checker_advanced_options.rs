// Advanced request options in the checker (NAMI-UP-24): IP version, a cookie
// jar per check and the monitor's own HTTP(S) proxy. Pure checker tests: no
// database.
use crate::common;

use common::{
    check_context, proxy::FakeProxy, tls, unsaved_monitor, with_basic_auth, with_headers,
    with_proxy,
};
use nami_engine::{
    uptime::checker::{CONFIG_DECRYPT_ERROR, CheckStatus, DESTINATION_NOT_ALLOWED, perform_check},
    uptime::monitor::model::Monitor,
};
use uuid::Uuid;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{header, method, path},
};

const OK: &str = "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok";
const DENIED: &str =
    "HTTP/1.1 407 Proxy Authentication Required\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";

const CROSS_ORIGIN: &str = "headers, credentials and request bodies are only sent to the monitored origin, so the redirect to another host (localhost) was not followed";

fn http_monitor(url: &str) -> Monitor {
    Monitor {
        url: Some(url.to_string()),
        ..unsaved_monitor("http")
    }
}

// ---- IP version -------------------------------------------------------------

#[tokio::test]
async fn an_ipv6_only_target_with_ipv4_is_down_without_fallback() {
    let ctx = check_context();
    let cases = [
        Monitor {
            url: Some("http://[::1]:9/".into()),
            ip_version: "ipv4".into(),
            ..unsaved_monitor("http")
        },
        Monitor {
            url: Some("http://[::1]:9/".into()),
            keyword: Some("ok".into()),
            ip_version: "ipv4".into(),
            ..unsaved_monitor("keyword")
        },
        Monitor {
            host: Some("::1".into()),
            port: Some(9),
            ip_version: "ipv4".into(),
            ..unsaved_monitor("tcp")
        },
        Monitor {
            host: Some("::1".into()),
            ip_version: "ipv4".into(),
            ..unsaved_monitor("ping")
        },
        Monitor {
            host: Some("::1".into()),
            port: Some(443),
            ip_version: "ipv4".into(),
            ..unsaved_monitor("ssl_expiry")
        },
    ];
    for monitor in cases {
        let outcome = perform_check(&ctx, &monitor).await;
        assert_eq!(outcome.status, CheckStatus::Down, "{}", monitor.check_type);
        assert_eq!(
            outcome.error.as_deref(),
            Some("no IPv4 address"),
            "{}",
            monitor.check_type
        );
    }
    // And the mirror image, for every type that has an address family.
    let v4_only = [
        Monitor {
            url: Some("http://127.0.0.1:9/".into()),
            ip_version: "ipv6".into(),
            ..unsaved_monitor("http")
        },
        Monitor {
            host: Some("127.0.0.1".into()),
            port: Some(9),
            ip_version: "ipv6".into(),
            ..unsaved_monitor("tcp")
        },
        Monitor {
            host: Some("127.0.0.1".into()),
            ip_version: "ipv6".into(),
            ..unsaved_monitor("ping")
        },
        Monitor {
            host: Some("127.0.0.1".into()),
            port: Some(443),
            ip_version: "ipv6".into(),
            ..unsaved_monitor("ssl_expiry")
        },
    ];
    for monitor in v4_only {
        assert_eq!(
            perform_check(&ctx, &monitor).await.error.as_deref(),
            Some("no IPv6 address"),
            "{}",
            monitor.check_type
        );
    }
}

#[tokio::test]
async fn ip_version_picks_the_family_of_a_name() {
    // wiremock listens on 127.0.0.1 only.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let url = format!("http://localhost:{}/", server.address().port());
    let ctx = check_context();
    let v4 = Monitor {
        ip_version: "ipv4".into(),
        ..http_monitor(&url)
    };
    assert_eq!(perform_check(&ctx, &v4).await.status, CheckStatus::Up);
    let v6 = Monitor {
        ip_version: "ipv6".into(),
        ..http_monitor(&url)
    };
    let outcome = perform_check(&ctx, &v6).await;
    // ::1 has no listener (connection refused) or localhost has no AAAA here:
    // down either way, never up via 127.0.0.1.
    assert_eq!(outcome.status, CheckStatus::Down, "{:?}", outcome.error);
    // The same for tcp.
    let tcp = |ip: &str| Monitor {
        host: Some("localhost".into()),
        port: Some(i32::from(server.address().port())),
        ip_version: ip.into(),
        ..unsaved_monitor("tcp")
    };
    assert_eq!(
        perform_check(&ctx, &tcp("ipv4")).await.status,
        CheckStatus::Up
    );
    assert_eq!(
        perform_check(&ctx, &tcp("ipv6")).await.status,
        CheckStatus::Down
    );
}

/// reqwest never hands an IP literal to the resolver, so a redirect hop to a
/// literal of the excluded family is refused by the redirect policy.
#[tokio::test]
async fn a_redirect_to_a_literal_of_the_other_family_is_refused() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(path("/start"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("http://[::1]:{port}/v6")),
        )
        .mount(&server)
        .await;
    let monitor = Monitor {
        ip_version: "ipv4".into(),
        ..http_monitor(&format!("http://127.0.0.1:{port}/start"))
    };
    assert_eq!(
        perform_check(&check_context(), &monitor)
            .await
            .error
            .as_deref(),
        Some("no IPv4 address")
    );
}

// ---- cookies ----------------------------------------------------------------

#[tokio::test]
async fn cookies_are_kept_across_redirects_of_one_check_only() {
    let server = MockServer::start().await;
    Mock::given(path("/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", "/home")
                .insert_header("set-cookie", "session=abc; Path=/"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/home"))
        .and(header("cookie", "session=abc"))
        .respond_with(ResponseTemplate::new(200))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(path("/home"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(2)
        .mount(&server)
        .await;
    let ctx = check_context();
    let base = server.uri();
    let login = Monitor {
        remember_cookies: true,
        ..http_monitor(&format!("{base}/login"))
    };
    let outcome = perform_check(&ctx, &login).await;
    assert_eq!(
        outcome.status,
        CheckStatus::Up,
        "cookie carried to /home: {:?}",
        outcome.error
    );
    // Without the jar the redirect hop has no cookie.
    let no_jar = Monitor {
        remember_cookies: false,
        ..login.clone()
    };
    assert_eq!(perform_check(&ctx, &no_jar).await.status_code, Some(401));
    // The jar died with the check: the same monitor and another one send no
    // cookie later.
    let home = Monitor {
        url: Some(format!("{base}/home")),
        ..login.clone()
    };
    assert_eq!(perform_check(&ctx, &home).await.status_code, Some(401));
    let other = Monitor {
        id: Uuid::new_v4(),
        remember_cookies: true,
        ..http_monitor(&format!("{base}/home"))
    };
    assert_eq!(perform_check(&ctx, &other).await.status_code, Some(401));
}

/// A cookie set by one host is not sent to another host a redirect leads to:
/// reqwest's jar scopes it to the host that set it.
#[tokio::test]
async fn a_cookie_never_follows_a_redirect_to_another_host() {
    let server = MockServer::start().await;
    let port = server.address().port();
    Mock::given(path("/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://localhost:{port}/home"))
                .insert_header("set-cookie", "session=abc; Path=/"),
        )
        .mount(&server)
        .await;
    Mock::given(path("/home"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let monitor = Monitor {
        remember_cookies: true,
        ip_version: "ipv4".into(),
        ..http_monitor(&format!("http://127.0.0.1:{port}/login"))
    };
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let requests = server.received_requests().await.unwrap();
    let home = requests.iter().find(|r| r.url.path() == "/home").unwrap();
    assert!(home.headers.get("cookie").is_none(), "{:?}", home.headers);
}

/// Per-check clients (a cookie jar, a proxy) take their redirect mode from the
/// same decision as the shared clients: credentials or a body keep the request
/// on its origin.
#[tokio::test]
async fn per_check_clients_keep_secrets_and_bodies_on_the_monitored_origin() {
    let target = MockServer::start().await;
    let elsewhere = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200))
        .mount(&elsewhere)
        .await;
    let port = elsewhere.address().port();
    Mock::given(path("/go"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("http://localhost:{port}/collect")),
        )
        .mount(&target)
        .await;
    let url = format!("{}/go", target.uri());
    let jar = Monitor {
        remember_cookies: true,
        ..http_monitor(&url)
    };
    let cases = [
        (
            "header",
            with_headers(jar.clone(), &[("X-Api-Key", "k-123")]),
        ),
        ("basic auth", with_basic_auth(jar.clone(), "u", "p")),
        (
            "body",
            Monitor {
                http_method: "POST".into(),
                request_body: Some("{\"token\":\"t\"}".into()),
                ..jar.clone()
            },
        ),
    ];
    for (label, monitor) in cases {
        let outcome = perform_check(&check_context(), &monitor).await;
        assert_eq!(outcome.error.as_deref(), Some(CROSS_ORIGIN), "{label}");
    }
    assert!(elsewhere.received_requests().await.unwrap().is_empty());
    // A plain monitor with a jar still follows it.
    assert_eq!(
        perform_check(&check_context(), &jar).await.status,
        CheckStatus::Up
    );
}

// ---- proxy ------------------------------------------------------------------

#[tokio::test]
async fn checks_go_through_the_monitors_proxy_with_its_credentials() {
    let proxy = FakeProxy::start(OK).await;
    let monitor = with_proxy(
        http_monitor("http://target.test/health"),
        &format!("http://127.0.0.1:{}", proxy.port),
        Some(("nami", "pr0xy")),
    );
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let seen = proxy.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].request_line,
        "GET http://target.test/health HTTP/1.1"
    );
    assert_eq!(
        seen[0].proxy_authorization.as_deref(),
        Some("Basic bmFtaTpwcjB4eQ==")
    );
    // Without credentials no Proxy-Authorization is sent.
    let anonymous = FakeProxy::start(OK).await;
    let monitor = with_proxy(
        http_monitor("http://target.test/"),
        &format!("http://127.0.0.1:{}", anonymous.port),
        None,
    );
    assert_eq!(
        perform_check(&check_context(), &monitor).await.status,
        CheckStatus::Up
    );
    assert_eq!(anonymous.requests()[0].proxy_authorization, None);
}

#[tokio::test]
async fn proxy_errors_say_the_proxy_failed() {
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let target = http_monitor("http://target.test/");
    let unreachable = with_proxy(target.clone(), &format!("http://127.0.0.1:{dead}"), None);
    let error = perform_check(&check_context(), &unreachable)
        .await
        .error
        .unwrap();
    assert!(error.starts_with("proxy connection failed"), "{error}");
    let tunnel_unreachable = with_proxy(
        http_monitor("https://target.test/"),
        &format!("http://127.0.0.1:{dead}"),
        None,
    );
    let error = perform_check(&check_context(), &tunnel_unreachable)
        .await
        .error
        .unwrap();
    assert!(error.starts_with("proxy connection failed"), "{error}");

    let denied = FakeProxy::start(DENIED).await;
    let refused = with_proxy(
        target.clone(),
        &format!("http://127.0.0.1:{}", denied.port),
        Some(("nami", "wrong")),
    );
    let outcome = perform_check(&check_context(), &refused).await;
    assert_eq!(
        outcome.error.as_deref(),
        Some("proxy authentication failed (HTTP 407)")
    );
    // HTTPS targets tunnel with CONNECT: a 407 there is the same message.
    let tunnel = with_proxy(
        http_monitor("https://target.test/"),
        &format!("http://127.0.0.1:{}", denied.port),
        None,
    );
    assert_eq!(
        perform_check(&check_context(), &tunnel)
            .await
            .error
            .as_deref(),
        Some("proxy authentication failed (HTTP 407)")
    );
    assert!(
        denied.requests()[1]
            .request_line
            .starts_with("CONNECT target.test:443 ")
    );
    // The target's own answer stays the target's.
    let upstream = FakeProxy::start(
        "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    let failing = with_proxy(target, &format!("http://127.0.0.1:{}", upstream.port), None);
    assert_eq!(
        perform_check(&check_context(), &failing)
            .await
            .error
            .as_deref(),
        Some("expected status 200-399, got 503")
    );
    // A CONNECT the proxy refuses otherwise is a tunnel failure.
    let forbidden = FakeProxy::start(
        "HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    let tunnel = with_proxy(
        http_monitor("https://target.test/"),
        &format!("http://127.0.0.1:{}", forbidden.port),
        None,
    );
    assert_eq!(
        perform_check(&check_context(), &tunnel)
            .await
            .error
            .as_deref(),
        Some("proxy tunnel failed: unsuccessful")
    );
}

/// A 407 is the proxy's, whatever range the monitor expects: it is checked
/// before the expected status.
#[tokio::test]
async fn a_407_from_the_proxy_is_never_an_expected_status() {
    let denied = FakeProxy::start(DENIED).await;
    let monitor = Monitor {
        expected_status_min: 200,
        expected_status_max: 499,
        ..with_proxy(
            http_monitor("http://target.test/"),
            &format!("http://127.0.0.1:{}", denied.port),
            None,
        )
    };
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    assert_eq!(outcome.status_code, Some(407));
    assert_eq!(
        outcome.error.as_deref(),
        Some("proxy authentication failed (HTTP 407)")
    );
    // Without a proxy a 407 is just the target's status.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(407))
        .mount(&server)
        .await;
    let direct = Monitor {
        expected_status_min: 200,
        expected_status_max: 499,
        ..http_monitor(&server.uri())
    };
    assert_eq!(
        perform_check(&check_context(), &direct).await.status,
        CheckStatus::Up
    );
}

#[tokio::test]
async fn a_link_local_proxy_is_refused_before_any_connection() {
    for proxy in [
        "http://169.254.169.254:3128",
        "http://[fe80::1]:3128",
        "http://0.0.0.0:3128",
        "http://[::ffff:169.254.169.254]:80",
        "http://100.100.100.200:80",
        "https://[fd00:ec2::254]:443",
    ] {
        let monitor = with_proxy(http_monitor("http://example.test/"), proxy, None);
        assert_eq!(
            perform_check(&check_context(), &monitor)
                .await
                .error
                .as_deref(),
            Some(DESTINATION_NOT_ALLOWED),
            "{proxy}"
        );
    }
    // A target IP literal that is forbidden stays refused behind a proxy.
    let proxy = FakeProxy::start(OK).await;
    let monitor = with_proxy(
        http_monitor("http://169.254.169.254/latest/"),
        &format!("http://127.0.0.1:{}", proxy.port),
        None,
    );
    assert_eq!(
        perform_check(&check_context(), &monitor)
            .await
            .error
            .as_deref(),
        Some(DESTINATION_NOT_ALLOWED)
    );
    // And so does a redirect hop to one.
    let redirecting = FakeProxy::start(
        "HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/latest/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    let monitor = with_proxy(
        http_monitor("http://target.test/"),
        &format!("http://127.0.0.1:{}", redirecting.port),
        None,
    );
    assert_eq!(
        perform_check(&check_context(), &monitor)
            .await
            .error
            .as_deref(),
        Some(DESTINATION_NOT_ALLOWED)
    );
    assert_eq!(redirecting.requests().len(), 1, "the hop never went out");
    assert!(proxy.requests().is_empty());
}

#[tokio::test]
async fn an_undecryptable_proxy_password_sends_nothing() {
    let proxy = FakeProxy::start(OK).await;
    let mut monitor = with_proxy(
        http_monitor("http://target.test/"),
        &format!("http://127.0.0.1:{}", proxy.port),
        Some(("nami", "pw")),
    );
    monitor.id = Uuid::new_v4(); // sealed for another id
    assert_eq!(
        perform_check(&check_context(), &monitor)
            .await
            .error
            .as_deref(),
        Some(CONFIG_DECRYPT_ERROR)
    );
    assert!(proxy.requests().is_empty());
}

/// Cookies work behind a proxy too, within the one check.
#[tokio::test]
async fn cookies_ride_along_through_the_proxy_within_one_check() {
    let proxy = FakeProxy::start(
        "HTTP/1.1 302 Found\r\nlocation: /next\r\nset-cookie: s=1; Path=/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
    )
    .await;
    let monitor = Monitor {
        remember_cookies: true,
        follow_redirects: true,
        ..with_proxy(
            http_monitor("http://target.test/"),
            &format!("http://127.0.0.1:{}", proxy.port),
            None,
        )
    };
    // The fake answers every hop with the same redirect: too many redirects,
    // but every hop after the first carries the cookie.
    let outcome = perform_check(&check_context(), &monitor).await;
    assert_eq!(outcome.status, CheckStatus::Down);
    let seen = proxy.requests();
    assert!(seen.len() > 1);
    assert_eq!(seen[0].cookie, None);
    assert_eq!(seen[1].cookie.as_deref(), Some("s=1"));
    // A second check starts with an empty jar.
    let before = seen.len();
    perform_check(&check_context(), &monitor).await;
    assert_eq!(proxy.requests()[before].cookie, None);
}

/// The TLS connection to an https proxy is always verified: `verify_ssl`
/// applies to the target only.
#[tokio::test]
async fn an_https_proxy_certificate_is_verified_even_when_verify_ssl_is_off() {
    let proxy = FakeProxy::start_tls(OK, tls::self_signed()).await;
    for verify_ssl in [true, false] {
        for target in ["http://target.test/", "https://target.test/"] {
            let monitor = Monitor {
                verify_ssl,
                ..with_proxy(
                    http_monitor(target),
                    &format!("https://localhost:{}", proxy.port),
                    Some(("nami", "pr0xy")),
                )
            };
            let outcome = perform_check(&check_context(), &monitor).await;
            let error = outcome.error.unwrap_or_default();
            assert!(
                error.starts_with("proxy connection failed") && error.contains("certificate"),
                "verify_ssl={verify_ssl} {target}: {error}"
            );
        }
    }
    assert!(proxy.requests().is_empty(), "no request reached the proxy");
}

/// ...while `verify_ssl` still decides about the target's certificate behind
/// a (tunnelling) proxy.
#[tokio::test]
async fn verify_ssl_still_applies_to_the_target_behind_a_proxy() {
    let target_port = tls::serve_https_ok(tls::self_signed()).await;
    let proxy = FakeProxy::start_tunnel().await;
    let url = format!("https://localhost:{target_port}/");
    let unverified = Monitor {
        verify_ssl: false,
        ..with_proxy(
            http_monitor(&url),
            &format!("http://127.0.0.1:{}", proxy.port),
            None,
        )
    };
    let outcome = perform_check(&check_context(), &unverified).await;
    assert_eq!(outcome.status, CheckStatus::Up, "{:?}", outcome.error);
    let verified = Monitor {
        verify_ssl: true,
        ..unverified.clone()
    };
    let error = perform_check(&check_context(), &verified)
        .await
        .error
        .unwrap();
    assert!(!error.starts_with("proxy"), "the target failed: {error}");
    assert!(error.contains("certificate"), "{error}");
    assert!(
        proxy
            .requests()
            .iter()
            .all(|r| r.request_line == format!("CONNECT localhost:{target_port} HTTP/1.1"))
    );
}

/// Which side failed is decided by the failing hop's scheme: an http monitor
/// redirected (through the proxy) to an https URL fails inside the tunnel,
/// and a certificate error there is the target's, not the proxy's.
#[tokio::test]
async fn a_redirect_to_https_behind_a_proxy_blames_the_target_for_its_certificate() {
    let target_port = tls::serve_https_ok(tls::self_signed()).await;
    let proxy = FakeProxy::start_tunnel_or_reply(format!(
        "HTTP/1.1 302 Found\r\nlocation: https://localhost:{target_port}/\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
    ))
    .await;
    let monitor = with_proxy(
        http_monitor("http://target.test/start"),
        &format!("http://127.0.0.1:{}", proxy.port),
        None,
    );
    let error = perform_check(&check_context(), &monitor)
        .await
        .error
        .unwrap();
    assert!(!error.starts_with("proxy"), "the target failed: {error}");
    assert!(error.contains("certificate"), "{error}");
    let seen = proxy.requests();
    assert_eq!(
        seen[0].request_line,
        "GET http://target.test/start HTTP/1.1"
    );
    assert_eq!(
        seen[1].request_line,
        format!("CONNECT localhost:{target_port} HTTP/1.1")
    );
}
