use std::{
    fmt,
    net::{IpAddr, Ipv6Addr},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    is_invisible_or_bidi_char, monitor_group,
    monitor_request::{
        self, BODY_METHODS, HTTP_METHODS, HeaderView, HeadersChange, MAX_BODY_BYTES,
        PasswordChange, RequestHeader, seal_headers, seal_password, seal_proxy_password,
    },
    nullable,
    target::{check_host_text, parse_nameserver, validate_host},
    validate_name,
};
use crate::{
    crypto::{Cipher, SecretString},
    error::AppError,
    middleware::rbac::ProjectReader,
    uptime::expiry::{DEFAULT_DOMAIN_THRESHOLD_DAYS, DEFAULT_SSL_THRESHOLD_DAYS},
};

/// Size caps for the free-text target fields. Without them only the 2 MiB
/// request-body limit bounds what is stored, returned to every viewer and
/// put in alert emails.
const MAX_URL_BYTES: usize = 2048;
const MAX_KEYWORD_BYTES: usize = 1024;

/// What `check_type` may be.
pub const CHECK_TYPES: [&str; 11] = [
    "http",
    "keyword",
    "tcp",
    "ping",
    "dns",
    "ssl_expiry",
    "domain_expiry",
    "smtp",
    "pop3",
    "imap",
    "udp",
];

/// What `ip_version` may be.
pub const IP_VERSIONS: [&str; 3] = ["any", "ipv4", "ipv6"];

/// The check types that honour `ip_version`; every other type stores `any`.
pub const IP_VERSION_TYPES: [&str; 5] = ["http", "keyword", "tcp", "ping", "ssl_expiry"];

/// The address family a check connects over. `Any` takes what the resolver
/// returns; `V4` / `V6` drop the other family, with no fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IpVersion {
    #[default]
    Any,
    V4,
    V6,
}

impl IpVersion {
    /// The stored spelling (one of [`IP_VERSIONS`]); anything else is `None`.
    pub fn parse(text: &str) -> Option<IpVersion> {
        match text {
            "any" => Some(IpVersion::Any),
            "ipv4" => Some(IpVersion::V4),
            "ipv6" => Some(IpVersion::V6),
            _ => None,
        }
    }
}

/// The longest `udp_payload`, in bytes.
pub const MAX_UDP_PAYLOAD_BYTES: usize = 1024;

/// The record types a dns monitor can look up (the `dns_record_type` column's
/// check constraint lists the same).
pub const DNS_RECORD_TYPES: [&str; 6] = ["A", "AAAA", "CNAME", "MX", "TXT", "NS"];

/// The longest `expected_value`, in bytes: the cap of a keyword, which it
/// resembles (text matched against what the target says, and quoted in error
/// text).
pub const MAX_EXPECTED_VALUE_BYTES: usize = 1024;

/// One `monitors` row. The `*_enc` columns and `request_header_names` are
/// never serialized: API responses go through [`MonitorView`]. `Debug` leaves
/// the sealed columns out too, and only says whether the request body, the
/// basic-auth username and the udp payload are set. `Default` exists for
/// tests that build monitors in memory (`..Monitor::default()`); real
/// monitors always come from the database.
#[derive(Clone, Default, PartialEq, sqlx::FromRow, Serialize)]
pub struct Monitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
    pub current_status: String,
    pub consecutive_failures: i32,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub next_check_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub request_body: Option<String>,
    /// AES-GCM sealed JSON `[{"name","value"}]`, see
    /// `monitor_request::open_headers`.
    #[serde(skip_serializing)]
    pub request_headers_enc: Option<Vec<u8>>,
    /// The same headers' names in the clear, for display.
    #[serde(skip_serializing)]
    pub request_header_names: Vec<String>,
    pub auth_username: Option<String>,
    /// AES-GCM sealed basic-auth password, see `monitor_request::open_password`.
    #[serde(skip_serializing)]
    pub auth_password_enc: Option<Vec<u8>>,
    pub follow_redirects: bool,
    pub verify_ssl: bool,
    /// Consecutive successful checks a `down` monitor needs before it
    /// recovers (1-10).
    pub recovery_threshold: i32,
    /// The successes counted toward that so far. Only the worker counts them
    /// (see `uptime::worker::apply_outcome`); no API request can set them, a
    /// PATCH that changes what is checked, or resumes the monitor, zeroes
    /// them (and `consecutive_failures`).
    pub consecutive_successes: i32,
    /// dns: the record type to look up, one of [`DNS_RECORD_TYPES`].
    pub dns_record_type: Option<String>,
    /// dns: `ip:port` of the nameserver to ask; `None` is the system resolver.
    pub dns_nameserver: Option<String>,
    /// dns: text that one answer must contain (case-insensitive).
    pub expected_value: Option<String>,
    /// ssl_expiry and domain_expiry: down when the expiry is less than this
    /// many days away (1-365).
    pub expiry_threshold_days: Option<i32>,
    /// smtp, pop3 and imap: speak TLS from the first byte (ports 465, 995,
    /// 993).
    pub use_tls: bool,
    /// udp: the datagram to send, verbatim (UTF-8, no NUL).
    pub udp_payload: Option<String>,
    /// A `monitor_groups` row of the same project, or ungrouped.
    pub group_id: Option<Uuid>,
    /// One of [`IP_VERSIONS`]; see [`Monitor::ip_version`].
    pub ip_version: String,
    /// http/keyword: keep the cookies a response sets for the rest of that
    /// one check (redirects); never kept between checks.
    pub remember_cookies: bool,
    /// http/keyword: `http(s)://host:port` of the proxy every request of the
    /// check goes through.
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    /// AES-GCM sealed proxy password, see `monitor_request::open_proxy_password`.
    #[serde(skip_serializing)]
    pub proxy_password_enc: Option<Vec<u8>>,
}

impl Monitor {
    /// The stored `ip_version`; text the API never writes reads as `Any`.
    pub fn ip_version(&self) -> IpVersion {
        IpVersion::parse(&self.ip_version).unwrap_or_default()
    }
}

/// What `Debug` prints in place of a sealed column that holds a value.
const SEALED: &str = "<sealed>";
/// What `Debug` prints in place of a plaintext column that often carries a
/// credential (request body, basic-auth username, udp payload) and holds a
/// value: logs say whether it is set, not what it is.
const SET: &str = "<set>";

impl fmt::Debug for Monitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Exhaustive, so that a new column cannot be left out by accident.
        let Monitor {
            id,
            project_id,
            name,
            check_type,
            url,
            host,
            port,
            http_method,
            keyword,
            keyword_mode,
            expected_status_min,
            expected_status_max,
            interval_seconds,
            timeout_seconds,
            confirmation_threshold,
            paused,
            current_status,
            consecutive_failures,
            last_checked_at,
            next_check_at,
            created_at,
            request_body,
            request_headers_enc,
            request_header_names,
            auth_username,
            auth_password_enc,
            follow_redirects,
            verify_ssl,
            recovery_threshold,
            consecutive_successes,
            dns_record_type,
            dns_nameserver,
            expected_value,
            expiry_threshold_days,
            use_tls,
            udp_payload,
            group_id,
            ip_version,
            remember_cookies,
            proxy_url,
            proxy_username,
            proxy_password_enc,
        } = self;
        f.debug_struct("Monitor")
            .field("id", id)
            .field("project_id", project_id)
            .field("name", name)
            .field("check_type", check_type)
            .field("url", url)
            .field("host", host)
            .field("port", port)
            .field("http_method", http_method)
            .field("keyword", keyword)
            .field("keyword_mode", keyword_mode)
            .field("expected_status_min", expected_status_min)
            .field("expected_status_max", expected_status_max)
            .field("interval_seconds", interval_seconds)
            .field("timeout_seconds", timeout_seconds)
            .field("confirmation_threshold", confirmation_threshold)
            .field("paused", paused)
            .field("current_status", current_status)
            .field("consecutive_failures", consecutive_failures)
            .field("last_checked_at", last_checked_at)
            .field("next_check_at", next_check_at)
            .field("created_at", created_at)
            .field("request_body", &request_body.as_ref().map(|_| SET))
            .field(
                "request_headers_enc",
                &request_headers_enc.as_ref().map(|_| SEALED),
            )
            .field("request_header_names", request_header_names)
            .field("auth_username", &auth_username.as_ref().map(|_| SET))
            .field(
                "auth_password_enc",
                &auth_password_enc.as_ref().map(|_| SEALED),
            )
            .field("follow_redirects", follow_redirects)
            .field("verify_ssl", verify_ssl)
            .field("recovery_threshold", recovery_threshold)
            .field("consecutive_successes", consecutive_successes)
            .field("dns_record_type", dns_record_type)
            .field("dns_nameserver", dns_nameserver)
            .field("expected_value", expected_value)
            .field("expiry_threshold_days", expiry_threshold_days)
            .field("use_tls", use_tls)
            .field("udp_payload", &udp_payload.as_ref().map(|_| SET))
            .field("group_id", group_id)
            .field("ip_version", ip_version)
            .field("remember_cookies", remember_cookies)
            .field("proxy_url", proxy_url)
            .field("proxy_username", &proxy_username.as_ref().map(|_| SET))
            .field(
                "proxy_password_enc",
                &proxy_password_enc.as_ref().map(|_| SEALED),
            )
            .finish()
    }
}

/// A monitor as every API response shows it: the secrets are reduced to
/// `request_headers` (names, `value_set: true`), `has_auth_password` and
/// `has_proxy_password`,
/// `in_maintenance` says whether a maintenance window covers it right now, and
/// `redacted` whether fields were masked for its reader (see
/// [`Monitor::for_reader`]): a client tells a masked view from a URL that
/// merely contains `***` by this flag, never by the text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorView {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub request_headers: Vec<HeaderView>,
    pub has_auth_password: bool,
    pub has_proxy_password: bool,
    pub in_maintenance: bool,
    pub redacted: bool,
}

impl MonitorView {
    /// The monitor as `reader` may see it (see [`Monitor::for_reader`]).
    pub fn for_reader(
        monitor: Monitor,
        reader: ProjectReader,
        in_maintenance: bool,
    ) -> MonitorView {
        MonitorView {
            redacted: reader == ProjectReader::Viewer,
            ..MonitorView::new(monitor.for_reader(reader), in_maintenance)
        }
    }

    /// The whole monitor, for a member or above.
    pub fn new(monitor: Monitor, in_maintenance: bool) -> MonitorView {
        let request_headers = monitor
            .request_header_names
            .iter()
            .map(|name| HeaderView {
                name: name.clone(),
                value_set: true,
            })
            .collect();
        let has_auth_password = monitor.auth_password_enc.is_some();
        let has_proxy_password = monitor.proxy_password_enc.is_some();
        MonitorView {
            monitor,
            request_headers,
            has_auth_password,
            has_proxy_password,
            in_maintenance,
            redacted: false,
        }
    }
}

/// What replaces each secret part of a URL a viewer reads (see
/// [`redact_url`]), and a request body or username they read.
const REDACTED: &str = "***";

impl Monitor {
    /// The monitor as `reader` may see it: whole for a member or above (header
    /// values and the password are never part of a monitor, see
    /// [`MonitorView`]); for a viewer, with the secrets of its URL masked (see
    /// [`redact_url`]) and its request body, basic-auth username and udp
    /// payload hidden: they carry credentials (tokens, SNMP communities,
    /// RADIUS secrets) as often as a URL does, so a viewer learns that they
    /// are set, not what they are. The proxy username is hidden the same way
    /// and the proxy URL goes through [`redact_url`] (a no-op for every proxy
    /// URL validation accepts; it only matters for a row written around the
    /// API).
    pub fn for_reader(mut self, reader: ProjectReader) -> Self {
        if reader == ProjectReader::Viewer {
            self.url = self.url.as_deref().map(redact_url);
            self.request_body = self.request_body.map(|_| REDACTED.to_string());
            self.auth_username = self.auth_username.map(|_| REDACTED.to_string());
            self.udp_payload = self.udp_payload.map(|_| REDACTED.to_string());
            self.proxy_url = self.proxy_url.map(redact_proxy_url);
            self.proxy_username = self.proxy_username.map(|_| REDACTED.to_string());
        }
        self
    }
}

