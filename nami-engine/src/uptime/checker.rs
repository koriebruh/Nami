//! Performs one check of one monitor. Never fails: every problem (timeout,
//! refused connection, wrong status, missing keyword, a destination no check
//! may contact, secrets that cannot be decrypted) becomes a `Down` outcome
//! with a short human-readable `error`.

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::{
    crypto::{Cipher, SecretString},
    models::{
        monitor::{Monitor, url_has_credentials},
        monitor_request::{
            BODY_METHODS, HTTP_METHODS, RESERVED_HEADERS, open_headers, open_password,
        },
    },
    state::AppState,
};

/// Keyword checks read at most this much of the body; a keyword that only
/// appears after the first MiB counts as not found.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Redirect hops a check follows before giving up (`follow_redirects`).
pub const MAX_REDIRECTS: usize = 10;

/// The down reason when a monitor's stored secrets cannot be decrypted. It
/// ends up in check history, incidents and alert emails, so it is deliberately
/// generic: it says what happened, never what the secrets were.
pub const CONFIG_DECRYPT_ERROR: &str =
    "request configuration could not be decrypted (was NAMI_ENCRYPTION_KEY changed?)";

/// The down reason when decrypted headers are not valid HTTP, or are ones the
/// client owns (see `RESERVED_HEADERS`). Validation keeps such headers from
/// being stored, so this is a safety net; like the one above it never repeats a
/// header name or value.
const INVALID_HEADERS_ERROR: &str = "invalid request header configuration";

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
pub(crate) fn allowed_addresses(
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

/// What every check of one worker pass shares: the HTTP clients and the cipher
/// that opens the monitors' secrets. Cloning is cheap: every clone shares the
/// same clients (including the ones built later, see [`HttpClients`]) and key
/// material.
///
/// Meant to be built once per pass and dropped with it, not kept for the life
/// of the worker: clients that live across passes resume the TLS sessions of
/// earlier checks, which skips verifying the certificate the server presents
/// now and leaves most of the handshake out of the measured response time.
/// Building a context is cheap (no client exists until a check needs it), and
/// one built per pass never outlives the runtime that drives its connections.
#[derive(Clone)]
pub struct CheckContext {
    pub http: HttpClients,
    pub cipher: Cipher,
}

impl CheckContext {
    pub fn new(cipher: Cipher) -> CheckContext {
        CheckContext {
            http: HttpClients::new(),
            cipher,
        }
    }

    pub fn for_state(state: &AppState) -> CheckContext {
        CheckContext::new(state.cipher.clone())
    }
}

/// How a client treats a redirect.
#[derive(Clone, Copy)]
enum RedirectMode {
    /// No redirect is followed: the 3xx response is the result.
    None,
    /// Up to [`MAX_REDIRECTS`], anywhere.
    Follow,
    /// Up to [`MAX_REDIRECTS`], within the monitored origin (scheme, host and
    /// port) only: for requests that carry credentials of any kind.
    SameOrigin,
}

/// The clients [`HttpClients`] keeps: every [`RedirectMode`] with and without
/// certificate verification.
const CLIENT_COUNT: usize = 3 * 2;

/// One reqwest client per combination of the per-monitor transport settings:
/// how redirects are handled (see [`RedirectMode`]) and whether certificates
/// are verified. All of them come out of [`build_client`], so every one is
/// built the same way, and hardened the same way:
///
/// - Idle connections are never kept, so every check opens a fresh connection:
///   a pooled keep-alive socket would hide a server that stopped accepting new
///   connections, and would leave connection setup out of the measured
///   response time.
/// - Proxy settings from the environment (`HTTP_PROXY`, `HTTPS_PROXY`,
///   `ALL_PROXY`) or the operating system are ignored: a check must reach the
///   monitored address itself, not whatever a proxy in the engine's
///   environment makes of it, and tcp checks never use a proxy.
/// - No connection goes to an address [`is_forbidden_destination`] refuses:
///   names resolve through [`GuardedResolver`], and a redirect to an IP
///   literal is refused by the redirect policy (the monitor's own URL is
///   checked in [`probe_http`]).
/// - No `Referer` is sent when a redirect is followed: the monitored URL can
///   carry a token in its query, and the redirect may lead to another origin.
///
/// A client is built when a check first needs it, not up front: building one
/// can mean parsing the system's root certificates, and a pass rarely uses more
/// than one or two of the six. Clones share what has been built.
#[derive(Clone)]
pub struct HttpClients {
    /// Indexed by `mode * 2 + (verify_ssl ? 0 : 1)`, `mode` being the
    /// [`RedirectMode`]'s position in its declaration.
    clients: Arc<[OnceLock<reqwest::Client>; CLIENT_COUNT]>,
}

impl Default for HttpClients {
    fn default() -> Self {
        HttpClients::new()
    }
}

impl HttpClients {
    pub fn new() -> HttpClients {
        HttpClients {
            clients: Arc::new(std::array::from_fn(|_| OnceLock::new())),
        }
    }

    /// The client for one request, built if no check has needed it yet.
    ///
    /// A request that carries credentials of any kind (custom headers, basic
    /// auth, credentials in the URL) only follows redirects that stay on the
    /// monitored origin. reqwest drops `Authorization`, `Cookie`,
    /// `Proxy-Authorization` and `WWW-Authenticate` only on the hop that
    /// crosses to another host, from that hop's copy of the request headers;
    /// the next hop starts again from the original ones, so a chain that moves
    /// to another host and carries on there gets them back. Every other header
    /// (an API key, say) is forwarded to wherever the target points.
    ///
    /// # Panics
    ///
    /// If the client cannot be built (no TLS roots on the system, say).
    pub fn select(
        &self,
        follow_redirects: bool,
        verify_ssl: bool,
        carries_secrets: bool,
    ) -> &reqwest::Client {
        let mode = match (follow_redirects, carries_secrets) {
            (false, _) => RedirectMode::None,
            (true, false) => RedirectMode::Follow,
            (true, true) => RedirectMode::SameOrigin,
        };
        self.clients[mode as usize * 2 + usize::from(!verify_ssl)]
            .get_or_init(|| build_client(mode, verify_ssl))
    }

    /// How many clients exist so far.
    #[cfg(test)]
    fn built(&self) -> usize {
        self.clients
            .iter()
            .filter(|slot| slot.get().is_some())
            .count()
    }
}

/// The one place a check's HTTP client is built (see [`HttpClients`]).
fn build_client(redirects: RedirectMode, verify_ssl: bool) -> reqwest::Client {
    let policy = match redirects {
        RedirectMode::None => reqwest::redirect::Policy::none(),
        RedirectMode::Follow => follow_policy(false),
        RedirectMode::SameOrigin => follow_policy(true),
    };
    reqwest::Client::builder()
        .no_proxy()
        .dns_resolver(GuardedResolver)
        .redirect(policy)
        .referer(false)
        .danger_accept_invalid_certs(!verify_ssl)
        .pool_max_idle_per_host(0)
        .user_agent(concat!("Nami-Uptime/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("failed to build an uptime HTTP client")
}

/// Follows up to [`MAX_REDIRECTS`] redirects, refusing every hop to an address
/// no check may contact (see [`is_forbidden_destination`]), and, with
/// `same_origin_only`, every hop that leaves the monitored origin.
fn follow_policy(same_origin_only: bool) -> reqwest::redirect::Policy {
    let limit = reqwest::redirect::Policy::limited(MAX_REDIRECTS);
    reqwest::redirect::Policy::custom(move |attempt| {
        if names_forbidden_address(attempt.url()) {
            return attempt.error(DestinationNotAllowed);
        }
        let leaves = same_origin_only
            .then(|| leaves_origin(attempt.previous(), attempt.url()))
            .flatten();
        match leaves {
            Some(refused) => attempt.error(refused),
            None => limit.redirect(attempt),
        }
    })
}

/// Why a redirect was refused: it leaves the monitored origin, and the request
/// carries credentials (see [`HttpClients::select`]). Names where it points, as
/// an origin or just a host, never a path, query or userinfo. The reason comes
/// first and the host is shortened (see [`named_host`]), so that the whole text
/// fits `MAX_ERROR_CHARS` and its explanation is never what gets cut.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CrossOriginRedirect {
    target: String,
}

impl fmt::Display for CrossOriginRedirect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "headers and credentials are only sent to the monitored origin, so the redirect to {} was not followed",
            self.target
        )
    }
}

