//! Performs one check of one monitor. Never fails: every problem (timeout,
//! refused connection, wrong status, missing keyword, a destination no check
//! may contact) becomes a `Down` outcome with a short human-readable `error`.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::models::monitor::Monitor;

/// Keyword checks read at most this much of the body; a keyword that only
/// appears after the first MiB counts as not found.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// An outcome's `error` keeps at most this many chars, followed by an ellipsis
/// when it was cut. Part of the text can come from the target (a TLS error
/// lists the names on the peer's certificate) and callers store it verbatim,
/// so it is capped once, where the outcome is built.
pub const MAX_ERROR_CHARS: usize = 256;

/// The error of a check that refused its destination (see
/// [`is_forbidden_destination`]). Deliberately generic.
pub const DESTINATION_NOT_ALLOWED: &str = "destination not allowed";

/// Why a check did not contact its destination: every address it names or
/// resolves to is one no check may contact (see [`is_forbidden_destination`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestinationNotAllowed;

impl fmt::Display for DestinationNotAllowed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(DESTINATION_NOT_ALLOWED)
    }
}

impl std::error::Error for DestinationNotAllowed {}

/// Addresses no check may contact, whatever the monitor says:
///
/// - link-local addresses, `169.254.0.0/16` and `fe80::/10`, where cloud
///   metadata services answer (`169.254.169.254`) and hand out credentials;
/// - AWS's IPv6 metadata address, `fd00:ec2::254`;
/// - "this network", `0.0.0.0/8`, and the unspecified address `::`, which
///   reach the engine's own host;
/// - any of these written as an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`)
///   or behind the NAT64 well-known prefix (`64:ff9b::a.b.c.d`).
///
/// Everything else stays allowed, private networks and loopback included:
/// monitoring internal services is what a self-hosted monitor is for.
pub fn is_forbidden_destination(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_forbidden_v4(v4),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_forbidden_v4(v4);
            }
            let segments = v6.segments();
            if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
                let [a, b] = segments[6].to_be_bytes();
                let [c, d] = segments[7].to_be_bytes();
                return is_forbidden_v4(Ipv4Addr::new(a, b, c, d));
            }
            v6.is_unspecified()
                || segments[0] & 0xffc0 == 0xfe80
                || v6 == Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
        }
    }
}

fn is_forbidden_v4(v4: Ipv4Addr) -> bool {
    v4.octets()[0] == 0 || v4.is_link_local()
}

/// The addresses among `resolved` that a check may contact. An error when
/// some were resolved and every one of them is forbidden; nothing resolved is
/// an empty list, which fails later as any unresolvable name does.
fn allowed_addresses(
    resolved: impl IntoIterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, DestinationNotAllowed> {
    let mut forbidden = false;
    let allowed: Vec<SocketAddr> = resolved
        .into_iter()
        .filter(|address| {
            let refused = is_forbidden_destination(address.ip());
            forbidden |= refused;
            !refused
        })
        .collect();
    if allowed.is_empty() && forbidden {
        Err(DestinationNotAllowed)
    } else {
        Ok(allowed)
    }
}

/// Whether `url` names a forbidden address directly. Names are judged where
/// they are resolved (see [`GuardedResolver`]): an IP literal never reaches a
/// resolver, so it is checked here, for the monitor's URL and for every
/// redirect.
fn names_forbidden_address(url: &reqwest::Url) -> bool {
    // The URL parser has already turned every numeric host (`0x7f000001`,
    // `2130706433`, `127.1`) into a dotted IPv4 address and brackets IPv6
    // ones, so a host that parses as neither is a name.
    let Some(host) = url.host_str() else {
        return false;
    };
    let literal = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6.parse::<Ipv6Addr>().ok().map(IpAddr::V6),
        None => host.parse::<Ipv4Addr>().ok().map(IpAddr::V4),
    };
    literal.is_some_and(is_forbidden_destination)
}