/// A monitor URL as a viewer reads it: the password of the userinfo, every
/// query-parameter value and the fragment become `***`, while the scheme,
/// host, port, path and parameter names stay, so the target is still
/// recognisable. A username on its own becomes `***` too (a token is often
/// put there: `https://<token>@host/`), and so does a query item without `=`,
/// which is all value. A URL that does not even parse (only a row written
/// around the API can hold one) is masked whole.
pub fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = reqwest::Url::parse(url) else {
        return REDACTED.to_string();
    };
    // Neither can fail for the http(s) URLs a monitor holds; a URL that cannot
    // carry credentials has none to mask.
    if parsed.password().is_some() {
        let _ = parsed.set_password(Some(REDACTED));
    } else if !parsed.username().is_empty() {
        let _ = parsed.set_username(REDACTED);
    }
    if let Some(query) = parsed.query() {
        let masked: Vec<String> = query
            .split('&')
            .map(|item| match item.split_once('=') {
                Some((name, _value)) => format!("{name}={REDACTED}"),
                None if item.is_empty() => String::new(),
                None => REDACTED.to_string(),
            })
            .collect();
        parsed.set_query(Some(&masked.join("&")));
    }
    if parsed.fragment().is_some() {
        parsed.set_fragment(Some(REDACTED));
    }
    parsed.to_string()
}

/// A proxy URL as a viewer reads it: [`redact_url`], except that a URL with
/// nothing to mask is returned as stored (the URL parser would add a `/` to
/// `http://proxy:3128`, which validation accepts without one).
fn redact_proxy_url(url: String) -> String {
    let redacted = redact_url(&url);
    match reqwest::Url::parse(&url) {
        Ok(parsed) if parsed.as_str() == redacted => url,
        _ => redacted,
    }
}

/// Whether `url` carries credentials of its own (`https://user:pass@host/`,
/// `https://token@host/`). reqwest sends them as an `Authorization: Basic`
/// header, so they count as basic auth wherever that matters: a monitor cannot
/// have both (see `validate_request_config`), and a check carrying them never
/// follows a redirect to another origin.
pub fn url_has_credentials(url: &reqwest::Url) -> bool {
    !url.username().is_empty() || url.password().is_some()
}

/// The user-editable fields of a monitor. Create and PATCH both build one,
/// run it through [`validate`], and only then write it.
#[derive(Debug, Clone, PartialEq)]
pub struct MonitorConfig {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: String,
    pub keyword: Option<String>,
    pub keyword_mode: String,
    pub expected_status_min: i32,
    pub expected_status_max: i32,
    pub interval_seconds: i32,
    pub timeout_seconds: i32,
    pub confirmation_threshold: i32,
    pub paused: bool,
    pub request_body: Option<String>,
    pub request_headers: HeadersChange,
    pub auth_username: Option<String>,
    pub auth_password: PasswordChange,
    pub follow_redirects: bool,
    pub verify_ssl: bool,
    pub recovery_threshold: i32,
    pub dns_record_type: Option<String>,
    pub dns_nameserver: Option<String>,
    pub expected_value: Option<String>,
    pub expiry_threshold_days: Option<i32>,
    pub use_tls: bool,
    pub udp_payload: Option<String>,
    pub group_id: Option<Uuid>,
    pub ip_version: String,
    pub remember_cookies: bool,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: PasswordChange,
}

impl From<&Monitor> for MonitorConfig {
    fn from(m: &Monitor) -> Self {
        MonitorConfig {
            name: m.name.clone(),
            check_type: m.check_type.clone(),
            url: m.url.clone(),
            host: m.host.clone(),
            port: m.port,
            http_method: m.http_method.clone(),
            keyword: m.keyword.clone(),
            keyword_mode: m.keyword_mode.clone(),
            expected_status_min: m.expected_status_min,
            expected_status_max: m.expected_status_max,
            interval_seconds: m.interval_seconds,
            timeout_seconds: m.timeout_seconds,
            confirmation_threshold: m.confirmation_threshold,
            paused: m.paused,
            request_body: m.request_body.clone(),
            request_headers: HeadersChange::Keep {
                names: m.request_header_names.clone(),
            },
            auth_username: m.auth_username.clone(),
            auth_password: PasswordChange::Keep {
                present: m.auth_password_enc.is_some(),
            },
            follow_redirects: m.follow_redirects,
            verify_ssl: m.verify_ssl,
            recovery_threshold: m.recovery_threshold,
            dns_record_type: m.dns_record_type.clone(),
            dns_nameserver: m.dns_nameserver.clone(),
            expected_value: m.expected_value.clone(),
            expiry_threshold_days: m.expiry_threshold_days,
            use_tls: m.use_tls,
            udp_payload: m.udp_payload.clone(),
            group_id: m.group_id,
            ip_version: m.ip_version.clone(),
            remember_cookies: m.remember_cookies,
            proxy_url: m.proxy_url.clone(),
            proxy_username: m.proxy_username.clone(),
            proxy_password: PasswordChange::Keep {
                present: m.proxy_password_enc.is_some(),
            },
        }
    }
}

impl MonitorConfig {
    /// True when a field that changes how or when the monitor is checked
    /// differs from `current`. Renaming, pausing, regrouping and the
    /// confirmation and recovery thresholds do not count; replacing or
    /// clearing a secret does (a secret cannot be compared with its
    /// ciphertext).
    pub fn changes_check_of(&self, current: &Monitor) -> bool {
        self.check_type != current.check_type
            || self.url != current.url
            || self.host != current.host
            || self.port != current.port
            || self.http_method != current.http_method
            || self.keyword != current.keyword
            || self.keyword_mode != current.keyword_mode
            || self.expected_status_min != current.expected_status_min
            || self.expected_status_max != current.expected_status_max
            || self.interval_seconds != current.interval_seconds
            || self.timeout_seconds != current.timeout_seconds
            || self.request_body != current.request_body
            || !matches!(self.request_headers, HeadersChange::Keep { .. })
            || self.auth_username != current.auth_username
            || !matches!(self.auth_password, PasswordChange::Keep { .. })
            || self.follow_redirects != current.follow_redirects
            || self.verify_ssl != current.verify_ssl
            || self.dns_record_type != current.dns_record_type
            || self.dns_nameserver != current.dns_nameserver
            || self.expected_value != current.expected_value
            || self.expiry_threshold_days != current.expiry_threshold_days
            || self.use_tls != current.use_tls
            || self.udp_payload != current.udp_payload
            || self.ip_version != current.ip_version
            || self.remember_cookies != current.remember_cookies
            || self.proxy_url != current.proxy_url
            || self.proxy_username != current.proxy_username
            || !matches!(self.proxy_password, PasswordChange::Keep { .. })
    }
}

/// `POST /projects/{id}/monitors` body. Server-owned fields (`id`,
/// `project_id`, `current_status`, ...) are not part of it, so a client that
/// sends them is ignored rather than obeyed.
#[derive(Debug, Deserialize)]
pub struct CreateMonitorRequest {
    pub name: String,
    pub check_type: String,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    #[serde(default = "default_http_method")]
    pub http_method: String,
    pub keyword: Option<String>,
    #[serde(default = "default_keyword_mode")]
    pub keyword_mode: String,
    #[serde(default = "default_expected_status_min")]
    pub expected_status_min: i32,
    #[serde(default = "default_expected_status_max")]
    pub expected_status_max: i32,
    /// Defaults by type, see [`default_interval_for`].
    pub interval_seconds: Option<i32>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: i32,
    /// Defaults by type, see [`default_confirmation_for`].
    pub confirmation_threshold: Option<i32>,
    #[serde(default)]
    pub paused: bool,
    pub request_body: Option<String>,
    pub request_headers: Option<Vec<RequestHeader>>,
    pub auth_username: Option<String>,
    pub auth_password: Option<SecretString>,
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_true")]
    pub verify_ssl: bool,
    #[serde(default = "default_recovery_threshold")]
    pub recovery_threshold: i32,
    pub dns_record_type: Option<String>,
    pub dns_nameserver: Option<String>,
    pub expected_value: Option<String>,
    pub expiry_threshold_days: Option<i32>,
    #[serde(default)]
    pub use_tls: bool,
    pub udp_payload: Option<String>,
    pub group_id: Option<Uuid>,
    #[serde(default = "default_ip_version")]
    pub ip_version: String,
    #[serde(default)]
    pub remember_cookies: bool,
    pub proxy_url: Option<String>,
    pub proxy_username: Option<String>,
    pub proxy_password: Option<SecretString>,
}

fn default_ip_version() -> String {
    "any".to_string()
}
fn default_http_method() -> String {
    "GET".to_string()
}
fn default_keyword_mode() -> String {
    "contains".to_string()
}
fn default_expected_status_min() -> i32 {
    200
}
fn default_expected_status_max() -> i32 {
    399
}
/// 60 s, or daily for the expiry types: expiry dates move slowly.
fn default_interval_for(check_type: &str) -> i32 {
    if is_expiry_type(check_type) {
        86_400
    } else {
        60
    }
}
fn is_expiry_type(check_type: &str) -> bool {
    matches!(check_type, "ssl_expiry" | "domain_expiry")
}
fn default_timeout_seconds() -> i32 {
    10
}
/// 2 failed checks, or 1 for the expiry types: they run daily, so waiting for
/// a second failure would delay the alert by a day.
fn default_confirmation_for(check_type: &str) -> i32 {
    if is_expiry_type(check_type) { 1 } else { 2 }
}
fn default_true() -> bool {
    true
}
fn default_recovery_threshold() -> i32 {
    1
}

impl CreateMonitorRequest {
    pub fn into_config(self) -> MonitorConfig {
        let interval_seconds = self
            .interval_seconds
            .unwrap_or_else(|| default_interval_for(&self.check_type));
        let confirmation_threshold = self
            .confirmation_threshold
            .unwrap_or_else(|| default_confirmation_for(&self.check_type));
        MonitorConfig {
            name: self.name,
            check_type: self.check_type,
            url: self.url,
            host: self.host,
            port: self.port,
            http_method: self.http_method,
            keyword: self.keyword,
            keyword_mode: self.keyword_mode,
            expected_status_min: self.expected_status_min,
            expected_status_max: self.expected_status_max,
            interval_seconds,
            timeout_seconds: self.timeout_seconds,
            confirmation_threshold,
            paused: self.paused,
            request_body: self.request_body,
            request_headers: match self.request_headers {
                Some(headers) => HeadersChange::Replace(headers),
                None => HeadersChange::Keep { names: Vec::new() },
            },
            auth_username: self.auth_username,
            auth_password: match self.auth_password {
                Some(password) => PasswordChange::Replace(password),
                None => PasswordChange::Keep { present: false },
            },
            follow_redirects: self.follow_redirects,
            verify_ssl: self.verify_ssl,
            recovery_threshold: self.recovery_threshold,
            dns_record_type: self.dns_record_type,
            dns_nameserver: self.dns_nameserver,
            expected_value: self.expected_value,
            expiry_threshold_days: self.expiry_threshold_days,
            use_tls: self.use_tls,
            udp_payload: self.udp_payload,
            group_id: self.group_id,
            ip_version: self.ip_version,
            remember_cookies: self.remember_cookies,
            proxy_url: self.proxy_url,
            proxy_username: self.proxy_username,
            proxy_password: match self.proxy_password {
                Some(password) => PasswordChange::Replace(password),
                None => PasswordChange::Keep { present: false },
            },
        }
    }
}

/// `PATCH` body: any subset of the editable fields. Absent fields keep
/// their current value; unknown and server-owned fields are ignored. The
/// request-config fields that can be removed distinguish absent (keep) from
/// `null` (clear), see [`nullable`]: that is how the write-only secrets are
/// cleared. `request_headers` replaces the whole list (`[]` clears it too).
#[derive(Debug, Default, Deserialize)]
pub struct PatchMonitorRequest {
    pub name: Option<String>,
    pub check_type: Option<String>,
    pub url: Option<String>,
    pub host: Option<String>,
    pub port: Option<i32>,
    pub http_method: Option<String>,
    pub keyword: Option<String>,
    pub keyword_mode: Option<String>,
    pub expected_status_min: Option<i32>,
    pub expected_status_max: Option<i32>,
    pub interval_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub confirmation_threshold: Option<i32>,
    pub paused: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub request_body: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub request_headers: Option<Option<Vec<RequestHeader>>>,
    #[serde(default, deserialize_with = "nullable")]
    pub auth_username: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub auth_password: Option<Option<SecretString>>,
    pub follow_redirects: Option<bool>,
    pub verify_ssl: Option<bool>,
    pub recovery_threshold: Option<i32>,
    pub dns_record_type: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub dns_nameserver: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub expected_value: Option<Option<String>>,
    /// `null` resets to the type's default (see `validate_threshold`).
    #[serde(default, deserialize_with = "nullable")]
    pub expiry_threshold_days: Option<Option<i32>>,
    pub use_tls: Option<bool>,
    pub udp_payload: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub group_id: Option<Option<Uuid>>,
    pub ip_version: Option<String>,
    pub remember_cookies: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub proxy_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub proxy_username: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub proxy_password: Option<Option<SecretString>>,
}

/// A nullable PATCH field applied to its current value.
fn patched<T>(patch: Option<Option<T>>, current: Option<T>) -> Option<T> {
    match patch {
        None => current,
        Some(value) => value,
    }
}