impl std::error::Error for CrossOriginRedirect {}

/// The most chars of a host name a refusal quotes; the rest is replaced by an
/// ellipsis. A host name can be 253 chars long.
const NAMED_HOST_CHARS: usize = 100;

fn named_host(host: &str) -> String {
    match host.char_indices().nth(NAMED_HOST_CHARS) {
        Some((cut, _)) => format!("{}…", &host[..cut]),
        None => host.to_string(),
    }
}

/// `scheme://host`, with `:port` unless it is the scheme's default: the origin
/// as `url::Origin::ascii_serialization` writes it, with the host shortened.
fn named_origin(url: &reqwest::Url) -> String {
    let host = named_host(url.host_str().unwrap_or("unknown"));
    match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    }
}

/// Whether the redirect to `next` leaves the origin of the monitored URL, the
/// first of the URLs `previous` requested so far. Origins are compared as
/// browsers do: scheme, host and port, where a port left out is the scheme's
/// default one. When there is no monitored URL to compare with, nothing is
/// known to stay, so the redirect is refused.
fn leaves_origin(previous: &[reqwest::Url], next: &reqwest::Url) -> Option<CrossOriginRedirect> {
    let original = previous.first();
    if original.is_some_and(|original| original.origin() == next.origin()) {
        return None;
    }
    let same_host = original.is_some_and(|original| original.host_str() == next.host_str());
    let target = if same_host {
        format!("another origin ({})", named_origin(next))
    } else {
        format!(
            "another host ({})",
            named_host(next.host_str().unwrap_or("unknown"))
        )
    };
    Some(CrossOriginRedirect { target })
}

