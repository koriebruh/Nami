//! Performs one check of one monitor. Never fails: every problem (timeout,
//! refused connection, wrong status, missing keyword) becomes a `Down`
//! outcome with a short human-readable `error`.

use std::time::{Duration, Instant};

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
    /// chars, plus a trailing ellipsis when it was cut. Part of the text can
    /// come from the target, so treat it as untrusted wherever it is shown.
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
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
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
/// The outcome's `error` is capped at `MAX_ERROR_CHARS` chars.
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
        error: observation.error.map(truncate_error),
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
    match tokio::net::TcpStream::connect((host, port)).await {
        Ok(_stream) => Observation::up(None, started),
        Err(e) => Observation::down(None, None, describe_io_error(&e)),
    }
}

fn describe_request_error(error: &reqwest::Error) -> String {
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
    fn other_io_errors_keep_their_text() {
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(describe_io_error(&refused), "connection refused");
        let other = std::io::Error::other("boom");
        assert_eq!(describe_io_error(&other), "connection failed: boom");
    }
}