impl PatchMonitorRequest {
    pub fn apply_to(self, base: MonitorConfig) -> MonitorConfig {
        // The threshold's default differs per expiry type (14 days for ssl,
        // 30 for domain): a type change drops the stored one unless the
        // request names a value, and validation then fills the new default.
        let new_type = self.check_type.as_deref().unwrap_or(&base.check_type);
        let expiry_threshold_days = match self.expiry_threshold_days {
            Some(explicit) => explicit,
            None if new_type != base.check_type => None,
            None => base.expiry_threshold_days,
        };
        // Likewise a port that is the default of the old type and `use_tls`
        // (smtp 25, smtps 465, ...) follows them to the new default when the
        // request names none: `use_tls: true` must not leave a TLS check on
        // the plaintext port. A port the user chose stays.
        let new_tls = self.use_tls.unwrap_or(base.use_tls);
        let port = match self.port {
            Some(explicit) => Some(explicit),
            None if (new_type, new_tls) != (base.check_type.as_str(), base.use_tls)
                && default_port(new_type, new_tls).is_some()
                && base.port.is_some()
                && base.port == default_port(&base.check_type, base.use_tls) =>
            {
                None
            }
            None => base.port,
        };
        // `proxy_url: null` (or `""`) removes the proxy, and with it the stored username
        // (validation then drops the password); a username the request names
        // next to it is still refused as a username without a proxy.
        let proxy_username = match self.proxy_username {
            Some(explicit) => explicit,
            None if matches!(&self.proxy_url, Some(None)) => None,
            None if matches!(&self.proxy_url, Some(Some(url)) if url.is_empty()) => None,
            None => base.proxy_username,
        };
        MonitorConfig {
            name: self.name.unwrap_or(base.name),
            check_type: self.check_type.unwrap_or(base.check_type),
            url: self.url.or(base.url),
            host: self.host.or(base.host),
            port,
            http_method: self.http_method.unwrap_or(base.http_method),
            keyword: self.keyword.or(base.keyword),
            keyword_mode: self.keyword_mode.unwrap_or(base.keyword_mode),
            expected_status_min: self.expected_status_min.unwrap_or(base.expected_status_min),
            expected_status_max: self.expected_status_max.unwrap_or(base.expected_status_max),
            interval_seconds: self.interval_seconds.unwrap_or(base.interval_seconds),
            timeout_seconds: self.timeout_seconds.unwrap_or(base.timeout_seconds),
            confirmation_threshold: self
                .confirmation_threshold
                .unwrap_or(base.confirmation_threshold),
            paused: self.paused.unwrap_or(base.paused),
            request_body: patched(self.request_body, base.request_body),
            request_headers: match self.request_headers {
                None => base.request_headers,
                Some(None) => HeadersChange::Clear,
                Some(Some(headers)) => HeadersChange::Replace(headers),
            },
            auth_username: patched(self.auth_username, base.auth_username),
            auth_password: match self.auth_password {
                None => base.auth_password,
                Some(None) => PasswordChange::Clear,
                Some(Some(password)) => PasswordChange::Replace(password),
            },
            follow_redirects: self.follow_redirects.unwrap_or(base.follow_redirects),
            verify_ssl: self.verify_ssl.unwrap_or(base.verify_ssl),
            recovery_threshold: self.recovery_threshold.unwrap_or(base.recovery_threshold),
            dns_record_type: self.dns_record_type.or(base.dns_record_type),
            dns_nameserver: patched(self.dns_nameserver, base.dns_nameserver),
            expected_value: patched(self.expected_value, base.expected_value),
            expiry_threshold_days,
            use_tls: self.use_tls.unwrap_or(base.use_tls),
            udp_payload: self.udp_payload.or(base.udp_payload),
            group_id: patched(self.group_id, base.group_id),
            ip_version: self.ip_version.unwrap_or(base.ip_version),
            remember_cookies: self.remember_cookies.unwrap_or(base.remember_cookies),
            proxy_url: patched(self.proxy_url, base.proxy_url),
            proxy_username,
            proxy_password: match self.proxy_password {
                None => base.proxy_password,
                Some(None) => PasswordChange::Clear,
                Some(Some(password)) => PasswordChange::Replace(password),
            },
        }
    }
}

/// Fail-closed validation of a complete config. PATCH validates the merged
/// result, so every rule here also holds for edits. Returns the normalized
/// config (trimmed name/url/host; fields irrelevant to the check type
/// cleared) or a message for a 400.
pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
    c.name = validate_name(&c.name)?;
    if !HTTP_METHODS.contains(&c.http_method.as_str()) {
        return Err(format!(
            "http_method must be one of {}",
            HTTP_METHODS.join(", ")
        ));
    }
    if !matches!(c.keyword_mode.as_str(), "contains" | "not_contains") {
        return Err("keyword_mode must be contains or not_contains".into());
    }
    if IpVersion::parse(&c.ip_version).is_none() {
        return Err("ip_version must be any, ipv4 or ipv6".into());
    }
    match c.check_type.as_str() {
        "http" | "keyword" => validate_http(&mut c)?,
        "tcp" => validate_tcp(&mut c)?,
        "ping" => validate_ping(&mut c)?,
        "dns" => validate_dns(&mut c)?,
        "ssl_expiry" => validate_ssl_expiry(&mut c)?,
        "domain_expiry" => validate_domain_expiry(&mut c)?,
        "smtp" | "pop3" | "imap" => validate_mail(&mut c)?,
        "udp" => validate_udp(&mut c)?,
        _ => {
            return Err(format!(
                "check_type must be one of {}",
                CHECK_TYPES.join(", ")
            ));
        }
    }
    clear_unused_fields(&mut c);
    if c.interval_seconds < 30 {
        return Err("interval_seconds must be at least 30".into());
    }
    if is_expiry_type(&c.check_type) && c.interval_seconds < 3600 {
        return Err(
            "interval_seconds must be at least 3600 for ssl_expiry and domain_expiry monitors"
                .into(),
        );
    }
    if !(1..=60).contains(&c.timeout_seconds) {
        return Err("timeout_seconds must be between 1 and 60".into());
    }
    // The worker re-claims a monitor `interval_seconds` after claiming it, so
    // a check allowed to run as long as its interval would still be in
    // flight when the next one starts.
    if c.timeout_seconds >= c.interval_seconds {
        return Err("timeout_seconds must be less than interval_seconds".into());
    }
    if !(100..=599).contains(&c.expected_status_min)
        || !(100..=599).contains(&c.expected_status_max)
    {
        return Err("expected status codes must be between 100 and 599".into());
    }
    if c.expected_status_min > c.expected_status_max {
        return Err("expected_status_min must not exceed expected_status_max".into());
    }
    if !(1..=10).contains(&c.confirmation_threshold) {
        return Err("confirmation_threshold must be between 1 and 10".into());
    }
    if !(1..=10).contains(&c.recovery_threshold) {
        return Err("recovery_threshold must be between 1 and 10".into());
    }
    Ok(c)
}

fn validate_http(c: &mut MonitorConfig) -> Result<(), String> {
    // Checked before trimming, like names: U+2028 and U+2029 are
    // whitespace, so `trim` would drop them at the edges instead of
    // rejecting them.
    if c.url.as_deref().is_some_and(has_invisible_or_bidi_chars) {
        return Err("url must not contain invisible or text-direction characters".into());
    }
    let url = c
        .url
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    // `Url::parse` silently strips tabs and newlines and
    // percent-encodes other control characters, so it accepts
    // strings that must never be stored (and shown in alert emails)
    // verbatim: check the string itself, not its parse.
    if url.len() > MAX_URL_BYTES {
        return Err(format!("url must be at most {MAX_URL_BYTES} bytes"));
    }
    if url.chars().any(|ch| ch.is_control() || ch.is_whitespace()) {
        return Err("url must not contain whitespace or control characters".into());
    }
    let parsed =
        reqwest::Url::parse(&url).map_err(|_| "url must be a valid http(s) URL".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("url must be a valid http(s) URL".into());
    }
    let credentials_in_url = url_has_credentials(&parsed);
    c.url = Some(url);
    if c.check_type == "keyword" {
        let keyword = c.keyword.as_deref().unwrap_or_default();
        if keyword.trim().is_empty() {
            return Err("keyword is required for keyword monitors".into());
        }
        validate_keyword(keyword)?;
        // Any other method has a body to search; a HEAD response has none.
        if c.http_method == "HEAD" {
            return Err(
                "keyword monitors cannot use HEAD: a HEAD response has no body to search (use GET)"
                    .into(),
            );
        }
    }
    validate_request_config(c, credentials_in_url)
}

/// The rules of every keyword: at most [`MAX_KEYWORD_BYTES`] and no control
/// characters, because keywords end up in error text, incidents and emails.
fn validate_keyword(keyword: &str) -> Result<(), String> {
    if keyword.len() > MAX_KEYWORD_BYTES {
        return Err(format!("keyword must be at most {MAX_KEYWORD_BYTES} bytes"));
    }
    if keyword.chars().any(char::is_control) {
        return Err("keyword must not contain control characters".into());
    }
    Ok(())
}

/// Body, headers and basic auth of an http or keyword monitor (the method is
/// checked by [`validate`]); `credentials_in_url` says whether its URL carries
/// credentials of its own. Error messages never repeat a secret.
fn validate_request_config(c: &mut MonitorConfig, credentials_in_url: bool) -> Result<(), String> {
    c.request_body = c.request_body.take().filter(|body| !body.is_empty());
    if let Some(body) = &c.request_body {
        if !BODY_METHODS.contains(&c.http_method.as_str()) {
            return Err("request_body is only allowed with POST, PUT or PATCH".into());
        }
        if body.len() > MAX_BODY_BYTES {
            return Err(format!(
                "request_body must be at most {MAX_BODY_BYTES} bytes"
            ));
        }
        // Postgres text cannot hold NUL: refuse it here, as a 400, instead of
        // failing the insert as a 500.
        if body.contains('\0') {
            return Err("request_body must not contain NUL characters".into());
        }
    }
    if let HeadersChange::Replace(headers) = &mut c.request_headers {
        let headers = monitor_request::validate_headers(std::mem::take(headers))?;
        c.request_headers = if headers.is_empty() {
            HeadersChange::Clear
        } else {
            HeadersChange::Replace(headers)
        };
    }
    c.auth_username = match c
        .auth_username
        .take()
        .filter(|username| !username.is_empty())
    {
        Some(username) => Some(monitor_request::validate_username(&username)?),
        None => None,
    };
    if let PasswordChange::Replace(password) = &c.auth_password {
        if c.auth_username.is_none() {
            return Err("auth_password requires auth_username".into());
        }
        monitor_request::validate_password(password)?;
    }
    if c.auth_username.is_none() && c.auth_password.is_set() {
        // Removing the username removes basic auth, password included.
        c.auth_password = PasswordChange::Clear;
    }
    let has_authorization_header = c
        .request_headers
        .names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case("authorization"));
    if c.auth_username.is_some() && has_authorization_header {
        return Err("use either basic auth or an Authorization header, not both".into());
    }
    // Credentials in the URL become an `Authorization` header too, and basic
    // auth would add a second one next to it.
    if c.auth_username.is_some() && credentials_in_url {
        return Err("use either basic auth or credentials in the url, not both".into());
    }
    validate_proxy(c)
}

/// The proxy of an http or keyword monitor: an optional URL (see
/// `monitor_request::validate_proxy_url`), a username that needs the URL and
/// a password that needs the username. Removing the URL removes the username
/// and password; removing the username removes the password. A proxy resolves
/// the target itself, so `ip_version` cannot be combined with one. Error
/// messages never repeat the password.
fn validate_proxy(c: &mut MonitorConfig) -> Result<(), String> {
    c.proxy_url = match c.proxy_url.take().filter(|url| !url.is_empty()) {
        Some(url) => Some(monitor_request::validate_proxy_url(&url)?),
        None => None,
    };
    c.proxy_username = match c
        .proxy_username
        .take()
        .filter(|username| !username.is_empty())
    {
        Some(username) => {
            if c.proxy_url.is_none() {
                return Err("proxy_username requires proxy_url".into());
            }
            Some(monitor_request::validate_proxy_username(&username)?)
        }
        None => None,
    };
    if let PasswordChange::Replace(password) = &c.proxy_password {
        if c.proxy_username.is_none() {
            return Err("proxy_password requires proxy_username".into());
        }
        monitor_request::validate_proxy_password(password)?;
    }
    if c.proxy_username.is_none() && c.proxy_password.is_set() {
        // No username (because there is no proxy, or the username was
        // removed): the stored password goes with it.
        c.proxy_password = PasswordChange::Clear;
    }
    if c.proxy_url.is_some() && c.ip_version != "any" {
        return Err(
            "ip_version cannot be combined with a proxy: the proxy resolves the target".into(),
        );
    }
    Ok(())
}

