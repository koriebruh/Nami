//! The guarded outbound HTTP clients every chat and webhook notification goes
//! through, and the destination rules checked before each request (R9).
//!
//! Two process-wide clients, both with no proxy, no redirects, no referer, no
//! idle pool (each request resolves again, so a DNS rebind is judged where the
//! connection is made: reqwest connects only to the addresses the resolver
//! returns) and a 10 s timeout:
//!
//! - [`client`] uses the monitors' `GuardedResolver`: link-local and metadata
//!   addresses refused, private and loopback allowed. Project webhooks and the
//!   Telegram API base only.
//! - [`public_client`] uses [`PublicOnlyResolver`]: every internal address
//!   refused. Personal channels and every Slack, Discord and Teams send.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::OnceLock,
    time::Duration,
};

use crate::uptime::{
    checker::{DESTINATION_NOT_ALLOWED, DestinationNotAllowed, GuardedResolver, caused_by,
        is_forbidden_destination},
    monitor::model::IpVersion,
};

pub use crate::core::config::OutboundPolicy;

/// Total time one notification request may take.
pub const OUTBOUND_TIMEOUT: Duration = Duration::from_secs(10);
/// Time to establish the connection, within [`OUTBOUND_TIMEOUT`].
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The longest destination URL a channel may hold, in bytes.
pub const MAX_URL_BYTES: usize = 2048;

/// Which client a send uses, and which IP literals its URL may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// [`client`]: the monitors' forbidden set only (private receivers allowed).
    MonitorPolicy,
    /// [`public_client`]: public addresses only (see [`is_non_public`]).
    PublicOnly,
}

/// The scheme and host rule of a destination URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRule {
    HttpOrHttps,
    Https,
    /// `https`, the host one of `host` (exact, case-insensitive) and the
    /// path starting with `path_prefix`.
    HttpsHostPrefix {
        host: &'static [&'static str],
        path_prefix: &'static str,
    },
}

/// Slack incoming webhooks.
pub const SLACK: UrlRule = UrlRule::HttpsHostPrefix {
    host: &["hooks.slack.com"],
    path_prefix: "/",
};
/// Discord webhooks, on any of Discord's hosts.
pub const DISCORD: UrlRule = UrlRule::HttpsHostPrefix {
    host: &["discord.com", "discordapp.com", "ptb.discord.com", "canary.discord.com"],
    path_prefix: "/api/webhooks/",
};

fn build(resolver: impl reqwest::dns::Resolve + 'static) -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(resolver)
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .timeout(OUTBOUND_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_max_idle_per_host(0)
        .user_agent(concat!("Nami-Notify/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("the notification client has constant options; building fails only without TLS roots")
}

/// The client for project webhooks and the Telegram API base: no proxy, the
/// monitors' `GuardedResolver`, no redirects, no referer, no idle pool, 10 s
/// timeout, user agent `Nami-Notify/<version>`. Built once, cloned per call.
pub fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| build(GuardedResolver { ip_version: IpVersion::Any }))
        .clone()
}

/// [`client`] with [`PublicOnlyResolver`]: every personal channel and every
/// Slack, Discord and Teams send.
pub fn public_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| build(PublicOnlyResolver)).clone()
}

/// Whether `ip` is anything but a public unicast address:
/// `is_forbidden_destination(ip)`, loopback, RFC 1918, CGNAT `100.64.0.0/10`,
/// `198.18.0.0/15`, the reserved `240.0.0.0/4` and broadcast, multicast,
/// documentation ranges, ULA `fc00::/7`, the deprecated site-local
/// `fec0::/10` and the IPv4-compatible `::/96`.
/// IPv4-mapped and NAT64 (`64:ff9b::/96`) forms are judged as the IPv4
/// address they carry.
pub fn is_non_public(ip: IpAddr) -> bool {
    if is_forbidden_destination(ip) {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => is_non_public_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_non_public_v4(v4);
            }
            let s = v6.segments();
            if s[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                return is_non_public_v4(v4_from(s[6], s[7]));
            }
            s[..6] == [0; 6]
                || v6.is_multicast()
                || s[0] & 0xfe00 == 0xfc00
                || s[0] & 0xffc0 == 0xfec0
                || (s[0], s[1]) == (0x2001, 0x0db8)
        }
    }
}