/// Runs the monitor's check within its timeout. Response time is measured
/// to the response headers for `http` checks, to the end of the (capped)
/// body for `keyword` checks, and to connection setup for `tcp` checks. For
/// `http` and `keyword` checks it starts when the request is ready to be sent:
/// building the HTTP client the first time a pass needs it, and opening the
/// monitor's secrets, are not part of it.
///
/// `http` and `keyword` checks send the monitor's request configuration: its
/// method, body (for POST, PUT and PATCH), custom headers and basic auth, on
/// the client matching its redirect and TLS settings (see [`HttpClients`]). The
/// secrets are opened here, for this one check; if they cannot be, the check is
/// down with [`CONFIG_DECRYPT_ERROR`] and nothing is sent. A keyword check
/// needs the body, and a HEAD response has none, so a stored HEAD goes out as
/// GET. Matching is case-sensitive, on the body decoded as UTF-8 with invalid
/// bytes replaced, whatever charset the response declares: a keyword with
/// non-ASCII characters never matches a page served in another encoding such
/// as Latin-1.
///
/// A destination no check may contact (see [`is_forbidden_destination`]) is
/// refused before any connection, with the generic [`DESTINATION_NOT_ALLOWED`].
///
/// The outcome's `error` is capped at `MAX_ERROR_CHARS` chars and is one line
/// of text (see [`clean_error`]).
pub async fn perform_check(ctx: &CheckContext, monitor: &Monitor) -> CheckOutcome {
    let checked_at = Utc::now();
    let timeout_secs = u64::try_from(monitor.timeout_seconds.clamp(1, 60)).unwrap_or(10);
    let timeout = Duration::from_secs(timeout_secs);
    let started = Instant::now();
    let probe = async {
        match monitor.check_type.as_str() {
            "http" | "keyword" => probe_http(ctx, monitor).await,
            "tcp" => probe_tcp(monitor, started).await,
            "ping" => super::ping::probe(monitor, timeout).await,
            "dns" => super::dns::probe(monitor, timeout, started).await,
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

/// What one probe saw; `perform_check` turns it into a [`CheckOutcome`] (up
/// exactly when `error` is `None`).
pub(crate) struct Observation {
    pub(crate) status_code: Option<u16>,
    pub(crate) response_time_ms: Option<u32>,
    pub(crate) error: Option<String>,
}

impl Observation {
    pub(crate) fn up(status_code: Option<u16>, started: Instant) -> Self {
        Self {
            status_code,
            response_time_ms: Some(elapsed_ms(started)),
            error: None,
        }
    }

    /// Up with a response time the probe measured itself (ping's round trip).
    pub(crate) fn up_after(response_time: Duration) -> Self {
        Self {
            status_code: None,
            response_time_ms: Some(u32::try_from(response_time.as_millis()).unwrap_or(u32::MAX)),
            error: None,
        }
    }

    pub(crate) fn down(
        status_code: Option<u16>,
        response_time_ms: Option<u32>,
        error: String,
    ) -> Self {
        Self {
            status_code,
            response_time_ms,
            error: Some(error),
        }
    }
}

pub(crate) fn elapsed_ms(started: Instant) -> u32 {
    u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX)
}

/// Text from the monitored service (DNS answers, banners) trimmed for check
/// history and alert emails: control characters dropped, at most `max_chars`
/// characters kept, a cut marked with an ellipsis. The final cleaning of the
/// whole error (direction controls, the overall cap) is `clean_error`'s.
pub(crate) fn printable(text: &str, max_chars: usize) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() <= max_chars {
        return clean;
    }
    let mut cut: String = clean.chars().take(max_chars).collect();
    cut.push('…');
    cut
}

async fn probe_http(ctx: &CheckContext, monitor: &Monitor) -> Observation {
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
    // The URL's own host, if it is an IP literal: a name is judged when it
    // resolves (see `GuardedResolver`), a redirect by the redirect policy.
    if reqwest::Url::parse(url).is_ok_and(|parsed| names_forbidden_address(&parsed)) {
        return Observation::down(None, None, DESTINATION_NOT_ALLOWED.to_string());
    }
    // Last before sending: the secrets are opened only for a request that
    // will be sent.
    let request = match build_request(ctx, monitor, url, keyword.is_some()) {
        Ok(request) => request,
        Err(message) => return Observation::down(None, None, message),
    };
    // The response time starts here: the request may just have built its
    // client (see `HttpClients`), which has nothing to do with the target.
    let started = Instant::now();
    let mut response = match request.send().await {
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

/// The request one check sends: the monitor's method, custom headers, basic
/// auth and body, on the client matching its redirect and TLS settings.
///
/// The secrets are opened here, for this check alone, and never logged or
/// echoed: every failure becomes one of two fixed messages. The request has no
/// timeout of its own, the deadline in [`perform_check`] covers it.
///
/// `needs_body` is set for keyword checks, which read the response body: a
/// stored HEAD is sent as GET for them, because a HEAD response has none. The
/// request body only goes out with a method that may carry one, whatever is
/// stored.
fn build_request(
    ctx: &CheckContext,
    monitor: &Monitor,
    url: &str,
    needs_body: bool,
) -> Result<reqwest::RequestBuilder, String> {
    let method = request_method(&monitor.http_method, needs_body)?;
    let headers = match &monitor.request_headers_enc {
        Some(sealed) => open_headers(&ctx.cipher, monitor.id, sealed)
            .map_err(|_| CONFIG_DECRYPT_ERROR.to_string())?,
        None => Vec::new(),
    };
    let password = match &monitor.auth_password_enc {
        Some(sealed) => Some(
            open_password(&ctx.cipher, monitor.id, sealed)
                .map_err(|_| CONFIG_DECRYPT_ERROR.to_string())?,
        ),
        None => None,
    };
    let mut header_map = HeaderMap::with_capacity(headers.len());
    for header in &headers {
        // Values go out as the bytes they are: validation accepts any text
        // without control characters, and `HeaderValue::from_str` would refuse
        // every non-ASCII one.
        let (Ok(name), Ok(mut value)) = (
            HeaderName::from_bytes(header.name.as_bytes()),
            HeaderValue::from_bytes(header.value.expose().as_bytes()),
        ) else {
            return Err(INVALID_HEADERS_ERROR.to_string());
        };
        // Framing headers belong to the client. Validation refuses them, so
        // they are only here if a sealed list was not written through the API.
        if RESERVED_HEADERS.contains(&name.as_str()) {
            return Err(INVALID_HEADERS_ERROR.to_string());
        }
        // Keeps the value out of `Debug` output and HTTP/2 header compression.
        value.set_sensitive(true);
        header_map.append(name, value);
    }
    // Credentials of any kind keep the request on its origin (see
    // `HttpClients::select`): custom headers, basic auth, and the ones in the
    // URL itself, which reqwest sends as an `Authorization` header.
    let carries_secrets = !headers.is_empty()
        || monitor.auth_username.is_some()
        || reqwest::Url::parse(url).is_ok_and(|parsed| url_has_credentials(&parsed));
    let client = ctx.http.select(
        monitor.follow_redirects,
        monitor.verify_ssl,
        carries_secrets,
    );
    let sends_body = BODY_METHODS.contains(&method.as_str());
    let mut request = client.request(method, url).headers(header_map);
    if let Some(username) = &monitor.auth_username {
        request = request.basic_auth(username, password.as_ref().map(SecretString::expose));
    }
    match &monitor.request_body {
        Some(body) if sends_body => request = request.body(body.clone()),
        _ => {}
    }
    Ok(request)
}

/// The method a check sends: the stored one, as long as it is one a monitor
/// may have (see `HTTP_METHODS`), and with `needs_body`, GET in place of HEAD.
fn request_method(stored: &str, needs_body: bool) -> Result<reqwest::Method, String> {
    let method = if needs_body && stored == "HEAD" {
        "GET"
    } else {
        stored
    };
    HTTP_METHODS
        .contains(&method)
        .then(|| reqwest::Method::from_bytes(method.as_bytes()).ok())
        .flatten()
        .ok_or_else(|| format!("unsupported http method {method:?}"))
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
        // "too many redirects", or the reason a redirect was refused.
        return root_cause(error);
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
    fn printable_drops_control_characters_and_cuts_with_an_ellipsis() {
        assert_eq!(printable("a\nb\u{0}c\u{1b}d", 10), "abcd");
        assert_eq!(printable("abcdef", 6), "abcdef");
        assert_eq!(printable("abcdefg", 6), "abcdef…");
        assert_eq!(printable(&"é".repeat(5), 3), "ééé…");
        assert_eq!(printable("", 3), "");
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

    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    /// The redirect check as the client's policy asks it: the monitored URL
    /// first, then wherever the redirect points.
    fn leaves(monitored: &str, next: &str) -> Option<String> {
        leaves_origin(&[url(monitored)], &url(next)).map(|refused| refused.target)
    }

    #[test]
    fn a_redirect_that_keeps_the_origin_is_followed() {
        for (monitored, next) in [
            ("http://a.test/x", "http://a.test/y?z=1#f"),
            ("http://a.test/x", "http://a.test:80/y"),
            ("https://a.test/", "https://a.test:443/y"),
            ("https://a.test:8443/", "https://a.test:8443/other"),
            ("HTTP://A.TEST/x", "http://a.test/y"),
            ("http://user:secret@a.test/x", "http://a.test/y"),
            ("http://127.0.0.1:9000/", "http://127.0.0.1:9000/login"),
            ("http://[::1]:9000/", "http://[::1]:9000/login"),
        ] {
            assert_eq!(leaves(monitored, next), None, "{monitored} -> {next}");
        }
    }

    #[test]
    fn a_redirect_to_another_origin_is_refused_and_names_only_that_origin() {
        for (monitored, next, target) in [
            ("http://a.test/", "http://b.test/", "another host (b.test)"),
            (
                "http://a.test/",
                "http://www.a.test/",
                "another host (www.a.test)",
            ),
            (
                "http://a.test/",
                "http://a.test./",
                "another host (a.test.)",
            ),
            (
                "http://127.0.0.1:9000/",
                "http://[::1]:9000/",
                "another host ([::1])",
            ),
            (
                "http://a.test/",
                "https://a.test/",
                "another origin (https://a.test)",
            ),
            (
                "https://a.test/",
                "http://a.test/",
                "another origin (http://a.test)",
            ),
            (
                "http://a.test/",
                "http://a.test:8080/",
                "another origin (http://a.test:8080)",
            ),
            (
                "https://a.test/",
                "https://a.test:8443/",
                "another origin (https://a.test:8443)",
            ),
            // Nothing but the origin is ever named.
            (
                "http://a.test/",
                "http://u:p@b.test:81/path?token=secret#fragment",
                "another host (b.test)",
            ),
        ] {
            assert_eq!(
                leaves(monitored, next).as_deref(),
                Some(target),
                "{monitored} -> {next}"
            );
        }
    }

    #[test]
    fn the_origin_is_the_monitored_url_not_the_hop_before() {
        // Every hop is judged against where the monitor started, so a chain
        // cannot walk away one same-origin step at a time.
        let chain = [url("http://a.test/"), url("http://a.test/one")];
        assert!(leaves_origin(&chain, &url("http://a.test/two")).is_none());
        assert!(leaves_origin(&chain, &url("http://b.test/")).is_some());
    }

    #[test]
    fn with_nothing_to_compare_to_a_redirect_is_refused() {
        let refused = leaves_origin(&[], &url("http://a.test/")).unwrap();
        assert_eq!(refused.target, "another host (a.test)");
    }

    #[test]
    fn the_refusal_reads_as_a_down_reason() {
        let refused = leaves_origin(&[url("http://a.test/")], &url("http://b.test/")).unwrap();
        assert_eq!(
            refused.to_string(),
            "headers and credentials are only sent to the monitored origin, so the redirect to another host (b.test) was not followed"
        );
    }

    /// The error text is cut at `MAX_ERROR_CHARS`, and a host name can be 253
    /// chars: the reason comes first and the host is shortened, so that the
    /// whole sentence always fits.
    #[test]
    fn the_refusal_gives_its_reason_first_and_always_fits_the_error_cap() {
        let long = format!(
            "{}.{}.{}.{}.io",
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(57)
        );
        assert_eq!(long.len(), 252);
        for (monitored, next) in [
            ("http://a.test/".to_string(), format!("http://{long}/")),
            (format!("http://{long}/"), format!("https://{long}:8443/")),
            (format!("http://{long}/"), format!("http://{long}:65535/")),
            (
                "http://a.test/".to_string(),
                "https://[::1]:65535/".to_string(),
            ),
        ] {
            let refused = leaves(&monitored, &next).unwrap();
            let message = CrossOriginRedirect { target: refused }.to_string();
            assert!(
                message
                    .starts_with("headers and credentials are only sent to the monitored origin"),
                "{message}"
            );
            assert!(
                message.chars().count() <= MAX_ERROR_CHARS,
                "{} chars",
                message.chars().count()
            );
            assert_eq!(clean_error(message.clone()), message, "not cut");
            assert!(message.ends_with(" was not followed"), "{message}");
        }
        // A host that fits is named whole, at any length up to the shortening.
        let fits = "h".repeat(100);
        assert_eq!(
            leaves("http://a.test/", &format!("http://{fits}/")).unwrap(),
            format!("another host ({fits})")
        );
        assert_eq!(
            leaves("http://a.test/", &format!("http://{fits}h/")).unwrap(),
            format!("another host ({fits}…)")
        );
    }

    #[test]
    fn every_method_a_monitor_may_have_is_sent_as_stored() {
        for stored in HTTP_METHODS {
            assert_eq!(request_method(stored, false).unwrap().as_str(), stored);
        }
    }

    #[test]
    fn a_keyword_check_sends_get_for_a_stored_head_and_the_stored_method_otherwise() {
        assert_eq!(request_method("HEAD", true).unwrap(), reqwest::Method::GET);
        assert_eq!(
            request_method("HEAD", false).unwrap(),
            reqwest::Method::HEAD
        );
        for stored in HTTP_METHODS.into_iter().filter(|stored| *stored != "HEAD") {
            assert_eq!(request_method(stored, true).unwrap().as_str(), stored);
        }
    }

    #[test]
    fn a_method_that_is_not_one_of_the_seven_is_refused() {
        for stored in [
            "", "get", "Get", "TRACE", "CONNECT", "BREW", "GET ", "G\u{c9}T",
        ] {
            for needs_body in [false, true] {
                assert_eq!(
                    request_method(stored, needs_body),
                    Err(format!("unsupported http method {stored:?}"))
                );
            }
        }
    }

    #[test]
    fn there_is_a_slot_for_every_mode_with_and_without_verification() {
        assert_eq!(RedirectMode::None as usize, 0);
        assert_eq!(RedirectMode::Follow as usize, 1);
        assert_eq!(RedirectMode::SameOrigin as usize * 2 + 1, CLIENT_COUNT - 1);
    }

    /// `(follow_redirects, verify_ssl, carries_secrets)`, one per client.
    const ALL_CLIENTS: [(bool, bool, bool); 6] = [
        (false, true, false),
        (false, false, false),
        (true, true, false),
        (true, false, false),
        (true, true, true),
        (true, false, true),
    ];

    /// Building a client can parse the system's root certificates: only the
    /// ones a pass uses are built, when its first check needs them.
    #[test]
    fn a_client_is_built_when_a_check_first_needs_it() {
        let clients = HttpClients::new();
        assert_eq!(clients.built(), 0);
        clients.select(true, true, false);
        assert_eq!(clients.built(), 1);
        clients.select(true, true, false);
        assert_eq!(clients.built(), 1, "a second check reuses it");
        clients.select(false, false, true);
        assert_eq!(clients.built(), 2);
        for (follow, verify, secrets) in ALL_CLIENTS {
            clients.select(follow, verify, secrets);
        }
        assert_eq!(clients.built(), 6);
    }

    #[test]
    fn every_combination_of_settings_gets_its_own_client_and_keeps_it() {
        let clients = HttpClients::new();
        let chosen: Vec<&reqwest::Client> = ALL_CLIENTS
            .iter()
            .map(|&(follow, verify, secrets)| clients.select(follow, verify, secrets))
            .collect();
        for (position, one) in chosen.iter().enumerate() {
            for other in &chosen[position + 1..] {
                assert!(!std::ptr::eq(*one, *other));
            }
        }
        for (&(follow, verify, secrets), first) in ALL_CLIENTS.iter().zip(&chosen) {
            assert!(std::ptr::eq(
                clients.select(follow, verify, secrets),
                *first
            ));
        }
    }

    #[test]
    fn nothing_is_followed_so_secrets_need_no_client_of_their_own_then() {
        let clients = HttpClients::new();
        for verify in [true, false] {
            assert!(std::ptr::eq(
                clients.select(false, verify, true),
                clients.select(false, verify, false)
            ));
        }
        assert_eq!(clients.built(), 2);
    }

    #[test]
    fn clones_share_the_clients_that_were_built() {
        let clients = HttpClients::new();
        let clone = clients.clone();
        let built_by_the_clone = clone.select(true, false, false);
        assert_eq!(clients.built(), 1);
        assert!(std::ptr::eq(
            clients.select(true, false, false),
            built_by_the_clone
        ));
    }

    #[test]
    fn other_io_errors_keep_their_text() {
        let refused = std::io::Error::from(ErrorKind::ConnectionRefused);
        assert_eq!(describe_io_error(&refused), "connection refused");
        let other = std::io::Error::other("boom");
        assert_eq!(describe_io_error(&other), "connection failed: boom");
    }
}
