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
    pub error: Option<String>,
    pub checked_at: DateTime<Utc>,
}

/// The client used for checks. Redirects are followed (reqwest's default
/// policy, up to 10 hops). Idle connections are never kept, so every check
/// opens a fresh connection: a pooled keep-alive socket would hide a server
/// that stopped accepting new connections, and would leave connection setup
/// out of the measured response time.
pub fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .pool_max_idle_per_host(0)
        .user_agent(concat!("Nami-Uptime/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build the uptime HTTP client")
}

/// Runs the monitor's check within its timeout. Response time is measured
/// to the response headers for `http` checks, to the end of the (capped)
/// body for `keyword` checks, and to connection setup for `tcp` checks.
/// Keyword matching is case-sensitive.
pub async fn perform_check(http: &reqwest::Client, monitor: &Monitor) -> CheckOutcome {
    let checked_at = Utc::now();
    let timeout_secs = u64::try_from(monitor.timeout_seconds.clamp(1, 60)).unwrap_or(10);
    let timeout = Duration::from_secs(timeout_secs);
    let started = Instant::now();
    let probe = async {
        match monitor.check_type.as_str() {
            "http" | "keyword" => probe_http(http, monitor, timeout, started).await,
            "tcp" => probe_tcp(monitor, started).await,
            other => Observation::down(None, None, format!("unsupported check type {other:?}")),
        }
    };
    // One deadline around everything (DNS, connect, TLS, headers, body).
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
        error: observation.error,
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

async fn probe_http(
    http: &reqwest::Client,
    monitor: &Monitor,
    timeout: Duration,
    started: Instant,
) -> Observation {
    let Some(url) = monitor.url.as_deref() else {
        return Observation::down(None, None, "monitor has no url".to_string());
    };
    let method = if monitor.http_method == "HEAD" {
        reqwest::Method::HEAD
    } else {
        reqwest::Method::GET
    };
    let mut response = match http.request(method, url).timeout(timeout).send().await {
        Ok(response) => response,
        Err(e) => return Observation::down(None, None, describe_request_error(&e, timeout)),
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
    if monitor.check_type != "keyword" {
        return Observation::up(Some(code), started);
    }

    let body = match read_capped(&mut response, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(e) => return Observation::down(Some(code), None, describe_request_error(&e, timeout)),
    };
    let keyword = monitor.keyword.as_deref().unwrap_or_default();
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

fn describe_request_error(error: &reqwest::Error, timeout: Duration) -> String {
    if error.is_timeout() {
        return format!("timeout after {}s", timeout.as_secs());
    }
    if error.is_redirect() {
        return "too many redirects".to_string();
    }
    match io_error_kind(error) {
        Some(std::io::ErrorKind::ConnectionRefused) => "connection refused".to_string(),
        Some(std::io::ErrorKind::ConnectionReset) => "connection reset".to_string(),
        _ if error.is_connect() => format!("connection failed: {}", root_cause(error)),
        _ => format!("request failed: {}", root_cause(error)),
    }
}

fn describe_io_error(error: &std::io::Error) -> String {
    match error.kind() {
        std::io::ErrorKind::ConnectionRefused => "connection refused".to_string(),
        std::io::ErrorKind::ConnectionReset => "connection reset".to_string(),
        _ => format!("connection failed: {error}"),
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
