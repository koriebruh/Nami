use std::{fmt, net::Ipv6Addr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::{
    is_invisible_or_bidi_char,
    monitor_request::{
        self, BODY_METHODS, HTTP_METHODS, HeaderView, HeadersChange, MAX_BODY_BYTES,
        PasswordChange, RequestHeader, seal_headers, seal_password,
    },
    nullable, validate_name,
};
use crate::{
    crypto::{Cipher, SecretString},
    error::AppError,
    middleware::rbac::ProjectReader,
};

/// Size caps for the free-text target fields. Without them only the 2 MiB
/// request-body limit bounds what is stored, returned to every viewer and
/// put in alert emails.
const MAX_URL_BYTES: usize = 2048;
const MAX_HOST_CHARS: usize = 253;
const MAX_KEYWORD_BYTES: usize = 1024;

/// One `monitors` row. The `*_enc` columns and `request_header_names` are
/// never serialized: API responses go through [`MonitorView`]. `Debug` leaves
/// the sealed columns out too. `Default` exists for tests that build monitors
/// in memory (`..Monitor::default()`); real monitors always come from the
/// database.
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
}

/// What `Debug` prints in place of a sealed column that holds a value.
const SEALED: &str = "<sealed>";

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
            .field("request_body", request_body)
            .field(
                "request_headers_enc",
                &request_headers_enc.as_ref().map(|_| SEALED),
            )
            .field("request_header_names", request_header_names)
            .field("auth_username", auth_username)
            .field(
                "auth_password_enc",
                &auth_password_enc.as_ref().map(|_| SEALED),
            )
            .field("follow_redirects", follow_redirects)
            .field("verify_ssl", verify_ssl)
            .finish()
    }
}

/// A monitor as every API response shows it: the secrets are reduced to
/// `request_headers` (names, `value_set: true`) and `has_auth_password`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MonitorView {
    #[serde(flatten)]
    pub monitor: Monitor,
    pub request_headers: Vec<HeaderView>,
    pub has_auth_password: bool,
}