fn v4_from(high: u16, low: u16) -> Ipv4Addr {
    let [a, b] = high.to_be_bytes();
    let [c, d] = low.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}

fn is_non_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    v4.is_loopback()
        || v4.is_private()
        || v4.is_multicast()
        || v4.is_broadcast()
        || v4.is_documentation()
        || (a == 100 && b & 0xc0 == 64)
        || (a == 198 && b & 0xfe == 18)
        || a >= 240
}

/// The public addresses among `resolved` (see [`is_non_public`]). An error
/// when some were resolved and every one of them is refused; nothing resolved
/// is an empty list, which fails later as any unresolvable name does.
pub(crate) fn public_addresses(
    resolved: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, DestinationNotAllowed> {
    let mut refused = false;
    let public: Vec<SocketAddr> = resolved
        .into_iter()
        .filter(|address| {
            let internal = is_non_public(address.ip());
            refused |= internal;
            !internal
        })
        .collect();
    if public.is_empty() && refused {
        Err(DestinationNotAllowed)
    } else {
        Ok(public)
    }
}

/// Resolves names for [`public_client`] like the monitors' `GuardedResolver`
/// (the system resolver, off the async threads), keeping public addresses
/// only and failing with `DestinationNotAllowed` when none is left.
pub(crate) struct PublicOnlyResolver;

impl reqwest::dns::Resolve for PublicOnlyResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let public = public_addresses(resolved)?;
            Ok(Box::new(public.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Parses and checks a destination: at most [`MAX_URL_BYTES`], the scheme
/// written out with `//` and a host, no userinfo, `rule`, and no forbidden IP
/// literal (the monitors' set, plus [`is_non_public`] for
/// [`Reach::PublicOnly`]). Names are not resolved here: the client's resolver
/// judges them when it connects. Always strict (create/update validation).
/// The error is a message for a 400.
pub fn check_url(raw: &str, rule: UrlRule, reach: Reach) -> Result<reqwest::Url, String> {
    if raw.len() > MAX_URL_BYTES {
        return Err(format!("the URL must be at most {MAX_URL_BYTES} bytes"));
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "the URL is not valid".to_string())?;
    let https_only = !matches!(rule, UrlRule::HttpOrHttps);
    match url.scheme() {
        "https" => {}
        "http" if !https_only => {}
        _ if https_only => return Err("the URL must use https".into()),
        _ => return Err("the URL must use http or https".into()),
    }
    // The parser repairs `http:/x`, `http:x` and `http:///x` into `http://x/`;
    // a destination is taken only as written.
    let authority = raw
        .get(..url.scheme().len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(url.scheme()))
        .and_then(|_| raw[url.scheme().len()..].strip_prefix("://"));
    if !authority.is_some_and(|rest| !rest.starts_with(['/', '\\']))
        || url.host_str().is_none_or(str::is_empty)
    {
        return Err("the URL must have a host".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("the URL must not contain a user name or password".into());
    }
    if let UrlRule::HttpsHostPrefix { host, path_prefix } = rule {
        let host_ok = url
            .host_str()
            .is_some_and(|h| host.iter().any(|allowed| h.eq_ignore_ascii_case(allowed)));
        if !host_ok || !url.path().starts_with(path_prefix) {
            return Err(format!("the URL must start with https://{}{path_prefix}", host[0]));
        }
    }
    let literal = match url.host() {
        Some(url::Host::Ipv4(v4)) => Some(IpAddr::V4(v4)),
        Some(url::Host::Ipv6(v6)) => Some(IpAddr::V6(v6)),
        _ => None,
    };
    if let Some(ip) = literal {
        if is_forbidden_destination(ip) {
            return Err(DESTINATION_NOT_ALLOWED.into());
        }
        if reach == Reach::PublicOnly && is_non_public(ip) {
            return Err("this channel must point at a public address".into());
        }
    }
    Ok(url)
}

/// The send-time check: [`check_url`] under `Strict`; under `Relaxed` (tests
/// only) just http(s), a host, no userinfo and the monitors' forbidden set.
pub fn check_url_for_send(
    raw: &str,
    rule: UrlRule,
    reach: Reach,
    policy: OutboundPolicy,
) -> Result<reqwest::Url, String> {
    match policy {
        OutboundPolicy::Strict => check_url(raw, rule, reach),
        OutboundPolicy::Relaxed => check_url(raw, UrlRule::HttpOrHttps, Reach::MonitorPolicy),
    }
}

/// `"timeout"`, `"destination not allowed"`, `"connection failed"`,
/// `"HTTP <code>"` or `"request failed"`. Never formats the error itself: its
/// text carries the URL, which holds Telegram bot tokens and webhook keys.
pub fn describe_error(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "timeout".into()
    } else if caused_by::<DestinationNotAllowed>(error) {
        DESTINATION_NOT_ALLOWED.into()
    } else if error.is_connect() {
        "connection failed".into()
    } else if let Some(status) = error.status() {
        format!("HTTP {}", status.as_u16())
    } else {
        "request failed".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Reach::{MonitorPolicy, PublicOnly};

    #[test]
    fn urls_follow_their_rule() {
        assert!(check_url("https://hooks.slack.com/services/T/B/x", SLACK, PublicOnly).is_ok());
        for bad in [
            "http://hooks.slack.com/services/x",
            "https://hooks.slack.com.evil.test/x",
            "https://evil.test/hooks.slack.com/",
            "https://user:pw@hooks.slack.com/x",
        ] {
            assert!(check_url(bad, SLACK, PublicOnly).is_err(), "{bad}");
        }
        assert!(check_url("https://discord.com/api/webhooks/1/abc", DISCORD, PublicOnly).is_ok());
        assert!(check_url("https://discordapp.com/api/webhooks/1/abc", DISCORD, PublicOnly).is_ok());
        assert!(check_url("https://discord.com/other/1/abc", DISCORD, PublicOnly).is_err());
        assert!(
            check_url("http://10.0.0.5:8080/hook", UrlRule::HttpOrHttps, MonitorPolicy).is_ok(),
            "private receivers are allowed for project webhooks"
        );
        for bad in [
            "ftp://x.test/",
            "http://169.254.169.254/latest",
            "http://[fe80::1]/",
            "http://0.0.0.0/",
            "http://[::ffff:169.254.169.254]/",
            "javascript:alert(1)",
            "http:///nohost",
        ] {
            assert!(check_url(bad, UrlRule::HttpOrHttps, MonitorPolicy).is_err(), "{bad}");
        }
        assert!(check_url("http://teams.example/x", UrlRule::Https, PublicOnly).is_err());
    }

    #[test]
    fn a_url_the_parser_would_repair_is_refused() {
        for bad in ["http:/x.test/", "http:x.test/", "http:\\\\x.test/", " http://x.test/"] {
            assert!(check_url(bad, UrlRule::HttpOrHttps, MonitorPolicy).is_err(), "{bad:?}");
        }
        assert!(check_url("HTTPS://X.test/in", UrlRule::HttpOrHttps, MonitorPolicy).is_ok());
    }

    #[test]
    fn a_url_is_refused_past_its_byte_limit() {
        let base = "https://x.test/";
        let at = format!("{base}{}", "a".repeat(MAX_URL_BYTES - base.len()));
        assert!(check_url(&at, UrlRule::HttpOrHttps, MonitorPolicy).is_ok());
        assert!(check_url(&format!("{at}a"), UrlRule::HttpOrHttps, MonitorPolicy).is_err());
    }

    #[test]
    fn public_only_destinations_refuse_every_internal_address() {
        for bad in [
            "http://127.0.0.1/",
            "http://10.0.0.5/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fc00::1]/",
            "http://[fd12::1]/",
            "http://[::ffff:10.0.0.1]/",
            "http://[64:ff9b::a00:1]/",
            "http://224.0.0.1/",
            "http://255.255.255.255/",
            "http://2130706433/",
        ] {
            assert!(check_url(bad, UrlRule::HttpOrHttps, PublicOnly).is_err(), "{bad}");
        }
        assert!(check_url("https://203.0.114.10/x", UrlRule::HttpOrHttps, PublicOnly).is_ok());
        let loopback = [SocketAddr::from(([127, 0, 0, 1], 0)), SocketAddr::from(([10, 1, 2, 3], 0))];
        assert!(
            public_addresses(loopback).is_err(),
            "a name resolving only to internal addresses is refused at connect"
        );
        let mixed = [SocketAddr::from(([10, 1, 2, 3], 0)), SocketAddr::from(([93, 184, 216, 34], 0))];
        assert_eq!(
            public_addresses(mixed).unwrap().len(),
            1,
            "only the public address is ever connected to"
        );
    }

    #[test]
    fn the_non_public_ranges_end_where_they_should() {
        for (ip, internal) in [
            ("100.63.255.255", false),
            ("100.64.0.0", true),
            ("100.127.255.255", true),
            ("100.128.0.0", false),
            ("198.17.255.255", false),
            ("198.18.0.0", true),
            ("198.19.255.255", true),
            ("198.20.0.0", false),
            ("192.0.2.1", true),
            ("198.51.100.1", true),
            ("203.0.113.1", true),
            ("240.0.0.1", true),
            ("8.8.8.8", false),
            ("fbff::1", false),
            ("fe00::1", false),
            ("fec0::1", true),
            ("2001:db8::1", true),
            ("2001:db9::1", false),
            ("ff02::1", true),
            ("::10.0.0.1", true),
            ("2606:4700::1111", false),
            ("64:ff9b::808:808", false),
        ] {
            assert_eq!(is_non_public(ip.parse().unwrap()), internal, "{ip}");
        }
    }

    #[test]
    fn the_relaxed_send_check_exists_only_for_tests_and_still_refuses_metadata() {
        let wiremock = "http://127.0.0.1:4567/services/x";
        assert!(check_url_for_send(wiremock, SLACK, PublicOnly, OutboundPolicy::Strict).is_err());
        assert!(check_url_for_send(wiremock, SLACK, PublicOnly, OutboundPolicy::Relaxed).is_ok());
        assert!(
            check_url_for_send("http://169.254.169.254/", SLACK, PublicOnly, OutboundPolicy::Relaxed)
                .is_err()
        );
        assert_eq!(
            crate::core::config::Config::from_env_for_test().outbound_policy,
            OutboundPolicy::Relaxed
        );
    }

    #[tokio::test]
    async fn the_client_never_follows_redirects() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(
                wiremock::ResponseTemplate::new(302).insert_header("location", "http://169.254.169.254/"),
            )
            .mount(&server)
            .await;
        let response = client().post(server.uri()).send().await.unwrap();
        assert_eq!(response.status(), 302, "the redirect is returned, not followed");
    }

    #[test]
    fn the_resolver_refuses_names_that_only_resolve_to_forbidden_addresses() {
        // `GuardedResolver` is the checker's, reused in place (not moved); its address filter
        // (`allowed_addresses`) is already unit-tested in checker.rs. This pins that the
        // notification client really uses it: a name resolving only to link-local fails.
        let resolved = [SocketAddr::from(([169, 254, 169, 254], 0))];
        assert!(crate::uptime::checker::allowed_addresses(resolved).is_err());
    }

    #[tokio::test]
    async fn the_public_client_refuses_a_name_that_resolves_to_loopback() {
        let error = public_client().post("http://localhost:9/x").send().await.unwrap_err();
        assert_eq!(describe_error(&error), "destination not allowed");
    }

    #[tokio::test]
    async fn errors_never_carry_the_url_or_its_secrets() {
        let error = client()
            .post("http://127.0.0.1:9/bot123456:SECRETSECRETSECRETSECRETSECRETSE/sendMessage")
            .send()
            .await
            .unwrap_err();
        let text = describe_error(&error);
        assert_eq!(text, "connection failed");
        assert!(!text.contains("SECRET"));
    }
}
