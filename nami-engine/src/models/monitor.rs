use std::net::Ipv6Addr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::{is_invisible_or_bidi_char, validate_name};
use crate::{error::AppError, middleware::rbac::ProjectReader};

/// Size caps for the free-text target fields. Without them only the 2 MiB
/// request-body limit bounds what is stored, returned to every viewer and
/// put in alert emails.
const MAX_URL_BYTES: usize = 2048;
const MAX_HOST_CHARS: usize = 253;
const MAX_KEYWORD_BYTES: usize = 1024;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow, Serialize)]
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
}

/// What replaces each secret part of a URL a viewer reads (see
/// [`redact_url`]).
const REDACTED: &str = "***";

impl Monitor {
    /// The monitor as `reader` may see it: whole for a member or above; for a
    /// viewer, with the secrets of its URL masked (see [`redact_url`]).
    pub fn for_reader(mut self, reader: ProjectReader) -> Self {
        if reader == ProjectReader::Viewer {
            self.url = self.url.as_deref().map(redact_url);
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
        }
    }
}

impl MonitorConfig {
    /// True when a field that changes how or when the monitor is checked
    /// differs from `current`. Renaming, pausing and the confirmation
    /// threshold do not count.
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
        }
    }
}

/// `PATCH` body: any subset of the editable fields. Absent fields keep
/// their current value; unknown and server-owned fields are ignored.
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
        }
    }
}

/// Fail-closed validation of a complete config. PATCH validates the merged
/// result, so every rule here also holds for edits. Returns the normalized
/// config (trimmed name/url/host; fields irrelevant to the check type
/// cleared) or a message for a 400.
pub fn validate(mut c: MonitorConfig) -> Result<MonitorConfig, String> {
    c.name = validate_name(&c.name)?;
    if !matches!(c.http_method.as_str(), "GET" | "HEAD") {
        return Err("http_method must be GET or HEAD".into());
    }
    if !matches!(c.keyword_mode.as_str(), "contains" | "not_contains") {
        return Err("keyword_mode must be contains or not_contains".into());
    }
    match c.check_type.as_str() {
        "http" | "keyword" => {
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
            let parsed = reqwest::Url::parse(&url)
                .map_err(|_| "url must be a valid http(s) URL".to_string())?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err("url must be a valid http(s) URL".into());
            }
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
                if c.http_method != "GET" {
                    return Err(
                        "keyword monitors must use GET (HEAD responses have no body)".into(),
                    );
                }
            } else {
                c.keyword = None;
            }
        }
        "tcp" => {
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
            if host.chars().any(|ch| {
                ch.is_control() || ch.is_whitespace() || matches!(ch, '/' | '?' | '#' | '@')
            }) {
                return Err(
                    "host must not contain whitespace, control characters or any of / ? # @".into(),
                );
            }
            // The port has its own field; a colon is only valid inside an
            // IPv6 literal. Without this, `db:5432` is stored and then
            // reports DOWN forever because no such name resolves.
            if host.contains(':') && host.parse::<Ipv6Addr>().is_err() {
                return Err(
                    "host must be a hostname or IP address; put the port in the port field".into(),
                );
            }
            if !c.port.is_some_and(|p| (1..=65535).contains(&p)) {
                return Err("port must be between 1 and 65535".into());
            }
            c.host = Some(host);
            c.url = None;
            c.keyword = None;
        }
        _ => return Err("check_type must be http, keyword or tcp".into()),
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

fn has_invisible_or_bidi_chars(text: &str) -> bool {
    text.chars().any(is_invisible_or_bidi_char)
}

pub async fn insert_monitor(
    pool: &PgPool,
    project_id: Uuid,
    c: &MonitorConfig,
) -> Result<Monitor, sqlx::Error> {
    sqlx::query_as(
        "insert into monitors (project_id, name, check_type, url, host, port, http_method,
             keyword, keyword_mode, expected_status_min, expected_status_max,
             interval_seconds, timeout_seconds, confirmation_threshold, paused)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         returning *",
    )
    .bind(project_id)
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
    .fetch_one(pool)
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
    let updated: Monitor = sqlx::query_as(
        "update monitors set
             name = $3, check_type = $4, url = $5, host = $6, port = $7, http_method = $8,
             keyword = $9, keyword_mode = $10, expected_status_min = $11,
             expected_status_max = $12, interval_seconds = $13, timeout_seconds = $14,
             confirmation_threshold = $15, paused = $16,
             next_check_at = case when $17 then now() else next_check_at end
         where id = $1 and project_id = $2
         returning *",
    )
    .bind(monitor_id)
    .bind(project_id)
    .bind(&config.name)
    .bind(&config.check_type)
    .bind(&config.url)
    .bind(&config.host)
    .bind(config.port)
    .bind(&config.http_method)
    .bind(&config.keyword)
    .bind(&config.keyword_mode)
    .bind(config.expected_status_min)
    .bind(config.expected_status_max)
    .bind(config.interval_seconds)
    .bind(config.timeout_seconds)
    .bind(config.confirmation_threshold)
    .bind(config.paused)
    .bind(reschedule)
    .fetch_one(&mut *tx)
    .await?;
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
    use super::redact_url;

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
}