/// Resolves host names for the checks' HTTP client as reqwest itself would
/// (the system resolver, off the async threads) and drops every address a
/// check may not contact, failing with [`DestinationNotAllowed`] when none is
/// left. Every connection the client opens resolves its host here, the target
/// of each redirect included.
struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let allowed = allowed_addresses(resolved)?;
            Ok(Box::new(allowed.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// Cleans an outcome's error once, where the outcome is built: cut to
/// [`MAX_ERROR_CHARS`] (see [`truncate_error`]), and every char that could
/// break the text out of its one line or reorder how it reads (see
/// [`is_unsafe_in_text`]) replaced by a space. The text is stored and shown
/// verbatim (ClickHouse, incident causes, alert emails, the UI), and part of
/// it comes from the target: a TLS error quotes its certificate, and later
/// check types quote server banners.
fn clean_error(error: String) -> String {
    truncate_error(error)
        .chars()
        .map(|c| if is_unsafe_in_text(c) { ' ' } else { c })
        .collect()
}

/// Control characters (NUL, CR, LF, ESC, DEL, the C1 range...), the line and
/// paragraph separators (U+2028, U+2029) and the bidirectional controls (the
/// Arabic letter mark U+061C, the marks U+200E/U+200F, the embeddings and
/// overrides U+202A..=U+202E, the isolates U+2066..=U+2069).
pub fn is_unsafe_in_text(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}'
                | '\u{2029}'
                | '\u{061C}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Up,
    Down,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckOutcome {
    pub status: CheckStatus,
    pub status_code: Option<u16>,
    pub response_time_ms: Option<u32>,
    /// Why the check is down, `None` when it is up. At most `MAX_ERROR_CHARS`
    /// chars, plus a trailing ellipsis when it was cut, on one line and with no
    /// direction controls (see `is_unsafe_in_text`). Part of the text can come
    /// from the target, so treat it as untrusted wherever it is shown.
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// The client used for checks. Redirects are followed (reqwest's default
/// policy, up to 10 hops). Idle connections are never kept, so every check
/// opens a fresh connection: a pooled keep-alive socket would hide a server
/// that stopped accepting new connections, and would leave connection setup
/// out of the measured response time.
///
/// Proxy settings from the environment (`HTTP_PROXY`, `HTTPS_PROXY`,
/// `ALL_PROXY`) or the operating system are ignored: a check must reach the
/// monitored address itself, not whatever a proxy in the engine's environment
/// makes of it, and tcp checks never use a proxy.
///
/// No connection goes to an address [`is_forbidden_destination`] refuses:
/// names resolve through [`GuardedResolver`], and a redirect to an IP literal
/// is refused by the redirect policy (the monitor's own URL is checked in
/// [`perform_check`]).
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(GuardedResolver)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if names_forbidden_address(attempt.url()) {
                attempt.error(DestinationNotAllowed)
            } else {
                reqwest::redirect::Policy::default().redirect(attempt)
            }
        }))
        .pool_max_idle_per_host(0)
        .user_agent(concat!("Nami-Uptime/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build the uptime HTTP client")
}

/// Runs the monitor's check within its timeout. Response time is measured
/// to the response headers for `http` checks, to the end of the (capped)
/// body for `keyword` checks, and to connection setup for `tcp` checks.
///
/// Keyword checks always send GET, whatever `http_method` says, because a HEAD
/// response has no body. Matching is case-sensitive, on the body decoded as
/// UTF-8 with invalid bytes replaced, whatever charset the response declares:
/// a keyword with non-ASCII characters never matches a page served in another
/// encoding such as Latin-1.
///
/// A destination no check may contact (see [`is_forbidden_destination`]) is
/// refused before any connection, with the generic [`DESTINATION_NOT_ALLOWED`].
///
/// The outcome's `error` is capped at `MAX_ERROR_CHARS` chars and is one line
/// of text (see [`clean_error`]).
pub async fn perform_check(http: &reqwest::Client, monitor: &Monitor) -> CheckOutcome {
    let checked_at = Utc::now();
    let timeout_secs = u64::try_from(monitor.timeout_seconds.clamp(1, 60)).unwrap_or(10);
    let timeout = Duration::from_secs(timeout_secs);
    let started = Instant::now();
    let probe = async {
        match monitor.check_type.as_str() {
            "http" | "keyword" => probe_http(http, monitor, started).await,
            "tcp" => probe_tcp(monitor, started).await,
            other => Observation::down(None, None, format!("unsupported check type {other:?}")),
        }
    };
    // The only deadline, around everything (DNS, connect, TLS, headers,
    // body). The requests carry no timeout of their own: a second one with
    // the same duration would race this one, and which of the two fired
    // would decide whether the outcome kept the status code.
    let observation = tokio::time::timeout(timeout, probe)
        .await
        .unwrap_or_else(|_| {
            Observation::down(None, None, format!("timeout after {timeout_secs}s"))
        });
    CheckOutcome {
        status: if observation.error.is_none() {
            CheckStatus::Up
        } else {
            CheckStatus::Down
        },
        status_code: observation.status_code,
        response_time_ms: observation.response_time_ms,
        // The one place every error text passes through.
        error: observation.error.map(clean_error),
        checked_at,
    }
}