impl From<Monitor> for MonitorView {
    fn from(monitor: Monitor) -> Self {
        let request_headers = monitor
            .request_header_names
            .iter()
            .map(|name| HeaderView {
                name: name.clone(),
                value_set: true,
            })
            .collect();
        let has_auth_password = monitor.auth_password_enc.is_some();
        MonitorView {
            monitor,
            request_headers,
            has_auth_password,
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
    /// [`redact_url`]) and its request body and basic-auth username hidden:
    /// both carry credentials as often as a URL does, so a viewer learns that
    /// they are set, not what they are.
    pub fn for_reader(mut self, reader: ProjectReader) -> Self {
        if reader == ProjectReader::Viewer {
            self.url = self.url.as_deref().map(redact_url);
            self.request_body = self.request_body.map(|_| REDACTED.to_string());
            self.auth_username = self.auth_username.map(|_| REDACTED.to_string());
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
        }
    }
}

impl MonitorConfig {
    /// True when a field that changes how or when the monitor is checked
    /// differs from `current`. Renaming, pausing and the confirmation
    /// threshold do not count; replacing or clearing a secret does (a
    /// secret cannot be compared with its ciphertext).
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
    #[serde(default = "default_interval_seconds")]
    pub interval_seconds: i32,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: i32,
    #[serde(default = "default_confirmation_threshold")]
    pub confirmation_threshold: i32,
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
fn default_interval_seconds() -> i32 {
    60
}
fn default_timeout_seconds() -> i32 {
    10
}
fn default_confirmation_threshold() -> i32 {
    2
}
fn default_true() -> bool {
    true
}

impl CreateMonitorRequest {
    pub fn into_config(self) -> MonitorConfig {
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
            interval_seconds: self.interval_seconds,
            timeout_seconds: self.timeout_seconds,
            confirmation_threshold: self.confirmation_threshold,
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
        MonitorConfig {
            name: self.name.unwrap_or(base.name),
            check_type: self.check_type.unwrap_or(base.check_type),
            url: self.url.or(base.url),
            host: self.host.or(base.host),
            port: self.port.or(base.port),
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
    match c.check_type.as_str() {
        "http" | "keyword" => validate_http(&mut c)?,
        "tcp" => validate_tcp(&mut c)?,
        _ => return Err("check_type must be http, keyword or tcp".into()),
    }
    if !matches!(c.check_type.as_str(), "http" | "keyword") {
        clear_request_config(&mut c);
    }
    if c.interval_seconds < 30 {
        return Err("interval_seconds must be at least 30".into());
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
    c.host = None;
    c.port = None;
    if c.check_type == "keyword" {
        let keyword = c.keyword.as_deref().unwrap_or_default();
        if keyword.trim().is_empty() {
            return Err("keyword is required for keyword monitors".into());
        }
        if keyword.len() > MAX_KEYWORD_BYTES {
            return Err(format!("keyword must be at most {MAX_KEYWORD_BYTES} bytes"));
        }
        if keyword.chars().any(char::is_control) {
            return Err("keyword must not contain control characters".into());
        }
        // Any other method has a body to search; a HEAD response has none.
        if c.http_method == "HEAD" {
            return Err(
                "keyword monitors cannot use HEAD: a HEAD response has no body to search (use GET)"
                    .into(),
            );
        }
    } else {
        c.keyword = None;
    }
    validate_request_config(c, credentials_in_url)
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
    Ok(())
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
}

fn validate_tcp(c: &mut MonitorConfig) -> Result<(), String> {
    if c.host.as_deref().is_some_and(has_invisible_or_bidi_chars) {
        return Err("host must not contain invisible or text-direction characters".into());
    }
    let host = c
        .host
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if host.is_empty() {
        return Err("host is required for tcp monitors".into());
    }
    if host.chars().count() > MAX_HOST_CHARS {
        return Err(format!("host must be at most {MAX_HOST_CHARS} characters"));
    }
    // A bare hostname or IP literal: a scheme, path, query, fragment
    // or credentials (`tcp://db/x`, `user@db`) can never be connected to.
    if host
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '@'))
    {
        return Err(
            "host must not contain whitespace, control characters or any of / ? # @".into(),
        );
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
    c.url = None;
    c.keyword = None;
    Ok(())
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
    let monitor = write_config(&mut tx, cipher, id, c, false).await?;
    tx.commit().await?;
    Ok(monitor)
}

/// Writes every configurable column of monitor `id` from a validated
/// config, sealing new secrets for that monitor.
/// `HeadersChange::Keep`/`PasswordChange::Keep` leave the stored ciphertext
/// untouched; `reschedule` makes the monitor due now. New columns are
/// appended at the end of the statement.
async fn write_config(
    conn: &mut PgConnection,
    cipher: &Cipher,
    id: Uuid,
    c: &MonitorConfig,
    reschedule: bool,
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
    sqlx::query_as(
        "update monitors set
             next_check_at = case when $2 then now() else next_check_at end,
             name = $3, check_type = $4, url = $5, host = $6, port = $7, http_method = $8,
             keyword = $9, keyword_mode = $10, expected_status_min = $11, expected_status_max = $12,
             interval_seconds = $13, timeout_seconds = $14, confirmation_threshold = $15, paused = $16,
             request_body = $17, auth_username = $18, follow_redirects = $19, verify_ssl = $20,
             request_header_names = case when $21 then request_header_names else $22 end,
             request_headers_enc = case when $21 then request_headers_enc else $23 end,
             auth_password_enc = case when $24 then auth_password_enc else $25 end
         where id = $1
         returning *",
    )
    .bind(id)
    .bind(reschedule)
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
    let config =
        validate(patch.apply_to(MonitorConfig::from(&current))).map_err(AppError::Validation)?;
    let reschedule = config.changes_check_of(&current);
    let updated = write_config(&mut tx, cipher, monitor_id, &config, reschedule).await?;
    tx.commit().await?;
    Ok(Some(updated))
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
        let view = MonitorView::from(stored_monitor());
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

        let none = MonitorView::from(Monitor {
            request_headers_enc: None,
            request_header_names: Vec::new(),
            auth_password_enc: None,
            ..stored_monitor()
        });
        assert!(none.request_headers.is_empty() && !none.has_auth_password);
    }

    #[test]
    fn debug_leaves_the_sealed_columns_out() {
        let monitor = stored_monitor();
        for printed in [format!("{monitor:?}"), format!("{monitor:#?}")] {
            assert!(printed.contains("<sealed>"), "{printed}");
            assert!(!printed.contains("[1, 2, 3]") && !printed.contains("[4, 5, 6]"));
            assert!(printed.contains("Orders") && printed.contains("X-Api-Key"));
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
}