/// Clears every type-specific field the check type does not use, so a monitor
/// never keeps stale config from a type it had before. Every type's
/// validation adds the fields it uses to the lists here.
fn clear_unused_fields(c: &mut MonitorConfig) {
    let check_type = c.check_type.clone();
    let uses = |types: &[&str]| types.contains(&check_type.as_str());
    if !uses(&["http", "keyword"]) {
        c.url = None;
        clear_request_config(c);
    }
    if !uses(&IP_VERSION_TYPES) {
        c.ip_version = default_ip_version();
    }
    if !uses(&["keyword", "udp"]) {
        c.keyword = None;
    }
    if !uses(&[
        "tcp",
        "ping",
        "dns",
        "ssl_expiry",
        "domain_expiry",
        "smtp",
        "pop3",
        "imap",
        "udp",
    ]) {
        c.host = None;
    }
    if !uses(&["tcp", "ssl_expiry", "smtp", "pop3", "imap", "udp"]) {
        c.port = None;
    }
    if !uses(&["smtp", "pop3", "imap"]) {
        c.use_tls = false;
    }
    if !uses(&["udp"]) {
        c.udp_payload = None;
    }
    if !uses(&["dns"]) {
        c.dns_record_type = None;
        c.dns_nameserver = None;
        c.expected_value = None;
    }
    if !uses(&["ssl_expiry", "domain_expiry"]) {
        c.expiry_threshold_days = None;
    }
}

/// Non-http types carry no request config. Secrets are only marked for
/// clearing when one is stored or supplied, so editing such a monitor does
/// not count as a check change (see `changes_check_of`).
fn clear_request_config(c: &mut MonitorConfig) {
    c.http_method = default_http_method();
    c.request_body = None;
    c.auth_username = None;
    c.follow_redirects = true;
    c.verify_ssl = true;
    if !matches!(&c.request_headers, HeadersChange::Keep { names } if names.is_empty()) {
        c.request_headers = HeadersChange::Clear;
    }
    if !matches!(c.auth_password, PasswordChange::Keep { present: false }) {
        c.auth_password = PasswordChange::Clear;
    }
    c.remember_cookies = false;
    c.proxy_url = None;
    c.proxy_username = None;
    if !matches!(c.proxy_password, PasswordChange::Keep { present: false }) {
        c.proxy_password = PasswordChange::Clear;
    }
}

fn validate_tcp(c: &mut MonitorConfig) -> Result<(), String> {
    let host = check_host_text(c.host.as_deref().unwrap_or_default())?.to_string();
    if host.is_empty() {
        return Err("host is required for tcp monitors".into());
    }
    // The port has its own field; a colon is only valid inside an
    // IPv6 literal. Without this, `db:5432` is stored and then
    // reports DOWN forever because no such name resolves.
    if host.contains(':') && host.parse::<Ipv6Addr>().is_err() {
        return Err("host must be a hostname or IP address; put the port in the port field".into());
    }
    if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
        return Err("port must be between 1 and 65535".into());
    }
    c.host = Some(host);
    Ok(())
}

/// The trimmed, validated `host` every host-based type requires (see
/// [`validate_host`], which a tcp monitor's host follows the text rules of).
fn required_host(c: &MonitorConfig) -> Result<String, String> {
    let host = check_host_text(c.host.as_deref().unwrap_or_default())?;
    if host.is_empty() {
        return Err(format!("host is required for {} monitors", c.check_type));
    }
    validate_host(host)
}

/// ping: a host and nothing else (ICMP has no port).
fn validate_ping(c: &mut MonitorConfig) -> Result<(), String> {
    c.host = Some(required_host(c)?);
    Ok(())
}

/// dns: the name to look up, the record type (default `A`), an optional
/// nameserver (`ip:port`, the system resolver when absent) and an optional
/// text an answer must contain. The nameserver is only an address here: a
/// check refuses the ones no check may contact (see `uptime::dns`).
fn validate_dns(c: &mut MonitorConfig) -> Result<(), String> {
    let host = required_host(c)?;
    // An address has no records to look up (`169.254.169.254` would be down
    // forever with NXDOMAIN).
    if is_ip_literal(&host) {
        return Err("host must be a domain name to look up".into());
    }
    c.host = Some(host);
    let record_type = c
        .dns_record_type
        .as_deref()
        .map(|t| t.trim().to_ascii_uppercase())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "A".to_string());
    if !DNS_RECORD_TYPES.contains(&record_type.as_str()) {
        return Err(format!(
            "dns_record_type must be one of {}",
            DNS_RECORD_TYPES.join(", ")
        ));
    }
    c.dns_record_type = Some(record_type);
    c.dns_nameserver = match c
        .dns_nameserver
        .as_deref()
        .map(str::trim)
        .filter(|ns| !ns.is_empty())
    {
        Some(raw) => Some(parse_nameserver(raw)?),
        None => None,
    };
    c.expected_value = validate_expected_value(c.expected_value.take())?;
    Ok(())
}

/// Whether `host` names an address rather than a domain, in any spelling a
/// resolver or URL parser would read as one: dotted, decimal (`2852039166`),
/// hex or octal parts (`0xa9.0xfe.0xa9.0xfe`), a trailing dot, IPv6 with or
/// without brackets, or a zone id (`fe80::1%en0`, which no name contains).
fn is_ip_literal(host: &str) -> bool {
    if host.contains('%') {
        return true;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<IpAddr>().is_ok() {
        return true;
    }
    matches!(
        url::Host::parse(host),
        Ok(url::Host::Ipv4(_) | url::Host::Ipv6(_))
    )
}

/// ssl_expiry: a host, a port (default 443) and the days before expiry
/// that make the monitor go down (default 14).
fn validate_ssl_expiry(c: &mut MonitorConfig) -> Result<(), String> {
    c.host = Some(required_host(c)?);
    let port = c.port.unwrap_or(443);
    if !(1..=65535).contains(&port) {
        return Err("port must be between 1 and 65535".into());
    }
    c.port = Some(port);
    c.expiry_threshold_days = Some(validate_threshold(
        c.expiry_threshold_days,
        DEFAULT_SSL_THRESHOLD_DAYS,
    )?);
    Ok(())
}

/// domain_expiry: a registered domain name (not an IP address, not a single
/// label), stored without a trailing dot, and the threshold (default 30).
fn validate_domain_expiry(c: &mut MonitorConfig) -> Result<(), String> {
    let host = required_host(c)?;
    let domain = host.trim_end_matches('.').to_string();
    if domain.parse::<IpAddr>().is_ok() || !domain.contains('.') {
        return Err("host must be a domain name such as example.com".into());
    }
    c.host = Some(domain);
    c.expiry_threshold_days = Some(validate_threshold(
        c.expiry_threshold_days,
        DEFAULT_DOMAIN_THRESHOLD_DAYS,
    )?);
    Ok(())
}

/// smtp, pop3 and imap: a host and a port that defaults to the protocol's:
/// 25, 110, 143, or with implicit TLS (`use_tls`) 465, 995, 993.
fn validate_mail(c: &mut MonitorConfig) -> Result<(), String> {
    c.host = Some(required_host(c)?);
    let default = default_port(&c.check_type, c.use_tls).unwrap_or(993);
    let port = c.port.unwrap_or(default);
    if !(1..=65535).contains(&port) {
        return Err("port must be between 1 and 65535".into());
    }
    c.port = Some(port);
    Ok(())
}

/// The port a type uses when none is given: 443 for ssl_expiry, and for the
/// mail types the protocol's, plain or with implicit TLS. `None` for the types
/// that require a port (tcp, udp) or have none.
fn default_port(check_type: &str, use_tls: bool) -> Option<i32> {
    Some(match (check_type, use_tls) {
        ("ssl_expiry", _) => 443,
        ("smtp", false) => 25,
        ("smtp", true) => 465,
        ("pop3", false) => 110,
        ("pop3", true) => 995,
        ("imap", false) => 143,
        ("imap", true) => 993,
        _ => return None,
    })
}

/// udp: host, port and the payload to send are required; `keyword` is an
/// optional match on the reply and follows the rules of every keyword.
fn validate_udp(c: &mut MonitorConfig) -> Result<(), String> {
    c.host = Some(required_host(c)?);
    if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
        return Err("port must be between 1 and 65535".into());
    }
    let Some(payload) = c
        .udp_payload
        .as_deref()
        .filter(|payload| !payload.is_empty())
    else {
        return Err("udp_payload is required for udp monitors".into());
    };
    // Postgres text cannot hold NUL: a 400 here, not a failed insert.
    if payload.contains('\0') {
        return Err("udp_payload must not contain NUL characters".into());
    }
    if payload.len() > MAX_UDP_PAYLOAD_BYTES {
        return Err(format!(
            "udp_payload must be at most {MAX_UDP_PAYLOAD_BYTES} bytes"
        ));
    }
    c.keyword = c.keyword.take().filter(|keyword| !keyword.is_empty());
    if let Some(keyword) = &c.keyword {
        validate_keyword(keyword)?;
    }
    Ok(())
}

fn validate_threshold(days: Option<i32>, default: i32) -> Result<i32, String> {
    let days = days.unwrap_or(default);
    if !(1..=365).contains(&days) {
        return Err("expiry_threshold_days must be between 1 and 365".into());
    }
    Ok(days)
}

/// Optional text an answer must contain: trimmed, empty means unset, and the
/// rules of a keyword (at most [`MAX_EXPECTED_VALUE_BYTES`], no control
/// characters): it is shown in error text, incidents and emails.
fn validate_expected_value(raw: Option<String>) -> Result<Option<String>, String> {
    let Some(value) = raw
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    if value.len() > MAX_EXPECTED_VALUE_BYTES {
        return Err(format!(
            "expected_value must be at most {MAX_EXPECTED_VALUE_BYTES} bytes"
        ));
    }
    if value.chars().any(char::is_control) {
        return Err("expected_value must not contain control characters".into());
    }
    Ok(Some(value))
}

fn has_invisible_or_bidi_chars(text: &str) -> bool {
    text.chars().any(is_invisible_or_bidi_char)
}

pub async fn insert_monitor(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    c: &MonitorConfig,
) -> Result<Monitor, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // The row starts from the column defaults; `write_config` then sets
    // every configurable column, so each column is written in one place.
    let id: Uuid = sqlx::query_scalar(
        "insert into monitors (project_id, name, check_type, interval_seconds)
         values ($1, $2, $3, $4)
         returning id",
    )
    .bind(project_id)
    .bind(&c.name)
    .bind(&c.check_type)
    .bind(c.interval_seconds)
    .fetch_one(&mut *tx)
    .await?;
    let monitor = write_config(&mut tx, cipher, project_id, id, c, Write::default()).await?;
    tx.commit().await?;
    Ok(monitor)
}

/// What else a [`write_config`] does besides writing the config.
#[derive(Debug, Clone, Copy, Default)]
struct Write {
    /// Make the monitor due now.
    reschedule: bool,
    /// Zero `consecutive_failures` and `consecutive_successes`.
    reset_counters: bool,
}

