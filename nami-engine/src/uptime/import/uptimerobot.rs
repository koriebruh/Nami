//! UptimeRobot CSV export. Columns are found by header name (case-insensitive,
//! unknown columns ignored). Cells are plain text: a leading `=` is just a
//! character in a name, never evaluated, and validation judges it later.
use super::printable;
use super::{MAX_ENTRIES, ParseError, ParsedEntry};
use serde_json::{Map, Value};

#[derive(Default)]
struct Columns {
    name: Option<usize>,
    target: Option<usize>,
    kind: Option<usize>,
    keyword_type: Option<usize>,
    keyword: Option<usize>,
    port: Option<usize>,
    interval: Option<usize>,
}

pub fn parse(content: &str) -> Result<Vec<ParsedEntry>, ParseError> {
    // Exports from spreadsheets often start with a byte-order mark.
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(content.as_bytes());
    let read_err = |e: csv::Error| ParseError::Malformed(format!("CSV could not be read: {e}"));
    let mut columns = Columns::default();
    for (i, header) in reader.headers().map_err(read_err)?.iter().enumerate() {
        let slot = match header.trim().to_lowercase().as_str() {
            "friendly name" | "name" => &mut columns.name,
            "url/ip" | "url" | "ip" | "host" => &mut columns.target,
            "type" | "monitor type" => &mut columns.kind,
            "keyword type" => &mut columns.keyword_type,
            "keyword value" | "keyword" => &mut columns.keyword,
            "port" => &mut columns.port,
            "interval" | "monitoring interval" => &mut columns.interval,
            _ => continue,
        };
        slot.get_or_insert(i);
    }
    let (Some(name), Some(target), Some(kind)) = (columns.name, columns.target, columns.kind)
    else {
        return Err(ParseError::Malformed(
            "CSV header must name the columns Friendly Name, URL/IP and Type".into(),
        ));
    };
    let mut records = Vec::new();
    for record in reader.records() {
        records.push(record.map_err(read_err)?);
        if records.len() > MAX_ENTRIES {
            // Keep counting without keeping the records.
            let extra = reader.records().filter(|r| r.is_ok()).count();
            return Err(ParseError::TooManyEntries(records.len() + extra));
        }
    }
    Ok(records
        .iter()
        .map(|record| {
            let cell = |i: Option<usize>| {
                i.and_then(|i| record.get(i))
                    .map(str::trim)
                    .filter(|c| !c.is_empty())
            };
            ParsedEntry {
                line: record.position().map_or(0, |p| p.line() as usize),
                body: entry(
                    cell(Some(name)),
                    cell(Some(target)),
                    cell(Some(kind)),
                    cell(columns.keyword_type),
                    cell(columns.keyword),
                    cell(columns.port),
                    cell(columns.interval),
                ),
                warning: None,
            }
        })
        .collect())
}

fn entry(
    name: Option<&str>,
    target: Option<&str>,
    kind: Option<&str>,
    keyword_type: Option<&str>,
    keyword: Option<&str>,
    port: Option<&str>,
    interval: Option<&str>,
) -> Result<Map<String, Value>, String> {
    let name = name.ok_or("row has no Friendly Name")?;
    let target = target.ok_or("row has no URL/IP")?;
    let kind = kind.ok_or("row has no Type")?;
    let mut body = Map::new();
    body.insert("name".into(), name.into());
    match kind.to_lowercase().as_str() {
        "http(s)" | "http" | "https" | "1" => {
            body.insert("check_type".into(), "http".into());
            body.insert("url".into(), target.into());
        }
        "keyword" | "2" => {
            body.insert("check_type".into(), "keyword".into());
            body.insert("url".into(), target.into());
            if let Some(keyword) = keyword {
                body.insert("keyword".into(), keyword.into());
            }
            let mode = match keyword_type.map(str::to_lowercase).as_deref() {
                Some("not exists") | Some("2") => "not_contains",
                _ => "contains",
            };
            body.insert("keyword_mode".into(), mode.into());
        }
        "ping" | "3" => {
            body.insert("check_type".into(), "ping".into());
            body.insert("host".into(), target.into());
        }
        "port" | "4" => {
            let port = port
                .and_then(|p| p.parse::<i64>().ok())
                .ok_or("port is required for Port monitors")?;
            body.insert("check_type".into(), "tcp".into());
            body.insert("host".into(), target.into());
            body.insert("port".into(), port.into());
        }
        _ => {
            return Err(format!(
                "UptimeRobot monitor type \"{}\" is not supported",
                printable(kind, 40)
            ));
        }
    }
    if let Some(n) = interval.and_then(|i| i.parse::<i64>().ok()) {
        body.insert("interval_seconds".into(), n.max(30).into());
    }
    Ok(body)
}
