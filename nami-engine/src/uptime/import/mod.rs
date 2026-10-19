//! Monitor import: parsers that turn a pasted URL list, an Uptime Kuma backup
//! or an UptimeRobot CSV export into `POST /projects/{id}/monitors` bodies.
//!
//! Pure functions, no I/O. A parser never validates monitors itself: every
//! body is judged later by `models::monitor::validate`, the same code a
//! hand-made monitor goes through (so invisible characters in names, private
//! URLs and the like are refused there). Secrets found in a source (headers,
//! basic-auth passwords) are carried in the bodies, in memory only; error text
//! never quotes a source value beyond a short `printable` snippet of a type
//! name.
use serde::Deserialize;
use serde_json::{Map, Value};
use std::fmt;
use uuid::Uuid;

pub mod kuma;
pub mod uptimerobot;
pub mod urls;

/// Most entries one import may hold (counted before any mapping).
pub const MAX_ENTRIES: usize = 500;
/// Largest import payload, in bytes. The route caps its request body with
/// this; `parse` re-checks it so the bound holds for every caller.
pub const MAX_IMPORT_BODY_BYTES: usize = 2 * 1024 * 1024;

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
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub line: usize,
    pub body: Result<Map<String, Value>, String>,
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