/// Writes every configurable column of monitor `id` of project `project_id`
/// from a validated config, sealing new secrets for that monitor.
/// `HeadersChange::Keep`/`PasswordChange::Keep` leave the stored ciphertext
/// untouched. The statement is scoped by both ids, so it can never write
/// another project's row whatever its caller checked. New columns are
/// appended at the end of the statement.
async fn write_config(
    conn: &mut PgConnection,
    cipher: &Cipher,
    project_id: Uuid,
    id: Uuid,
    c: &MonitorConfig,
    write: Write,
) -> Result<Monitor, sqlx::Error> {
    let (keep_headers, header_names, sealed_headers) = match &c.request_headers {
        HeadersChange::Keep { .. } => (true, Vec::new(), None),
        HeadersChange::Clear => (false, Vec::new(), None),
        HeadersChange::Replace(headers) => (
            false,
            headers
                .iter()
                .map(|h| h.name.clone())
                .collect::<Vec<String>>(),
            Some(seal_headers(cipher, id, headers)),
        ),
    };
    let (keep_password, sealed_password) = match &c.auth_password {
        PasswordChange::Keep { .. } => (true, None),
        PasswordChange::Clear => (false, None),
        PasswordChange::Replace(password) => (false, Some(seal_password(cipher, id, password))),
    };
    let (keep_proxy_password, sealed_proxy_password) = match &c.proxy_password {
        PasswordChange::Keep { .. } => (true, None),
        PasswordChange::Clear => (false, None),
        PasswordChange::Replace(password) => {
            (false, Some(seal_proxy_password(cipher, id, password)))
        }
    };
    sqlx::query_as(
        "update monitors set
             next_check_at = case when $2 then now() else next_check_at end,
             name = $3, check_type = $4, url = $5, host = $6, port = $7, http_method = $8,
             keyword = $9, keyword_mode = $10, expected_status_min = $11, expected_status_max = $12,
             interval_seconds = $13, timeout_seconds = $14, confirmation_threshold = $15, paused = $16,
             request_body = $17, auth_username = $18, follow_redirects = $19, verify_ssl = $20,
             request_header_names = case when $21 then request_header_names else $22 end,
             request_headers_enc = case when $21 then request_headers_enc else $23 end,
             auth_password_enc = case when $24 then auth_password_enc else $25 end,
             recovery_threshold = $26,
             dns_record_type = $27, dns_nameserver = $28, expected_value = $29,
             expiry_threshold_days = $30,
             use_tls = $31, udp_payload = $32,
             group_id = $33,
             consecutive_failures = case when $35 then 0 else consecutive_failures end,
             consecutive_successes = case when $35 then 0 else consecutive_successes end,
             ip_version = $36, remember_cookies = $37, proxy_url = $38, proxy_username = $39,
             proxy_password_enc = case when $40 then proxy_password_enc else $41 end
         where id = $1 and project_id = $34
         returning *",
    )
    .bind(id)
    .bind(write.reschedule)
    .bind(&c.name)
    .bind(&c.check_type)
    .bind(&c.url)
    .bind(&c.host)
    .bind(c.port)
    .bind(&c.http_method)
    .bind(&c.keyword)
    .bind(&c.keyword_mode)
    .bind(c.expected_status_min)
    .bind(c.expected_status_max)
    .bind(c.interval_seconds)
    .bind(c.timeout_seconds)
    .bind(c.confirmation_threshold)
    .bind(c.paused)
    .bind(&c.request_body)
    .bind(&c.auth_username)
    .bind(c.follow_redirects)
    .bind(c.verify_ssl)
    .bind(keep_headers)
    .bind(header_names)
    .bind(sealed_headers)
    .bind(keep_password)
    .bind(sealed_password)
    .bind(c.recovery_threshold)
    .bind(&c.dns_record_type)
    .bind(&c.dns_nameserver)
    .bind(&c.expected_value)
    .bind(c.expiry_threshold_days)
    .bind(c.use_tls)
    .bind(&c.udp_payload)
    .bind(c.group_id)
    .bind(project_id)
    .bind(write.reset_counters)
    .bind(&c.ip_version)
    .bind(c.remember_cookies)
    .bind(&c.proxy_url)
    .bind(&c.proxy_username)
    .bind(keep_proxy_password)
    .bind(sealed_proxy_password)
    .fetch_one(&mut *conn)
    .await
}

pub async fn list_for_project(
    pool: &PgPool,
    project_id: Uuid,
) -> Result<Vec<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where project_id = $1 order by name, id")
        .bind(project_id)
        .fetch_all(pool)
        .await
}

