//! Monitor import: parsers that turn a pasted URL list, an Uptime Kuma backup
//! or an UptimeRobot CSV export into `POST /projects/{id}/monitors` bodies.
//!
//! Pure functions, no I/O. A parser never validates monitors itself: every
//! body is judged later by `uptime::monitor::model::validate`, the same code a
//! hand-made monitor goes through (so invisible characters in names, private
//! URLs and the like are refused there). Secrets found in a source (headers,
//! basic-auth passwords) are carried in the bodies, in memory only; error text
//! never quotes a source value beyond a short `printable` snippet of a type
//! name.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::PgPool;
use std::{collections::HashSet, fmt};
use uuid::Uuid;

use crate::{
    core::crypto::Cipher,
    core::error::AppError,
    uptime::group::model as monitor_group,
    uptime::monitor::model::{self as monitor, CreateMonitorRequest, MonitorConfig},
};

pub mod kuma;
pub mod uptimerobot;
pub mod urls;

/// Most entries one import may hold (counted before any mapping).
pub const MAX_ENTRIES: usize = 500;
/// Largest import payload, in bytes. The route caps its request body with
/// this; `parse` re-checks it so the bound holds for every caller.
pub const MAX_IMPORT_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Largest `POST .../monitors/import` request body: twice the content cap,
/// because `content` travels as a JSON string and every `"`, `\` and newline
/// of it is escaped to two bytes (a pasted Kuma backup is full of them), plus
/// 64 KiB for the other fields. A body within this whose `content` is still
/// over [`MAX_IMPORT_BODY_BYTES`] is a 413 from the handler, so the content
/// limit is the same whatever the escaping.
pub const MAX_IMPORT_REQUEST_BYTES: usize = 2 * MAX_IMPORT_BODY_BYTES + 64 * 1024;

/// Advisory-lock class of "an import is running in this project" (the second
/// key is `hashtext(project_id::text)`). No other advisory lock exists in the
/// engine; a new one takes another class. The value is "nami" in ASCII.
pub const IMPORT_LOCK_CLASS: i32 = 0x6e61_6d69;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFormat {
    Urls,
    UptimeKuma,
    Uptimerobot,
}

/// Values for what a source leaves out; `group_id` is always applied.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ImportDefaults {
    pub interval_seconds: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub confirmation_threshold: Option<i32>,
    pub recovery_threshold: Option<i32>,
    pub group_id: Option<Uuid>,
}

/// One source entry: its 1-based line (URL list, CSV) or index (Kuma), and
/// either a create body or why it cannot be one.
#[derive(Clone, PartialEq)]
pub struct ParsedEntry {
    pub line: usize,
    pub body: Result<Map<String, Value>, String>,
    /// Something the source said that the body could not carry (Kuma's extra
    /// accepted status ranges). Never quotes a source value.
    pub warning: Option<String>,
}

/// What `Debug` prints in place of a secret that is set.
const MASKED: &str = "<set>";

impl fmt::Debug for ParsedEntry {
    /// Header values and the basic-auth password are masked, so a stray
    /// `{:?}` in a log line cannot leak them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let body = self.body.as_ref().map(|body| {
            let mut body = body.clone();
            if let Some(password) = body.get_mut("auth_password") {
                *password = Value::from(MASKED);
            }
            if let Some(Value::Array(headers)) = body.get_mut("request_headers") {
                for header in headers {
                    if let Some(value) = header.get_mut("value") {
                        *value = Value::from(MASKED);
                    }
                }
            }
            body
        });
        f.debug_struct("ParsedEntry")
            .field("line", &self.line)
            .field("body", &body)
            .field("warning", &self.warning)
            .finish()
    }
}