struct Observation {
    status_code: Option<u16>,
    response_time_ms: Option<u32>,
    error: Option<String>,
}

impl Observation {
    fn up(status_code: Option<u16>, started: Instant) -> Self {
        Self {
            status_code,
            response_time_ms: Some(elapsed_ms(started)),
            error: None,
        }
    }

    fn down(status_code: Option<u16>, response_time_ms: Option<u32>, error: String) -> Self {
        Self {
            status_code,
            response_time_ms,
            error: Some(error),
        }
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

async fn probe_http(http: &reqwest::Client, monitor: &Monitor, started: Instant) -> Observation {
    let Some(url) = monitor.url.as_deref() else {
        return Observation::down(None, None, "monitor has no url".to_string());
    };
    // `Some` for keyword checks only. A keyword monitor without a keyword is
    // refused before any request is made: an empty keyword is found in every
    // body, so the page would stop mattering and a `contains` monitor would
    // always report up.
    let keyword = if monitor.check_type == "keyword" {
        let Some(keyword) = monitor.keyword.as_deref().filter(|k| !k.is_empty()) else {
            return Observation::down(None, None, "monitor has no keyword".to_string());
        };
        Some(keyword)
    } else {
        None
    };
    // A keyword check needs the body, and a HEAD response has none.
    let method = if keyword.is_none() && monitor.http_method == "HEAD" {
        reqwest::Method::HEAD
    } else {
        reqwest::Method::GET
    };
    // The URL's own host, if it is an IP literal: a name is judged when it
    // resolves (see `GuardedResolver`), a redirect by the redirect policy.
    if reqwest::Url::parse(url).is_ok_and(|parsed| names_forbidden_address(&parsed)) {
        return Observation::down(None, None, DESTINATION_NOT_ALLOWED.to_string());
    }
    let mut response = match http.request(method, url).send().await {
        Ok(response) => response,
        Err(e) => return Observation::down(None, None, describe_request_error(&e)),
    };
    let code = response.status().as_u16();
    let in_range =
        (monitor.expected_status_min..=monitor.expected_status_max).contains(&i32::from(code));
    if !in_range {
        return Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!(
                "expected status {}-{}, got {code}",
                monitor.expected_status_min, monitor.expected_status_max
            ),
        );
    }
    let Some(keyword) = keyword else {
        return Observation::up(Some(code), started);
    };

    let body = match read_capped(&mut response, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(e) => return Observation::down(Some(code), None, describe_request_error(&e)),
    };
    let found = String::from_utf8_lossy(&body).contains(keyword);
    match (monitor.keyword_mode.as_str(), found) {
        ("not_contains", true) => Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" found"),
        ),
        ("not_contains", false) | (_, true) => Observation::up(Some(code), started),
        (_, false) => Observation::down(
            Some(code),
            Some(elapsed_ms(started)),
            format!("keyword \"{keyword}\" not found"),
        ),
    }
}

async fn read_capped(
    response: &mut reqwest::Response,
    cap: usize,
) -> Result<Vec<u8>, reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

async fn probe_tcp(monitor: &Monitor, started: Instant) -> Observation {
    let (Some(host), Some(port)) = (monitor.host.as_deref(), monitor.port) else {
        return Observation::down(None, None, "monitor has no host/port".to_string());
    };
    let Ok(port) = u16::try_from(port) else {
        return Observation::down(None, None, format!("invalid port {port}"));
    };
    // A (host, port) tuple, not a "host:port" string, so IPv6 literals work.
    let resolved = match tokio::net::lookup_host((host, port)).await {
        Ok(resolved) => resolved,
        Err(e) => return Observation::down(None, None, describe_io_error(&e)),
    };
    let Ok(allowed) = allowed_addresses(resolved) else {
        return Observation::down(None, None, DESTINATION_NOT_ALLOWED.to_string());
    };
    // Tried in turn, like a connect to the name itself.
    match tokio::net::TcpStream::connect(&allowed[..]).await {
        Ok(_stream) => Observation::up(None, started),
        Err(e) => Observation::down(None, None, describe_io_error(&e)),
    }
}

fn describe_request_error(error: &reqwest::Error) -> String {
    if caused_by::<DestinationNotAllowed>(error) {
        return DESTINATION_NOT_ALLOWED.to_string();
    }
    if error.is_redirect() {
        return "too many redirects".to_string();
    }
    if let Some(message) = io_error_kind(error).and_then(io_kind_message) {
        return message.to_string();
    }
    if error.is_connect() {
        format!("connection failed: {}", root_cause(error))
    } else {
        format!("request failed: {}", root_cause(error))
    }
}