/// Every lookup is scoped by BOTH ids: a monitor id from another project is
/// simply not found here (404), even for a caller who can see that project.
pub async fn find_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<Option<Monitor>, sqlx::Error> {
    sqlx::query_as("select * from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .fetch_optional(pool)
        .await
}

/// Merge-validate-write in one transaction. The `for update` row lock makes
/// concurrent PATCHes apply one after the other instead of the second one
/// overwriting the first with values it read before the first committed.
/// Editing any check-config field reschedules the monitor to run now.
pub async fn update_in_project(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    monitor_id: Uuid,
    patch: PatchMonitorRequest,
) -> Result<Option<Monitor>, AppError> {
    let mut tx = pool.begin().await?;
    let current: Option<Monitor> =
        sqlx::query_as("select * from monitors where id = $1 and project_id = $2 for update")
            .bind(monitor_id)
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(current) = current else {
        return Ok(None);
    };
    let resumed = current.paused && patch.paused == Some(false);
    let config =
        validate(patch.apply_to(MonitorConfig::from(&current))).map_err(AppError::Validation)?;
    secrets_stay_where_they_were(&current, &config).map_err(AppError::Validation)?;
    secrets_follow_the_proxy(&current, &config).map_err(AppError::Validation)?;
    if let Some(group_id) = config.group_id.filter(|g| Some(*g) != current.group_id) {
        // A group of another project is a 404, like any foreign id.
        if !monitor_group::belongs_to(&mut *tx, project_id, group_id).await? {
            return Err(AppError::NotFound);
        }
    }
    let reschedule = config.changes_check_of(&current);
    let write = Write {
        reschedule,
        // Results against the old target, or from before the pause, say
        // nothing about the monitor as it is now.
        reset_counters: reschedule || resumed,
    };
    let updated = write_config(&mut tx, cipher, project_id, monitor_id, &config, write)
        .await
        .map_err(|e| match &e {
            // The group was deleted between the check and the write.
            sqlx::Error::Database(db_err) if db_err.code().as_deref() == Some("23503") => {
                AppError::NotFound
            }
            _ => AppError::Database(e),
        })?;
    tx.commit().await?;
    Ok(Some(updated))
}

/// Refuses a PATCH that would send a stored write-only secret (header
/// values, the basic-auth password) somewhere it was not entered for: to
/// another origin (scheme, host or port, compared as `Url::origin` does, the
/// rule redirects follow too), or over TLS without certificate verification.
/// A member who never saw the secrets could otherwise point the monitor at a
/// server of their own and read them off its next check. Every stored secret
/// must be entered again or cleared in the same request. The message never
/// names a header or a value.
fn secrets_stay_where_they_were(current: &Monitor, config: &MonitorConfig) -> Result<(), String> {
    let keeps_headers = current.request_headers_enc.is_some()
        && matches!(config.request_headers, HeadersChange::Keep { .. });
    let keeps_password = current.auth_password_enc.is_some()
        && matches!(config.auth_password, PasswordChange::Keep { .. });
    if !keeps_headers && !keeps_password {
        return Ok(());
    }
    let origin = |url: Option<&str>| {
        url.and_then(|u| reqwest::Url::parse(u).ok())
            .map(|u| u.origin())
    };
    let (before, after) = (
        origin(current.url.as_deref()),
        origin(config.url.as_deref()),
    );
    // An unparsable stored URL (only a row written around the API holds one)
    // gives no origin to compare with: refuse, like a redirect with nothing
    // to compare to.
    if before.is_none() || before != after {
        return Err("changing the url's scheme, host or port requires entering the header values and password again, or clearing them".into());
    }
    let over_tls = config
        .url
        .as_deref()
        .is_some_and(|u| reqwest::Url::parse(u).is_ok_and(|p| p.scheme() == "https"));
    if over_tls && current.verify_ssl && !config.verify_ssl {
        return Err("turning off certificate verification requires entering the header values and password again, or clearing them".into());
    }
    Ok(())
}

/// Refuses a PATCH that moves the proxy to another origin (scheme, host or
/// port, compared as `Url::origin` does; adding a proxy where there was none
/// counts, removing one does not) while keeping any stored write-only secret:
/// header values, the basic-auth password or the proxy password. The proxy
/// password goes to the proxy, and the target's headers and basic auth pass
/// through it readable whenever the target is `http://` (or unverified TLS),
/// so a member who never saw them could otherwise point the proxy at a host
/// of their own and read them off the next check. Every stored secret must be
/// entered again or cleared in the same request. A change of the target URL
/// alone does not ask for the proxy password: it only ever goes to the proxy,
/// which did not change (`secrets_stay_where_they_were` covers the target's
/// secrets). The message never names a header or a value.
fn secrets_follow_the_proxy(current: &Monitor, config: &MonitorConfig) -> Result<(), String> {
    let Some(new_proxy) = config.proxy_url.as_deref() else {
        return Ok(());
    };
    let origin = |url: &str| reqwest::Url::parse(url).ok().map(|u| u.origin());
    let before = current.proxy_url.as_deref().and_then(origin);
    // An unparsable stored proxy URL (only a row written around the API holds
    // one) gives no origin to compare with: treated as a change.
    if before.is_some() && before == origin(new_proxy) {
        return Ok(());
    }
    let keeps_headers = current.request_headers_enc.is_some()
        && matches!(config.request_headers, HeadersChange::Keep { .. });
    let keeps_password = current.auth_password_enc.is_some()
        && matches!(config.auth_password, PasswordChange::Keep { .. });
    let keeps_proxy_password = current.proxy_password_enc.is_some()
        && matches!(config.proxy_password, PasswordChange::Keep { .. });
    if keeps_headers || keeps_password || keeps_proxy_password {
        return Err("changing the proxy requires entering the header values and passwords again, or clearing them".into());
    }
    Ok(())
}

/// Returns false when no monitor with this id exists in this project.
pub async fn delete_in_project(
    pool: &PgPool,
    project_id: Uuid,
    monitor_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("delete from monitors where id = $1 and project_id = $2")
        .bind(monitor_id)
        .bind(project_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::crypto::SecretString;

    #[test]
    fn credentials_query_values_and_the_fragment_are_masked() {
        for (url, masked) in [
            (
                "https://user:pass@example.com/health?token=abc&region=eu#frag",
                "https://user:***@example.com/health?token=***&region=***#***",
            ),
            (
                "https://example.com/a/b?key=",
                "https://example.com/a/b?key=***",
            ),
            (
                "https://example.com/?a=1&&b=2",
                "https://example.com/?a=***&&b=***",
            ),
            ("https://example.com/?flag", "https://example.com/?***"),
            ("https://example.com/?", "https://example.com/?"),
            ("https://tok3n@example.com/", "https://***@example.com/"),
            // An empty password is no password: the username stands alone.
            ("https://user:@example.com/", "https://***@example.com/"),
            (
                "http://example.com:8080/x#",
                "http://example.com:8080/x#***",
            ),
        ] {
            assert_eq!(redact_url(url), masked, "{url}");
        }
    }

    #[test]
    fn a_url_without_secrets_is_unchanged() {
        for url in [
            "https://example.com/",
            "https://example.com/health/deep/path",
            "http://10.0.0.5:8080/status",
            "https://[::1]:8443/",
        ] {
            assert_eq!(redact_url(url), url);
        }
    }

    #[test]
    fn parameter_names_keep_their_encoding() {
        assert_eq!(
            redact_url("https://example.com/?api%20key=s3cret&x%3Dy=z"),
            "https://example.com/?api%20key=***&x%3Dy=***"
        );
    }

    #[test]
    fn a_url_that_does_not_parse_is_masked_whole() {
        assert_eq!(redact_url("not a url with secret=abc"), "***");
    }

    // ---- request configuration ------------------------------------------

    fn stored_monitor() -> Monitor {
        Monitor {
            id: Uuid::new_v4(),
            name: "Orders".to_string(),
            check_type: "http".to_string(),
            url: Some("https://example.com/orders".to_string()),
            http_method: "POST".to_string(),
            keyword_mode: "contains".to_string(),
            expected_status_min: 200,
            expected_status_max: 399,
            interval_seconds: 60,
            timeout_seconds: 10,
            confirmation_threshold: 2,
            current_status: "pending".to_string(),
            request_body: Some("{}".to_string()),
            request_headers_enc: Some(vec![1, 2, 3]),
            request_header_names: vec!["X-Api-Key".to_string()],
            auth_username: Some("monitor".to_string()),
            auth_password_enc: Some(vec![4, 5, 6]),
            follow_redirects: true,
            verify_ssl: true,
            recovery_threshold: 1,
            ip_version: "any".to_string(),
            ..Monitor::default()
        }
    }

    fn patch(body: serde_json::Value) -> PatchMonitorRequest {
        serde_json::from_value(body).unwrap()
    }

    fn merged(body: serde_json::Value) -> MonitorConfig {
        patch(body).apply_to(MonitorConfig::from(&stored_monitor()))
    }

    #[test]
    fn a_patch_tells_absent_from_null_from_a_value_for_every_removable_field() {
        let stored = MonitorConfig::from(&stored_monitor());
        let kept = merged(json!({}));
        assert_eq!(kept, stored, "an empty patch changes nothing");
        assert_eq!(
            kept.request_headers,
            HeadersChange::Keep {
                names: vec!["X-Api-Key".to_string()]
            }
        );
        assert_eq!(kept.auth_password, PasswordChange::Keep { present: true });

        let cleared = merged(json!({
            "request_body": null, "request_headers": null,
            "auth_username": null, "auth_password": null,
        }));
        assert_eq!(cleared.request_body, None);
        assert_eq!(cleared.request_headers, HeadersChange::Clear);
        assert_eq!(cleared.auth_username, None);
        assert_eq!(cleared.auth_password, PasswordChange::Clear);

        let set = merged(json!({
            "request_body": "b", "auth_username": "u", "auth_password": "p",
            "request_headers": [{ "name": "X-A", "value": "v" }],
        }));
        assert_eq!(set.request_body.as_deref(), Some("b"));
        assert_eq!(set.auth_username.as_deref(), Some("u"));
        assert_eq!(
            set.auth_password,
            PasswordChange::Replace(SecretString::new("p"))
        );
        assert_eq!(set.request_headers.names(), ["X-A"]);
    }

    #[test]
    fn a_create_without_secrets_keeps_nothing_and_with_them_replaces() {
        let create: CreateMonitorRequest = serde_json::from_value(json!({
            "name": "n", "check_type": "http", "url": "https://example.com",
        }))
        .unwrap();
        let config = create.into_config();
        assert_eq!(
            config.request_headers,
            HeadersChange::Keep { names: Vec::new() }
        );
        assert_eq!(
            config.auth_password,
            PasswordChange::Keep { present: false }
        );
        assert!(config.follow_redirects && config.verify_ssl);

        let create: CreateMonitorRequest = serde_json::from_value(json!({
            "name": "n", "check_type": "http", "url": "https://example.com",
            "request_headers": null, "auth_password": null,
        }))
        .unwrap();
        let config = create.into_config();
        assert_eq!(
            config.request_headers,
            HeadersChange::Keep { names: Vec::new() },
            "null on create is the same as absent"
        );
    }

    #[test]
    fn validation_normalizes_the_request_config() {
        let ok = |body: serde_json::Value| validate(merged(body)).unwrap();
        // Empty means unset.
        assert_eq!(ok(json!({ "request_body": "" })).request_body, None);
        assert_eq!(
            ok(json!({ "auth_username": "", "auth_password": null })).auth_username,
            None
        );
        // An empty list clears, a list is trimmed.
        assert_eq!(
            ok(json!({ "request_headers": [] })).request_headers,
            HeadersChange::Clear
        );
        let trimmed = ok(json!({ "request_headers": [{ "name": " X-A ", "value": "v" }] }));
        assert_eq!(trimmed.request_headers.names(), ["X-A"]);
        // No username, no basic auth: the stored password goes with it.
        let no_user = ok(json!({ "auth_username": null }));
        assert_eq!(no_user.auth_username, None);
        assert_eq!(no_user.auth_password, PasswordChange::Clear);
        // A password may be empty.
        assert_eq!(
            ok(json!({ "auth_password": "" })).auth_password,
            PasswordChange::Replace(SecretString::new(""))
        );
    }

    #[test]
    fn a_tcp_monitor_carries_no_request_config() {
        let tcp = validate(merged(json!({
            "check_type": "tcp", "host": "db.internal", "port": 5432,
        })))
        .unwrap();
        assert_eq!(tcp.http_method, "GET");
        assert_eq!(tcp.request_body, None);
        assert_eq!(tcp.auth_username, None);
        assert_eq!(tcp.request_headers, HeadersChange::Clear);
        assert_eq!(tcp.auth_password, PasswordChange::Clear);
        assert!(tcp.follow_redirects && tcp.verify_ssl);

        // Nothing stored, nothing to clear: editing it is not a check change.
        let plain = Monitor {
            check_type: "tcp".to_string(),
            url: None,
            host: Some("db.internal".to_string()),
            port: Some(5432),
            http_method: "GET".to_string(),
            request_headers_enc: None,
            request_header_names: Vec::new(),
            request_body: None,
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let renamed = validate(
            PatchMonitorRequest {
                name: Some("renamed".to_string()),
                ..PatchMonitorRequest::default()
            }
            .apply_to(MonitorConfig::from(&plain)),
        )
        .unwrap();
        assert_eq!(
            renamed.request_headers,
            HeadersChange::Keep { names: Vec::new() }
        );
        assert_eq!(
            renamed.auth_password,
            PasswordChange::Keep { present: false }
        );
        assert!(!renamed.changes_check_of(&plain));
    }

    #[test]
    fn the_method_is_checked_for_every_check_type() {
        for body in [
            json!({ "http_method": "TRACE" }),
            json!({ "http_method": "get" }),
            json!({ "http_method": "" }),
            json!({ "http_method": "CONNECT", "check_type": "tcp", "host": "h", "port": 1 }),
        ] {
            let error = validate(merged(body.clone())).unwrap_err();
            assert_eq!(
                error, "http_method must be one of GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS",
                "{body}"
            );
        }
    }

    #[test]
    fn keyword_monitors_forbid_head_and_nothing_else() {
        let keyword = |method: &str| {
            validate(merged(json!({
                "check_type": "keyword", "keyword": "ok", "http_method": method,
                "request_body": null,
            })))
        };
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(keyword(method).is_ok(), "{method}");
        }
        let error = keyword("HEAD").unwrap_err();
        assert!(
            error.contains("HEAD") && error.contains("use GET"),
            "{error}"
        );
    }

    #[test]
    fn the_body_needs_a_method_that_carries_one_and_a_size_and_text_that_fit() {
        let with = |method: &str, body: &str| {
            validate(merged(
                json!({ "http_method": method, "request_body": body }),
            ))
        };
        for method in ["POST", "PUT", "PATCH"] {
            assert!(with(method, "x").is_ok(), "{method}");
        }
        for method in ["GET", "HEAD", "DELETE", "OPTIONS"] {
            assert_eq!(
                with(method, "x").unwrap_err(),
                "request_body is only allowed with POST, PUT or PATCH",
                "{method}"
            );
            assert!(
                with(method, "").is_ok(),
                "an empty body is no body ({method})"
            );
        }
        assert!(with("POST", &"a".repeat(65536)).is_ok());
        assert_eq!(
            with("POST", &"a".repeat(65537)).unwrap_err(),
            "request_body must be at most 65536 bytes"
        );
        assert_eq!(
            with("POST", &"\u{e9}".repeat(32769)).unwrap_err(),
            "request_body must be at most 65536 bytes",
            "bytes, not characters"
        );
        assert!(with("POST", "multi\nline\r\n\tbody \u{1f680}").is_ok());
        assert_eq!(
            with("POST", "a\0b").unwrap_err(),
            "request_body must not contain NUL characters"
        );
    }

    #[test]
    fn basic_auth_and_an_authorization_header_exclude_each_other_whatever_the_case() {
        let conflict = "use either basic auth or an Authorization header, not both";
        for name in ["Authorization", "authorization", "AUTHORIZATION"] {
            let header = json!([{ "name": name, "value": "Bearer t" }]);
            assert_eq!(
                validate(merged(json!({ "request_headers": header }))).unwrap_err(),
                conflict,
                "{name}"
            );
        }
        // A header with another name is fine next to basic auth.
        assert!(
            validate(merged(json!({
                "request_headers": [{ "name": "Proxy-Authorization", "value": "x" }]
            })))
            .is_ok()
        );
        // The names of kept headers count: the conflict is found without the key.
        let with_header = Monitor {
            request_header_names: vec!["Authorization".to_string()],
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let add_auth =
            patch(json!({ "auth_username": "u" })).apply_to(MonitorConfig::from(&with_header));
        assert_eq!(validate(add_auth).unwrap_err(), conflict);
        // Replacing the list in the same request resolves it.
        let swap = patch(json!({ "auth_username": "u", "request_headers": [] }))
            .apply_to(MonitorConfig::from(&with_header));
        assert!(validate(swap).is_ok());
    }

    /// reqwest turns credentials in the URL into an `Authorization` header and
    /// adds basic auth next to it: two of them.
    #[test]
    fn basic_auth_and_credentials_in_the_url_exclude_each_other() {
        let conflict = "use either basic auth or credentials in the url, not both";
        // Basic auth is stored: credentials in the new url are refused, however
        // they are spelled.
        for url in [
            "https://user:secret@example.com/orders",
            "https://user@example.com/",
            "https://:secret@example.com/",
            "http://token@example.com:8080/x?y=1",
        ] {
            let error = validate(merged(json!({ "url": url }))).unwrap_err();
            assert_eq!(error, conflict, "{url}");
            assert!(!error.contains("secret") && !error.contains("token"));
        }
        // An empty userinfo is no credentials.
        assert!(validate(merged(json!({ "url": "https://@example.com/" }))).is_ok());
        // Without basic auth, credentials in the url are fine.
        let no_auth = Monitor {
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let with =
            |body: serde_json::Value| validate(patch(body).apply_to(MonitorConfig::from(&no_auth)));
        assert!(with(json!({ "url": "https://user:secret@example.com/" })).is_ok());
        // The stored url counts too: basic auth cannot be added next to it...
        let with_credentials = Monitor {
            url: Some("https://user:secret@example.com/".to_string()),
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let add_auth = patch(json!({ "auth_username": "u", "auth_password": "p" }))
            .apply_to(MonitorConfig::from(&with_credentials));
        assert_eq!(validate(add_auth).unwrap_err(), conflict);
        // ...unless the same request takes the credentials out of the url, or
        // does without the basic auth.
        let swap = patch(json!({ "auth_username": "u", "url": "https://example.com/" }))
            .apply_to(MonitorConfig::from(&with_credentials));
        assert!(validate(swap).is_ok());
        let url_to_creds =
            patch(json!({ "url": "https://user:p@example.com/", "auth_username": null }))
                .apply_to(MonitorConfig::from(&stored_monitor()));
        assert!(validate(url_to_creds).is_ok());
    }

    #[test]
    fn a_password_needs_a_username_and_is_checked() {
        let no_user = Monitor {
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let with =
            |body: serde_json::Value| validate(patch(body).apply_to(MonitorConfig::from(&no_user)));
        assert_eq!(
            with(json!({ "auth_password": "p" })).unwrap_err(),
            "auth_password requires auth_username"
        );
        assert!(with(json!({ "auth_username": "u", "auth_password": "p" })).is_ok());
        assert_eq!(
            with(json!({ "auth_username": "u", "auth_password": "a\nb" })).unwrap_err(),
            "auth_password must not contain control characters"
        );
    }

    #[test]
    fn only_a_change_to_the_request_reschedules_the_check() {
        let stored = stored_monitor();
        let reschedules = |body: serde_json::Value| {
            validate(patch(body).apply_to(MonitorConfig::from(&stored)))
                .unwrap()
                .changes_check_of(&stored)
        };
        for body in [
            json!({}),
            json!({ "name": "renamed" }),
            json!({ "paused": true }),
            json!({ "confirmation_threshold": 5 }),
            json!({ "recovery_threshold": 5 }),
            json!({ "http_method": "POST", "follow_redirects": true, "verify_ssl": true }),
            json!({ "request_body": "{}" }),
            json!({ "auth_username": "monitor" }),
        ] {
            assert!(!reschedules(body.clone()), "{body}");
        }
        for body in [
            json!({ "url": "https://example.org/" }),
            json!({ "http_method": "PUT" }),
            json!({ "request_body": "other" }),
            json!({ "request_body": null }),
            json!({ "request_headers": [{ "name": "X-A", "value": "v" }] }),
            json!({ "request_headers": null }),
            json!({ "auth_username": "other" }),
            json!({ "auth_password": "p" }),
            json!({ "auth_password": null }),
            json!({ "follow_redirects": false }),
            json!({ "verify_ssl": false }),
        ] {
            assert!(reschedules(body.clone()), "{body}");
        }
    }

    #[test]
    fn the_recovery_threshold_defaults_to_one_follows_a_patch_and_stays_within_one_to_ten() {
        let create: CreateMonitorRequest = serde_json::from_value(json!({
            "name": "n", "check_type": "http", "url": "https://example.com",
        }))
        .unwrap();
        assert_eq!(create.into_config().recovery_threshold, 1);

        let stored = Monitor {
            recovery_threshold: 4,
            ..stored_monitor()
        };
        let patched = |body: serde_json::Value| {
            patch(body)
                .apply_to(MonitorConfig::from(&stored))
                .recovery_threshold
        };
        assert_eq!(patched(json!({})), 4, "absent keeps what is stored");
        assert_eq!(patched(json!({ "recovery_threshold": 7 })), 7);

        for threshold in [1, 10] {
            assert!(
                validate(merged(json!({ "recovery_threshold": threshold }))).is_ok(),
                "{threshold}"
            );
        }
        for threshold in [i32::MIN, -1, 0, 11, i32::MAX] {
            assert_eq!(
                validate(merged(json!({ "recovery_threshold": threshold }))).unwrap_err(),
                "recovery_threshold must be between 1 and 10",
                "{threshold}"
            );
        }
    }

    // ---- ping and dns ---------------------------------------------------

    fn stored_dns() -> Monitor {
        Monitor {
            check_type: "dns".to_string(),
            url: None,
            host: Some("example.com".to_string()),
            http_method: "GET".to_string(),
            request_body: None,
            request_headers_enc: None,
            request_header_names: Vec::new(),
            auth_username: None,
            auth_password_enc: None,
            dns_record_type: Some("MX".to_string()),
            dns_nameserver: Some("9.9.9.9:53".to_string()),
            expected_value: Some("mail".to_string()),
            ..stored_monitor()
        }
    }

    #[test]
    fn only_a_change_to_the_dns_lookup_reschedules_the_check() {
        let stored = stored_dns();
        let reschedules = |body: serde_json::Value| {
            validate(patch(body).apply_to(MonitorConfig::from(&stored)))
                .unwrap()
                .changes_check_of(&stored)
        };
        for body in [
            json!({}),
            json!({ "name": "renamed" }),
            json!({ "dns_record_type": "mx" }),
        ] {
            assert!(!reschedules(body.clone()), "{body}");
        }
        for body in [
            json!({ "dns_record_type": "TXT" }),
            json!({ "dns_nameserver": null }),
            json!({ "dns_nameserver": "1.1.1.1" }),
            json!({ "expected_value": null }),
            json!({ "expected_value": "other" }),
            json!({ "host": "example.org" }),
            json!({ "check_type": "ping" }),
        ] {
            assert!(reschedules(body.clone()), "{body}");
        }
    }

    #[test]
    fn ping_and_dns_keep_only_their_own_fields() {
        let ping = validate(merged(
            json!({ "check_type": "ping", "host": "Gateway.LAN" }),
        ))
        .unwrap();
        assert_eq!(ping.host.as_deref(), Some("gateway.lan"));
        assert_eq!((ping.url, ping.keyword, ping.port), (None, None, None));
        assert_eq!(ping.request_headers, HeadersChange::Clear);

        let dns = validate(
            patch(json!({ "check_type": "tcp", "port": 53 }))
                .apply_to(MonitorConfig::from(&stored_dns())),
        );
        let tcp = dns.unwrap();
        assert_eq!(
            (tcp.dns_record_type, tcp.dns_nameserver, tcp.expected_value),
            (None, None, None)
        );
    }

    #[test]
    fn debug_shows_the_dns_fields() {
        let printed = format!("{:?}", stored_dns());
        for field in [
            "dns_record_type",
            "dns_nameserver",
            "expected_value",
            "9.9.9.9:53",
        ] {
            assert!(printed.contains(field), "{field} in {printed}");
        }
    }

    // ---- what a reader sees ---------------------------------------------

    #[test]
    fn members_read_the_monitor_whole_and_viewers_without_its_credentials() {
        use crate::middleware::rbac::ProjectReader;

        let monitor = Monitor {
            url: Some("https://u:pw@example.com/x?k=v".to_string()),
            ..stored_monitor()
        };
        let member = monitor.clone().for_reader(ProjectReader::Member);
        assert_eq!(member, monitor);
        let viewer = monitor.clone().for_reader(ProjectReader::Viewer);
        assert_eq!(
            viewer.url.as_deref(),
            Some("https://u:***@example.com/x?k=***")
        );
        assert_eq!(viewer.request_body.as_deref(), Some("***"));
        assert_eq!(viewer.auth_username.as_deref(), Some("***"));
        assert_eq!(viewer.request_header_names, ["X-Api-Key"]);
        assert_eq!(viewer.name, "Orders");

        // Unset stays unset: there is nothing to hide.
        let bare = Monitor {
            request_body: None,
            auth_username: None,
            ..monitor
        };
        let viewer = bare.for_reader(ProjectReader::Viewer);
        assert_eq!((viewer.request_body, viewer.auth_username), (None, None));
    }

    #[test]
    fn a_view_lists_header_names_and_whether_a_password_is_set_and_nothing_sealed() {
        let view = MonitorView::new(stored_monitor(), false);
        assert_eq!(
            view.request_headers,
            [HeaderView {
                name: "X-Api-Key".to_string(),
                value_set: true
            }]
        );
        assert!(view.has_auth_password);
        let text = serde_json::to_string(&view).unwrap();
        for hidden in ["_enc", "request_header_names", "[1,2,3]", "[4,5,6]"] {
            assert!(!text.contains(hidden), "{hidden} in {text}");
        }
        assert!(text.contains("\"request_headers\":[{\"name\":\"X-Api-Key\",\"value_set\":true}]"));

        let none = MonitorView::new(
            Monitor {
                request_headers_enc: None,
                request_header_names: Vec::new(),
                auth_password_enc: None,
                ..stored_monitor()
            },
            true,
        );
        assert!(none.in_maintenance);
        assert!(none.request_headers.is_empty() && !none.has_auth_password);
    }

    #[test]
    fn debug_leaves_the_sealed_columns_out() {
        let monitor = stored_monitor();
        for printed in [format!("{monitor:?}"), format!("{monitor:#?}")] {
            assert!(printed.contains("<sealed>"), "{printed}");
            assert!(!printed.contains("[1, 2, 3]") && !printed.contains("[4, 5, 6]"));
            assert!(printed.contains("Orders") && printed.contains("X-Api-Key"));
            assert!(
                printed.contains("recovery_threshold") && printed.contains("consecutive_successes"),
                "{printed}"
            );
        }
        let unsealed = format!(
            "{:?}",
            Monitor {
                request_headers_enc: None,
                auth_password_enc: None,
                ..monitor
            }
        );
        assert!(!unsealed.contains("<sealed>"), "{unsealed}");
    }

    // ---- Phase 2 final review -------------------------------------------

    #[test]
    fn debug_says_whether_the_body_and_username_are_set_not_what_they_are() {
        let monitor = Monitor {
            request_body: Some("{\"token\":\"body-secret\"}".to_string()),
            auth_username: Some("user-secret".to_string()),
            udp_payload: Some("community=snmp-secret".to_string()),
            ..stored_monitor()
        };
        for printed in [format!("{monitor:?}"), format!("{monitor:#?}")] {
            for secret in ["body-secret", "user-secret", "snmp-secret"] {
                assert!(!printed.contains(secret), "{secret} in {printed}");
            }
            assert!(printed.contains("<set>"), "{printed}");
        }
    }

    #[test]
    fn viewers_read_the_udp_payload_masked() {
        use crate::middleware::rbac::ProjectReader;

        let monitor = Monitor {
            check_type: "udp".to_string(),
            udp_payload: Some("community=SNMP-COMMUNITY-SECRET".to_string()),
            ..Monitor::default()
        };
        let viewer = monitor.clone().for_reader(ProjectReader::Viewer);
        assert_eq!(viewer.udp_payload.as_deref(), Some("***"));
        let member = monitor.clone().for_reader(ProjectReader::Member);
        assert_eq!(member.udp_payload, monitor.udp_payload);
        let none = Monitor::default().for_reader(ProjectReader::Viewer);
        assert_eq!(none.udp_payload, None);
    }

    #[test]
    fn a_view_says_whether_it_was_redacted_for_its_reader() {
        use crate::middleware::rbac::ProjectReader;

        // A member's URL can hold a literal `***`: that is not redaction.
        let monitor = Monitor {
            url: Some("https://example.com/***".to_string()),
            ..stored_monitor()
        };
        let member = MonitorView::for_reader(monitor.clone(), ProjectReader::Member, false);
        assert!(!member.redacted);
        assert_eq!(member.monitor, monitor);
        let viewer = MonitorView::for_reader(monitor.clone(), ProjectReader::Viewer, true);
        assert!(viewer.redacted && viewer.in_maintenance);
        assert_eq!(viewer.monitor.request_body.as_deref(), Some("***"));
        assert!(!MonitorView::new(monitor, false).redacted);
        let text = serde_json::to_string(&viewer).unwrap();
        assert!(text.contains("\"redacted\":true"), "{text}");
    }

    fn mail_patch(
        check_type: &str,
        use_tls: bool,
        port: i32,
        body: serde_json::Value,
    ) -> MonitorConfig {
        let stored = Monitor {
            check_type: check_type.to_string(),
            host: Some("mail.example.com".to_string()),
            port: Some(port),
            use_tls,
            ..stored_monitor()
        };
        validate(patch(body).apply_to(MonitorConfig::from(&stored))).unwrap()
    }

    /// The stored port was the default of the old (type, use_tls): it follows
    /// the new default. A port the user chose, or one the request names, stays.
    #[test]
    fn a_default_mail_port_follows_use_tls_and_the_type() {
        for (check_type, plain, tls) in [("smtp", 25, 465), ("pop3", 110, 995), ("imap", 143, 993)]
        {
            let on = mail_patch(check_type, false, plain, json!({ "use_tls": true }));
            assert_eq!(on.port, Some(tls), "{check_type} on");
            let off = mail_patch(check_type, true, tls, json!({ "use_tls": false }));
            assert_eq!(off.port, Some(plain), "{check_type} off");
        }
        assert_eq!(
            mail_patch("smtp", false, 25, json!({ "check_type": "imap" })).port,
            Some(143)
        );
        assert_eq!(
            mail_patch(
                "smtp",
                false,
                25,
                json!({ "check_type": "imap", "use_tls": true })
            )
            .port,
            Some(993)
        );
        // Chosen ports stay.
        assert_eq!(
            mail_patch("smtp", false, 2525, json!({ "use_tls": true })).port,
            Some(2525)
        );
        assert_eq!(
            mail_patch("smtp", false, 25, json!({ "use_tls": true, "port": 25 })).port,
            Some(25)
        );
        assert_eq!(
            mail_patch("smtp", false, 25, json!({ "name": "x" })).port,
            Some(25)
        );
        // A type that has no default port keeps the stored one.
        let tcp = mail_patch(
            "smtp",
            false,
            25,
            json!({ "check_type": "tcp", "host": "mail.example.com" }),
        );
        assert_eq!(tcp.port, Some(25));
    }

    #[test]
    fn a_dns_monitor_looks_up_a_name_not_an_address() {
        for host in ["169.254.169.254", "10.0.0.1", "::1", "[::1]", "fe80::1"] {
            let error = validate(MonitorConfig {
                check_type: "dns".to_string(),
                host: Some(host.to_string()),
                ..MonitorConfig::from(&stored_monitor())
            })
            .unwrap_err();
            assert!(
                error == "host must be a domain name to look up" || error.contains("host"),
                "{host}: {error}"
            );
        }
        let error = validate(MonitorConfig {
            check_type: "dns".to_string(),
            host: Some("169.254.169.254".to_string()),
            ..MonitorConfig::from(&stored_monitor())
        })
        .unwrap_err();
        assert_eq!(error, "host must be a domain name to look up");
        for host in [
            "2852039166",
            "0xa9.0xfe.0xa9.0xfe",
            "0xa9fea9fe",
            "169.254.169.254.",
            "fe80::1%en0",
            "127.1",
        ] {
            assert!(is_ip_literal(host), "{host}");
            assert!(
                validate(MonitorConfig {
                    check_type: "dns".to_string(),
                    host: Some(host.to_string()),
                    ..MonitorConfig::from(&stored_monitor())
                })
                .unwrap_err()
                .starts_with("host must be"),
                "{host}"
            );
        }
        for host in [
            "example.com",
            "example.com.",
            "_dmarc.example.com",
            "a1.b2",
            "db.internal",
        ] {
            assert!(!is_ip_literal(host), "{host}");
        }
        assert!(
            validate(MonitorConfig {
                check_type: "dns".to_string(),
                host: Some("example.com".to_string()),
                ..MonitorConfig::from(&stored_monitor())
            })
            .is_ok()
        );
    }

    // ---- advanced request options (NAMI-UP-23) -------------------------------

    fn proxied_monitor() -> Monitor {
        Monitor {
            proxy_url: Some("http://proxy.internal:3128".to_string()),
            proxy_username: Some("nami".to_string()),
            proxy_password_enc: Some(vec![7, 8, 9]),
            ..stored_monitor()
        }
    }

    fn merged_proxied(body: serde_json::Value) -> MonitorConfig {
        patch(body).apply_to(MonitorConfig::from(&proxied_monitor()))
    }

    #[test]
    fn ip_version_is_kept_for_the_types_that_connect_and_reset_for_the_others() {
        let tcp = json!({ "check_type": "tcp", "host": "db.internal", "port": 5432 });
        let ping = json!({ "check_type": "ping", "host": "db.internal" });
        let ssl = json!({ "check_type": "ssl_expiry", "host": "example.com",
            "interval_seconds": 86400 });
        for version in ["ipv4", "ipv6", "any"] {
            for mut body in [
                json!({}),
                json!({ "check_type": "keyword", "keyword": "ok",
                "http_method": "GET", "request_body": null }),
                tcp.clone(),
                ping.clone(),
                ssl.clone(),
            ] {
                body["ip_version"] = json!(version);
                assert_eq!(
                    validate(merged(body.clone())).unwrap().ip_version,
                    version,
                    "{body}"
                );
            }
        }
        for mut body in [
            json!({ "check_type": "dns", "host": "example.com" }),
            json!({ "check_type": "domain_expiry", "host": "example.com", "interval_seconds": 86400 }),
            json!({ "check_type": "smtp", "host": "mail.example.com" }),
            json!({ "check_type": "pop3", "host": "mail.example.com" }),
            json!({ "check_type": "imap", "host": "mail.example.com" }),
            json!({ "check_type": "udp", "host": "example.com", "port": 53, "udp_payload": "x" }),
        ] {
            body["ip_version"] = json!("ipv6");
            assert_eq!(
                validate(merged(body.clone())).unwrap().ip_version,
                "any",
                "{body}"
            );
        }
        for wrong in ["IPv4", "v4", "", "ipv5"] {
            assert_eq!(
                validate(merged(json!({ "ip_version": wrong }))).unwrap_err(),
                "ip_version must be any, ipv4 or ipv6",
                "{wrong}"
            );
        }
        let create: CreateMonitorRequest = serde_json::from_value(json!({
            "name": "n", "check_type": "http", "url": "https://example.com",
        }))
        .unwrap();
        let config = create.into_config();
        assert_eq!(config.ip_version, "any");
        assert!(!config.remember_cookies);
        assert_eq!(
            config.proxy_password,
            PasswordChange::Keep { present: false }
        );
    }

    #[test]
    fn a_stored_ip_version_reads_as_its_family_and_unknown_text_as_any() {
        for (text, version) in [
            ("any", IpVersion::Any),
            ("ipv4", IpVersion::V4),
            ("ipv6", IpVersion::V6),
            ("bogus", IpVersion::Any),
        ] {
            let monitor = Monitor {
                ip_version: text.to_string(),
                ..Monitor::default()
            };
            assert_eq!(monitor.ip_version(), version, "{text}");
        }
        assert_eq!(IpVersion::default(), IpVersion::Any);
        assert_eq!(IpVersion::parse("IPv4"), None);
    }

    #[test]
    fn remember_cookies_belongs_to_http_and_keyword_only() {
        let ok = |body: serde_json::Value| validate(merged(body)).unwrap();
        assert!(ok(json!({ "remember_cookies": true })).remember_cookies);
        assert!(
            ok(
                json!({ "remember_cookies": true, "check_type": "keyword", "keyword": "ok",
            "http_method": "GET", "request_body": null })
            )
            .remember_cookies
        );
        assert!(
            !ok(json!({ "remember_cookies": true, "check_type": "tcp",
            "host": "db.internal", "port": 5432 }))
            .remember_cookies
        );
    }

    #[test]
    fn proxy_settings_need_their_url_and_username_in_order() {
        assert_eq!(
            validate(merged(json!({ "proxy_username": "nami" }))).unwrap_err(),
            "proxy_username requires proxy_url"
        );
        assert_eq!(
            validate(merged(
                json!({ "proxy_url": "http://proxy:3128", "proxy_password": "p" })
            ))
            .unwrap_err(),
            "proxy_password requires proxy_username"
        );
        // A username named next to the removal is still a username without a proxy.
        assert_eq!(
            validate(merged_proxied(
                json!({ "proxy_url": null, "proxy_username": "other" })
            ))
            .unwrap_err(),
            "proxy_username requires proxy_url"
        );
        let full = validate(merged(json!({
            "proxy_url": " http://proxy:3128 ", "proxy_username": "nami", "proxy_password": "p",
        })))
        .unwrap();
        assert_eq!(full.proxy_url.as_deref(), Some("http://proxy:3128"));
        assert_eq!(full.proxy_username.as_deref(), Some("nami"));
        assert_eq!(
            full.proxy_password,
            PasswordChange::Replace(SecretString::new("p"))
        );

        // Removing the url removes the username and the stored password.
        for removal in [json!({ "proxy_url": null }), json!({ "proxy_url": "" })] {
            let removed = validate(merged_proxied(removal.clone())).unwrap();
            assert_eq!(removed.proxy_url, None, "{removal}");
            assert_eq!(removed.proxy_username, None, "{removal}");
            assert_eq!(removed.proxy_password, PasswordChange::Clear, "{removal}");
        }
        // Removing the username removes the password, the url stays.
        let no_user = validate(merged_proxied(json!({ "proxy_username": null }))).unwrap();
        assert_eq!(
            no_user.proxy_url.as_deref(),
            Some("http://proxy.internal:3128")
        );
        assert_eq!(no_user.proxy_password, PasswordChange::Clear);
        // An empty patch keeps all three as they are.
        let kept = validate(merged_proxied(json!({}))).unwrap();
        assert_eq!(kept.proxy_password, PasswordChange::Keep { present: true });
        assert_eq!(kept.proxy_username.as_deref(), Some("nami"));
    }

    #[test]
    fn a_non_http_monitor_carries_no_proxy() {
        let tcp = validate(merged_proxied(json!({
            "check_type": "tcp", "host": "db.internal", "port": 5432,
        })))
        .unwrap();
        assert_eq!((tcp.proxy_url, tcp.proxy_username), (None, None));
        assert_eq!(tcp.proxy_password, PasswordChange::Clear);
        // Nothing stored, nothing to clear.
        let plain = validate(merged(json!({
            "check_type": "tcp", "host": "db.internal", "port": 5432,
        })))
        .unwrap();
        assert_eq!(
            plain.proxy_password,
            PasswordChange::Keep { present: false }
        );
    }

    #[test]
    fn ip_version_cannot_be_combined_with_a_proxy() {
        for version in ["ipv4", "ipv6"] {
            assert_eq!(
                validate(merged_proxied(json!({ "ip_version": version }))).unwrap_err(),
                "ip_version cannot be combined with a proxy: the proxy resolves the target"
            );
        }
        assert!(validate(merged_proxied(json!({ "ip_version": "any" }))).is_ok());
        assert_eq!(
            validate(merged_proxied(
                json!({ "ip_version": "ipv6", "proxy_url": null })
            ))
            .unwrap()
            .ip_version,
            "ipv6"
        );
    }

    #[test]
    fn proxy_username_and_password_are_checked_without_echoing_the_password() {
        let with = |username: &str, password: &str| {
            validate(merged(json!({
                "proxy_url": "http://proxy:3128",
                "proxy_username": username,
                "proxy_password": password,
            })))
        };
        assert_eq!(
            with("a:b", "p").unwrap_err(),
            "proxy_username must not contain ':'"
        );
        assert!(with(&"u".repeat(257), "p").is_err());
        assert!(with("a\u{1}b", "p").is_err());
        let long = format!("needle-{}", "p".repeat(1020));
        let error = with("nami", &long).unwrap_err();
        assert_eq!(error, "proxy_password must be at most 1024 bytes");
        assert!(!error.contains("needle"));
        let error = with("nami", "needle\n").unwrap_err();
        assert!(!error.contains("needle"), "{error}");
    }

    #[test]
    fn every_advanced_option_reschedules_the_check() {
        let stored = proxied_monitor();
        let reschedules = |body: serde_json::Value| {
            validate(patch(body).apply_to(MonitorConfig::from(&stored)))
                .unwrap()
                .changes_check_of(&stored)
        };
        for body in [
            json!({}),
            json!({ "ip_version": "any" }),
            json!({ "remember_cookies": false }),
            json!({ "proxy_url": "http://proxy.internal:3128", "proxy_username": "nami" }),
        ] {
            assert!(!reschedules(body.clone()), "{body}");
        }
        for body in [
            json!({ "remember_cookies": true }),
            json!({ "proxy_url": "http://proxy.internal:3129" }),
            json!({ "proxy_username": "other" }),
            json!({ "proxy_password": "new" }),
            json!({ "proxy_password": null }),
            json!({ "proxy_url": null }),
        ] {
            assert!(reschedules(body.clone()), "{body}");
        }
        let plain = stored_monitor();
        assert!(
            validate(patch(json!({ "ip_version": "ipv4" })).apply_to(MonitorConfig::from(&plain)))
                .unwrap()
                .changes_check_of(&plain)
        );
    }

    #[test]
    fn viewers_read_the_proxy_username_masked_and_the_proxy_url_redacted() {
        use crate::middleware::rbac::ProjectReader;

        let monitor = proxied_monitor();
        let viewer = MonitorView::for_reader(monitor.clone(), ProjectReader::Viewer, false);
        assert!(viewer.redacted && viewer.has_proxy_password);
        assert_eq!(viewer.monitor.proxy_username.as_deref(), Some("***"));
        assert_eq!(
            viewer.monitor.proxy_url.as_deref(),
            Some("http://proxy.internal:3128"),
            "a valid proxy url has nothing to redact"
        );
        // A row written around the API: redact_url still masks it.
        let odd = Monitor {
            proxy_url: Some("http://u:pw@proxy:3128/?k=v".to_string()),
            ..monitor.clone()
        };
        assert_eq!(
            odd.for_reader(ProjectReader::Viewer).proxy_url.as_deref(),
            Some("http://u:***@proxy:3128/?k=***")
        );
        let member = MonitorView::for_reader(monitor.clone(), ProjectReader::Member, false);
        assert_eq!(member.monitor, monitor);
        let text = serde_json::to_string(&member).unwrap();
        assert!(text.contains("\"has_proxy_password\":true"), "{text}");
        assert!(
            !text.contains("proxy_password_enc") && !text.contains("[7,8,9]"),
            "{text}"
        );
        let none = MonitorView::new(stored_monitor(), false);
        assert!(!none.has_proxy_password);
    }

    #[test]
    fn debug_hides_the_proxy_credentials() {
        let monitor = Monitor {
            proxy_username: Some("proxy-user-secret".to_string()),
            request_headers_enc: None,
            auth_username: None,
            auth_password_enc: None,
            request_body: None,
            ..proxied_monitor()
        };
        let compact = format!("{monitor:?}");
        assert!(
            compact.contains("proxy_password_enc: Some(\"<sealed>\")"),
            "{compact}"
        );
        assert!(
            compact.contains("proxy_username: Some(\"<set>\")"),
            "{compact}"
        );
        for printed in [format!("{monitor:?}"), format!("{monitor:#?}")] {
            assert!(!printed.contains("proxy-user-secret"), "{printed}");
            assert!(!printed.contains("[7, 8, 9]"), "{printed}");
            assert!(
                printed.contains("<sealed>") && printed.contains("<set>"),
                "{printed}"
            );
            assert!(printed.contains("http://proxy.internal:3128"), "{printed}");
            assert!(printed.contains("ip_version") && printed.contains("remember_cookies"));
        }
    }

    #[test]
    fn moving_the_proxy_requires_every_stored_secret_again() {
        let current = proxied_monitor();
        let check = |body: serde_json::Value| {
            let config = validate(patch(body).apply_to(MonitorConfig::from(&current))).unwrap();
            secrets_follow_the_proxy(&current, &config)
        };
        let refusal = "changing the proxy requires entering the header values and passwords again, or clearing them";
        for body in [
            json!({ "proxy_url": "http://proxy.internal:3129" }),
            json!({ "proxy_url": "https://proxy.internal:3128" }),
            json!({ "proxy_url": "http://other:3128" }),
            json!({ "proxy_url": "http://other:3128", "proxy_password": "new" }),
            json!({ "proxy_url": "http://other:3128", "proxy_password": "new",
                "request_headers": null }),
            json!({ "proxy_url": "http://other:3128", "proxy_password": "new",
                "auth_password": null }),
        ] {
            assert_eq!(check(body.clone()).unwrap_err(), refusal, "{body}");
        }
        for body in [
            json!({}),
            json!({ "name": "renamed" }),
            // Same origin, spelled with the trailing slash.
            json!({ "proxy_url": "http://proxy.internal:3128/" }),
            json!({ "proxy_url": null }),
            json!({ "proxy_url": "http://other:3128", "proxy_password": "new",
                "request_headers": null, "auth_password": "again" }),
            json!({ "proxy_url": "http://other:3128", "proxy_password": null,
                "request_headers": [{ "name": "X-Api-Key", "value": "again" }],
                "auth_username": null }),
            // The target's origin alone: the proxy password stays (the target
            // rule asks for the target's own secrets).
            json!({ "url": "https://example.org/" }),
        ] {
            assert!(check(body.clone()).is_ok(), "{body}");
        }
        // Adding a proxy counts as moving it.
        let unproxied = stored_monitor();
        let config = validate(
            patch(json!({ "proxy_url": "http://127.0.0.1:3128" }))
                .apply_to(MonitorConfig::from(&unproxied)),
        )
        .unwrap();
        assert_eq!(
            secrets_follow_the_proxy(&unproxied, &config).unwrap_err(),
            refusal
        );
        // Without stored secrets there is nothing to protect.
        let bare = Monitor {
            request_headers_enc: None,
            request_header_names: Vec::new(),
            auth_username: None,
            auth_password_enc: None,
            ..stored_monitor()
        };
        let config = validate(
            patch(json!({ "proxy_url": "http://127.0.0.1:3128" }))
                .apply_to(MonitorConfig::from(&bare)),
        )
        .unwrap();
        assert!(secrets_follow_the_proxy(&bare, &config).is_ok());
    }
}
