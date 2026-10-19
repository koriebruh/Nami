//! One URL per line; blank lines and `#` comments are skipped.
use super::{MAX_ENTRIES, ParseError, ParsedEntry};
use serde_json::{Map, Value};

pub fn parse(content: &str) -> Result<Vec<ParsedEntry>, ParseError> {
    let lines: Vec<(usize, &str)> = content
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|(i, l)| (i + 1, l.trim()))
        .collect();
    if lines.len() > MAX_ENTRIES {
        return Err(ParseError::TooManyEntries(lines.len()));
    }
    Ok(lines
        .into_iter()
        .map(|(line, text)| ParsedEntry {
            line,
            body: entry(text),
        })
        .collect())
}

fn entry(text: &str) -> Result<Map<String, Value>, String> {
    let host = reqwest::Url::parse(text)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .and_then(|u| u.host_str().map(str::to_string))
        .ok_or_else(|| "line is not a valid http(s) URL".to_string())?;
    let mut body = Map::new();
    body.insert("check_type".into(), "http".into());
    body.insert("name".into(), host.into());
    body.insert("url".into(), text.into());
    Ok(body)
}