fn describe_io_error(error: &std::io::Error) -> String {
    match io_kind_message(error.kind()) {
        Some(message) => message.to_string(),
        None => format!("connection failed: {error}"),
    }
}

/// The short message for the two I/O failures that get a name of their own;
/// every other kind is reported with its underlying error text instead.
fn io_kind_message(kind: std::io::ErrorKind) -> Option<&'static str> {
    match kind {
        std::io::ErrorKind::ConnectionRefused => Some("connection refused"),
        std::io::ErrorKind::ConnectionReset => Some("connection reset"),
        _ => None,
    }
}

/// reqwest wraps the OS error several layers deep (reqwest -> hyper-util ->
/// io::Error); walk the `source()` chain to find it.
fn io_error_kind(error: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(io) = err.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        current = err.source();
    }
    None
}

/// Whether an `E` is anywhere in `error`'s `source()` chain (reqwest wraps a
/// resolver's or a redirect policy's error several layers deep).
fn caused_by<E: std::error::Error + 'static>(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(err) = current {
        if err.is::<E>() {
            return true;
        }
        current = err.source();
    }
    false
}

/// The innermost error's message, e.g. "failed to lookup address
/// information: ..." instead of reqwest's generic "error sending request".
fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

/// Cuts `error` to `MAX_ERROR_CHARS` chars, always on a char boundary, and
/// marks the cut with an ellipsis. Shorter text is returned as it is. Only the
/// first `MAX_ERROR_CHARS + 1` chars are examined, so a huge message costs no
/// more to cap than a short one.
fn truncate_error(error: String) -> String {
    match error.char_indices().nth(MAX_ERROR_CHARS) {
        Some((cut, _)) => format!("{}…", &error[..cut]),
        None => error,
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn a_short_error_is_unchanged() {
        assert_eq!(
            truncate_error("connection refused".to_string()),
            "connection refused"
        );
        assert_eq!(truncate_error(String::new()), "");
    }

    #[test]
    fn an_error_of_exactly_the_cap_is_unchanged() {
        let exact = "a".repeat(MAX_ERROR_CHARS);
        assert_eq!(truncate_error(exact.clone()), exact);
    }

    #[test]
    fn a_long_ascii_error_is_cut_to_the_cap_plus_an_ellipsis() {
        let one_over = truncate_error("a".repeat(MAX_ERROR_CHARS + 1));
        assert_eq!(one_over, format!("{}…", "a".repeat(MAX_ERROR_CHARS)));

        let huge = truncate_error("b".repeat(1_000_000));
        assert_eq!(huge.chars().count(), MAX_ERROR_CHARS + 1);
        assert!(huge.starts_with(&"b".repeat(MAX_ERROR_CHARS)));
        assert!(huge.ends_with('…'));
    }

    #[test]
    fn the_cut_never_splits_a_multibyte_char() {
        // Byte 256 is inside a two-byte char here, so slicing at the byte
        // offset would panic instead of cutting.
        let two_byte = format!("a{}", "é".repeat(MAX_ERROR_CHARS));
        assert!(!two_byte.is_char_boundary(MAX_ERROR_CHARS));
        assert_eq!(
            truncate_error(two_byte),
            format!("a{}…", "é".repeat(MAX_ERROR_CHARS - 1))
        );

        // The same with four-byte chars.
        let four_byte = format!("ab{}", "🦀".repeat(MAX_ERROR_CHARS));
        assert!(!four_byte.is_char_boundary(MAX_ERROR_CHARS));
        assert_eq!(
            truncate_error(four_byte),
            format!("ab{}…", "🦀".repeat(MAX_ERROR_CHARS - 2))
        );
    }

    #[test]
    fn the_cap_counts_chars_not_bytes() {
        // 256 chars, 512 bytes: kept whole.
        let wide = "é".repeat(MAX_ERROR_CHARS);
        assert_eq!(truncate_error(wide.clone()), wide);
    }

    #[test]
    fn only_refused_and_reset_get_a_message_of_their_own() {
        assert_eq!(
            io_kind_message(ErrorKind::ConnectionRefused),
            Some("connection refused")
        );
        assert_eq!(
            io_kind_message(ErrorKind::ConnectionReset),
            Some("connection reset")
        );
        assert_eq!(io_kind_message(ErrorKind::TimedOut), None);
        assert_eq!(io_kind_message(ErrorKind::NotFound), None);
    }

    #[test]
    fn control_characters_separators_and_direction_controls_become_spaces() {
        let unsafe_chars: Vec<char> = ('\u{0}'..='\u{1f}')
            .chain('\u{7f}'..='\u{9f}')
            .chain(['\u{2028}', '\u{2029}', '\u{061C}', '\u{200E}', '\u{200F}'])
            .chain('\u{202A}'..='\u{202E}')
            .chain('\u{2066}'..='\u{2069}')
            .collect();
        for c in &unsafe_chars {
            assert!(is_unsafe_in_text(*c), "U+{:04X}", *c as u32);
        }
        let dirty: String = unsafe_chars.iter().collect();
        assert_eq!(
            clean_error(format!("a{dirty}b")),
            format!("a{}b", " ".repeat(unsafe_chars.len()))
        );
        assert_eq!(
            clean_error("x\0y\r\nz\u{1b}[31m".to_string()),
            "x y  z [31m"
        );
    }

    #[test]
    fn the_neighbours_of_the_replaced_chars_are_kept() {
        let kept = "\u{2027}\u{202F}\u{2065}\u{206A}\u{200D}\u{200C}\u{200B}\u{061B}\u{061D}\u{200D}\u{20AC} é 日本 🦀";
        for c in kept.chars() {
            assert!(!is_unsafe_in_text(c), "U+{:04X}", c as u32);
        }
        assert_eq!(clean_error(kept.to_string()), kept);
    }

    #[test]
    fn cleaning_keeps_the_cap() {
        let cleaned = clean_error("\n".repeat(10_000));
        assert_eq!(cleaned, format!("{}…", " ".repeat(MAX_ERROR_CHARS)));
    }

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn link_local_metadata_and_this_network_are_forbidden() {
        for text in [
            "169.254.169.254",
            "169.254.0.0",
            "169.254.255.255",
            "0.0.0.0",
            "0.255.255.255",
            "fe80::1",
            "febf:ffff::1",
            "fd00:ec2::254",
            "::",
            "::ffff:169.254.169.254",
            "::ffff:0.0.0.0",
            "::ffff:0.1.2.3",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::1",
        ] {
            assert!(is_forbidden_destination(ip(text)), "{text}");
        }
    }

    #[test]
    fn everything_else_is_allowed_loopback_and_private_networks_included() {
        for text in [
            "169.253.255.255",
            "169.255.0.0",
            "1.0.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.1.1",
            "100.100.100.200",
            "8.8.8.8",
            "255.255.255.255",
            "::1",
            "fe7f:ffff::1",
            "fec0::1",
            "fd00:ec2::253",
            "fd00:ec2::255",
            "fc00::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9c::a9fe:a9fe",
        ] {
            assert!(!is_forbidden_destination(ip(text)), "{text}");
        }
    }

    #[test]
    fn only_the_allowed_addresses_of_a_resolution_are_kept() {
        let address = |text: &str| SocketAddr::new(ip(text), 80);
        assert_eq!(
            allowed_addresses([
                address("169.254.169.254"),
                address("10.0.0.1"),
                address("fe80::1")
            ]),
            Ok(vec![address("10.0.0.1")])
        );
        assert_eq!(
            allowed_addresses([address("169.254.169.254"), address("::ffff:0.0.0.0")]),
            Err(DestinationNotAllowed)
        );
        assert_eq!(allowed_addresses([]), Ok(vec![]));
    }

    #[test]
    fn a_url_naming_a_forbidden_address_is_recognised_in_every_spelling() {
        for url in [
            "http://169.254.169.254/",
            "http://0xa9fea9fe/",
            "http://2852039166/",
            "http://0251.0376.0251.0376/",
            "https://169.254.169.254:8443/x?y#z",
            "http://[fe80::1]/",
            "http://[::ffff:a9fe:a9fe]/",
            "http://0/",
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert!(names_forbidden_address(&parsed), "{url}");
        }
        for url in [
            "http://127.0.0.1/",
            "http://localhost/",
            "http://169.254.169.254.example.com/",
            "http://[::1]/",
            "http://example.test/",
        ] {
            let parsed = reqwest::Url::parse(url).unwrap();
            assert!(!names_forbidden_address(&parsed), "{url}");
        }
    }

    #[test]
    fn other_io_errors_keep_their_text() {
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(describe_io_error(&refused), "connection refused");
        let other = std::io::Error::other("boom");
        assert_eq!(describe_io_error(&other), "connection failed: boom");
    }
}