/// `checker::printable` plus the invisible and text-direction characters the
/// monitor validators reject, for source text quoted in error messages.
pub(crate) fn printable(text: &str, max_chars: usize) -> String {
    let visible: String = text
        .chars()
        .filter(|c| !crate::core::validate::is_invisible_or_bidi_char(*c))
        .collect();
    crate::uptime::checker::printable(&visible, max_chars)
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    TooManyEntries(usize),
    Malformed(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::TooManyEntries(n) => {
                write!(f, "at most {MAX_ENTRIES} entries per import, got {n}")
            }
            ParseError::Malformed(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ParseError {}

pub fn parse(
    format: ImportFormat,
    content: &str,
    defaults: &ImportDefaults,
) -> Result<Vec<ParsedEntry>, ParseError> {
    if content.len() > MAX_IMPORT_BODY_BYTES {
        return Err(ParseError::Malformed(format!(
            "content is larger than {} MiB",
            MAX_IMPORT_BODY_BYTES / (1024 * 1024)
        )));
    }
    let mut entries = match format {
        ImportFormat::Urls => urls::parse(content)?,
        ImportFormat::UptimeKuma => kuma::parse(content)?,
        ImportFormat::Uptimerobot => uptimerobot::parse(content)?,
    };
    for entry in &mut entries {
        let Ok(body) = &mut entry.body else { continue };
        for (key, value) in [
            ("interval_seconds", defaults.interval_seconds),
            ("timeout_seconds", defaults.timeout_seconds),
            ("confirmation_threshold", defaults.confirmation_threshold),
            ("recovery_threshold", defaults.recovery_threshold),
        ] {
            if let Some(value) = value {
                body.entry(key).or_insert(Value::from(value));
            }
        }
        if let Some(group) = defaults.group_id {
            body.insert("group_id".into(), Value::from(group.to_string()));
        }
    }
    Ok(entries)
}

/// `POST /projects/{id}/monitors/import` body.
#[derive(Deserialize)]
pub struct ImportRequest {
    pub format: ImportFormat,
    pub content: String,
    #[serde(default)]
    pub defaults: ImportDefaults,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_true")]
    pub skip_existing: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum EntryStatus {
    Created,
    Skipped,
    Error,
}

/// One entry's outcome. `name` and `target` are only set once the entry has
/// passed validation (never raw source text); `target` is the URL through
/// `redact_url` or `host[:port]`. No header value, password or body appears.
#[derive(Debug, Serialize)]
pub struct ImportResultRow {
    pub line: usize,
    pub status: EntryStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Set on created entries whose source said more than a monitor holds
    /// (see `ParsedEntry::warning`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

impl ImportResultRow {
    fn error(line: usize, message: impl Into<String>) -> Self {
        ImportResultRow {
            line,
            status: EntryStatus::Error,
            name: None,
            target: None,
            monitor_id: None,
            error: Some(message.into()),
            warning: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub created: usize,
    pub skipped: usize,
    pub errors: usize,
    pub results: Vec<ImportResultRow>,
}

impl ImportReport {
    fn push(&mut self, row: ImportResultRow) {
        match row.status {
            EntryStatus::Created => self.created += 1,
            EntryStatus::Skipped => self.skipped += 1,
            EntryStatus::Error => self.errors += 1,
        }
        self.results.push(row);
    }
}

/// What a body that is not a `CreateMonitorRequest` reports. serde's own text
/// quotes the offending value (`invalid type: string "…", expected i32`),
/// which may be a secret, so it is never shown.
pub const UNREADABLE_ENTRY: &str =
    "entry could not be read as a monitor: a number is out of range or a field has the wrong type";
/// What an entry the database refused reports (a constraint validation does
/// not know of). Logged with its SQLSTATE, never with values.
const UNSTORABLE_ENTRY: &str = "entry could not be stored";
const ALREADY_MONITORED: &str = "already monitored";
const BUSY: &str = "another import is running in this project; try again when it finishes";

/// The `skip_existing` key: the check type and the normalized target, plus
/// what makes two monitors of one target different checks (controller
/// rulings). So a ping and a dns monitor of one host, an http and a keyword
/// monitor of one URL, dns monitors of different record types and keyword
/// monitors of different keywords or modes are not duplicates.
///
/// - http: `http {url}`
/// - keyword: `keyword {keyword_mode} {keyword} {url}`
/// - dns: `dns {record_type} {host}`
/// - others: `{check_type} {host}[:{port}]`
///
/// URLs are the validated string, hosts are lowercased. Parts are separated
/// by a space; only the keyword may contain one, and the URL after it never
/// does, so two different keys cannot collide.
pub fn existing_key(c: &MonitorConfig) -> String {
    let url = c.url.as_deref().unwrap_or_default();
    let host = c.host.as_deref().unwrap_or_default().to_lowercase();
    match c.check_type.as_str() {
        "http" => format!("http {url}"),
        "keyword" => format!(
            "keyword {} {} {url}",
            c.keyword_mode,
            c.keyword.as_deref().unwrap_or_default()
        ),
        "dns" => format!("dns {} {host}", c.dns_record_type.as_deref().unwrap_or("A")),
        other => match c.port {
            Some(port) => format!("{other} {host}:{port}"),
            None => format!("{other} {host}"),
        },
    }
}

/// What a result row shows of a validated monitor's target.
fn target_of(c: &MonitorConfig) -> Option<String> {
    if let Some(url) = &c.url {
        return Some(monitor::redact_url(url));
    }
    let host = c.host.as_deref()?;
    Some(match c.port {
        Some(port) if host.contains(':') => format!("[{host}]:{port}"),
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Runs an import for `project_id` (the caller checked the member role).
///
/// Every entry goes through the single-create path: `CreateMonitorRequest`
/// deserialization, `into_config`, `monitor::validate` and
/// `monitor::insert_monitor` (secrets sealed for that row, due at once), each
/// in its own transaction, so one bad entry never rolls back the others and a
/// failure halfway keeps what was created before it. Whole-request problems
/// (foreign group, too many entries, malformed document, another import
/// running) fail before anything is created. Dry runs create nothing and take
/// no lock.
pub async fn run_import(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    request: ImportRequest,
) -> Result<ImportReport, AppError> {
    // A global admin bypasses membership, so the project itself may not
    // exist: 404 before anything else, as a single create answers.
    if crate::projects::model::find_by_id(pool, project_id)
        .await?
        .is_none()
    {
        return Err(AppError::NotFound);
    }
    if let Some(group) = request.defaults.group_id
        && !monitor_group::belongs_to(pool, project_id, group).await?
    {
        // A group of another project is a 404, like any foreign id.
        return Err(AppError::NotFound);
    }
    let entries = parse(request.format, &request.content, &request.defaults)
        .map_err(|e| AppError::Validation(e.to_string()))?;
    drop(request.content);
    // One import at a time per project: without it two concurrent requests (a
    // double click) would both load `seen` before either inserted and create
    // every monitor twice. Transaction-scoped, so it is released when `lock`
    // is rolled back or dropped, on every path.
    let lock = if request.dry_run {
        None
    } else {
        let mut lock = pool.begin().await?;
        let taken: bool =
            sqlx::query_scalar("select pg_try_advisory_xact_lock($1, hashtext($2::text))")
                .bind(IMPORT_LOCK_CLASS)
                .bind(project_id)
                .fetch_one(&mut *lock)
                .await?;
        if !taken {
            return Err(AppError::Conflict(BUSY.into()));
        }
        Some(lock)
    };
    // Loaded after the lock, so a finished concurrent import is seen.
    let mut seen: HashSet<String> = if request.skip_existing {
        monitor::list_for_project(pool, project_id)
            .await?
            .iter()
            .map(|m| existing_key(&MonitorConfig::from(m)))
            .collect()
    } else {
        HashSet::new()
    };
    let mut report = ImportReport {
        dry_run: request.dry_run,
        created: 0,
        skipped: 0,
        errors: 0,
        results: Vec::with_capacity(entries.len()),
    };
    for entry in entries {
        let row = import_one(
            pool,
            cipher,
            project_id,
            entry,
            request.dry_run,
            request.skip_existing.then_some(&mut seen),
        )
        .await?;
        report.push(row);
    }
    if let Some(lock) = lock {
        lock.rollback().await?;
    }
    tracing::info!(
        %project_id,
        dry_run = report.dry_run,
        created = report.created,
        skipped = report.skipped,
        errors = report.errors,
        "monitor import finished"
    );
    Ok(report)
}

/// One entry: deserialize, validate, skip check, then create (unless a dry
/// run). `seen` is `None` when nothing is to be skipped. Only a database
/// failure that is not about this entry's data (the connection, the pool)
/// ends the import with an error.
async fn import_one(
    pool: &PgPool,
    cipher: &Cipher,
    project_id: Uuid,
    entry: ParsedEntry,
    dry_run: bool,
    seen: Option<&mut HashSet<String>>,
) -> Result<ImportResultRow, AppError> {
    let ParsedEntry {
        line,
        body,
        warning,
    } = entry;
    let body = match body {
        Ok(body) => body,
        Err(message) => return Ok(ImportResultRow::error(line, message)),
    };
    let Ok(request) = serde_json::from_value::<CreateMonitorRequest>(Value::Object(body)) else {
        return Ok(ImportResultRow::error(line, UNREADABLE_ENTRY));
    };
    // `validate`'s messages never repeat a secret (header errors name the
    // header, never its value), so they are shown verbatim, exactly as a
    // single create shows them.
    let config = match monitor::validate(request.into_config()) {
        Ok(config) => config,
        Err(message) => return Ok(ImportResultRow::error(line, message)),
    };
    let mut row = ImportResultRow {
        line,
        status: EntryStatus::Created,
        name: Some(config.name.clone()),
        target: target_of(&config),
        monitor_id: None,
        error: None,
        warning: None,
    };
    if let Some(seen) = seen
        && !seen.insert(existing_key(&config))
    {
        row.status = EntryStatus::Skipped;
        row.error = Some(ALREADY_MONITORED.into());
        return Ok(row);
    }
    row.warning = warning;
    if dry_run {
        return Ok(row);
    }
    match monitor::insert_monitor(pool, cipher, project_id, &config).await {
        Ok(created) => {
            row.monitor_id = Some(created.id);
            Ok(row)
        }
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => {
            // The group (or, for a global admin, the project) was deleted
            // while the import ran.
            Ok(ImportResultRow::error(line, "group not found"))
        }
        // Integrity (23) and data (22) errors are about this entry's values:
        // report them on the entry and go on with the next.
        Err(sqlx::Error::Database(e))
            if e.code()
                .is_some_and(|c| c.starts_with("22") || c.starts_with("23")) =>
        {
            tracing::warn!(
                %project_id,
                line,
                code = e.code().as_deref().unwrap_or_default(),
                "import entry refused by the database"
            );
            Ok(ImportResultRow::error(line, UNSTORABLE_ENTRY))
        }
        Err(e) => Err(AppError::Database(e)),
    }
}
