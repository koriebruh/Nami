//! Uptime Kuma backup (`monitorList` of a JSON export). Only monitor fields
//! are read: the notification list, proxies and everything else in the
//! document are never touched, so their secrets cannot leak.
use super::printable;
use super::{MAX_ENTRIES, ParseError, ParsedEntry};
use crate::uptime::monitor::model::DNS_RECORD_TYPES;
use serde_json::{Map, Value};

pub fn parse(content: &str) -> Result<Vec<ParsedEntry>, ParseError> {
    // serde_json refuses nesting beyond 128 levels (an error, not a stack
    // overflow); the caller's byte cap bounds the tree. Its error text only
    // carries a position for syntax errors, but a wrong-typed value is quoted,
    // so errors are rebuilt from line/column alone.
    let doc: Value = serde_json::from_str(content).map_err(|e| {
        ParseError::Malformed(format!(
            "content is not valid JSON (line {}, column {})",
            e.line(),
            e.column()
        ))
    })?;
    let list = doc
        .get("monitorList")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            ParseError::Malformed(
                "content has no monitorList array (is it an Uptime Kuma backup?)".into(),
            )
        })?;
    if list.len() > MAX_ENTRIES {
        return Err(ParseError::TooManyEntries(list.len()));
    }
    Ok(list
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let body = entry(item);
            let warning = body.as_ref().ok().and_then(|_| status_codes_warning(item));
            ParsedEntry {
                line: i + 1,
                body,
                warning,
            }
        })
        .collect())
}

/// Kuma accepts several status ranges; a monitor holds one, so only the first
/// is imported. Said in the entry's result instead of silently narrowing what
/// counts as up. Quotes no source text, only how many ranges were listed.
fn status_codes_warning(item: &Value) -> Option<String> {
    let listed = item.get("accepted_statuscodes")?.as_array()?.len();
    (listed > 1).then(|| {
        format!(
            "only the first of {listed} accepted_statuscodes ranges was imported; \
             adjust the expected status range if the others matter"
        )
    })
}

fn str_field<'a>(m: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    m.get(key).and_then(Value::as_str)
}

/// A Kuma boolean column: JSON bool, 0/1, or the strings "true"/"false"/"1"/"0"
/// (backups from different versions and databases differ). Anything else is
/// `None`, so the caller's default applies.
fn flag(m: &Map<String, Value>, key: &str) -> Option<bool> {
    match m.get(key)? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => match n.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "1" => Some(true),
            "false" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn int_field(m: &Map<String, Value>, key: &str) -> Option<i64> {
    let v = m.get(key)?;
    v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64))
}

fn entry(item: &Value) -> Result<Map<String, Value>, String> {
    let m = item
        .as_object()
        .ok_or_else(|| "entry is not a JSON object".to_string())?;
    let name = str_field(m, "name")
        .filter(|n| !n.trim().is_empty())
        .ok_or_else(|| "entry has no name".to_string())?;
    if flag(m, "upsideDown") == Some(true) {
        return Err("upside-down mode is not supported".into());
    }
    let kind = str_field(m, "type").unwrap_or("");
    let check_type = match kind {
        "http" => "http",
        "keyword" => "keyword",
        "port" => "tcp",
        "ping" => "ping",
        "dns" => "dns",
        other => {
            return Err(format!(
                "Uptime Kuma monitor type \"{}\" is not supported",
                printable(other, 40)
            ));
        }
    };
    let mut body = Map::new();
    body.insert("name".into(), name.into());
    body.insert("check_type".into(), check_type.into());

    match check_type {
        "http" | "keyword" => http_fields(m, check_type, &mut body)?,
        "tcp" | "ping" | "dns" => {
            if let Some(host) = str_field(m, "hostname") {
                body.insert("host".into(), host.into());
            }
            match check_type {
                "tcp" => {
                    if let Some(port) = int_field(m, "port") {
                        body.insert("port".into(), port.into());
                    }
                }
                "dns" => {
                    let record = str_field(m, "dns_resolve_type").unwrap_or("A");
                    if !DNS_RECORD_TYPES.contains(&record) {
                        return Err(format!(
                            "Uptime Kuma DNS record type \"{}\" is not supported",
                            printable(record, 40)
                        ));
                    }
                    body.insert("dns_record_type".into(), record.into());
                    if let Some(server) =
                        str_field(m, "dns_resolve_server").filter(|s| !s.is_empty())
                    {
                        let port = int_field(m, "port").unwrap_or(53);
                        let server = if server.contains(':') {
                            format!("[{server}]")
                        } else {
                            server.to_string()
                        };
                        body.insert("dns_nameserver".into(), format!("{server}:{port}").into());
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }

    let interval = int_field(m, "interval").map(|i| i.max(30));
    if let Some(interval) = interval {
        body.insert("interval_seconds".into(), interval.into());
    }
    if let Some(retries) = int_field(m, "maxretries") {
        body.insert(
            "confirmation_threshold".into(),
            retries.saturating_add(1).clamp(1, 10).into(),
        );
    }
    if let Some(timeout) = m.get("timeout").and_then(Value::as_f64) {
        let ceiling = interval.map_or(60, |i| (i - 1).min(60));
        body.insert(
            "timeout_seconds".into(),
            (timeout.round() as i64).clamp(1, ceiling).into(),
        );
    }
    let active = flag(m, "active").unwrap_or(true);
    body.insert("paused".into(), (!active).into());
    Ok(body)
}

fn http_fields(
    m: &Map<String, Value>,
    check_type: &str,
    body: &mut Map<String, Value>,
) -> Result<(), String> {
    if let Some(url) = str_field(m, "url") {
        body.insert("url".into(), url.into());
    }
    let method = str_field(m, "method").map_or("GET".to_string(), str::to_uppercase);
    body.insert("http_method".into(), method.into());
    if check_type == "keyword" {
        let keyword =
            str_field(m, "keyword").ok_or_else(|| "keyword monitor has no keyword".to_string())?;
        body.insert("keyword".into(), keyword.into());
        if flag(m, "invertKeyword") == Some(true) {
            body.insert("keyword_mode".into(), "not_contains".into());
        }
    }
    if let Some(text) = str_field(m, "body").filter(|b| !b.is_empty()) {
        body.insert("request_body".into(), text.into());
    }
    if let Some(raw) = m.get("headers").filter(|v| !v.is_null()) {
        const BAD: &str = "headers is not a JSON object of strings";
        let text = raw.as_str().ok_or_else(|| BAD.to_string())?;
        if !text.trim().is_empty() {
            let parsed: Value = serde_json::from_str(text).map_err(|_| BAD.to_string())?;
            let object = parsed.as_object().ok_or_else(|| BAD.to_string())?;
            let mut headers: Vec<(&String, &str)> = Vec::new();
            for (k, v) in object {
                headers.push((k, v.as_str().ok_or_else(|| BAD.to_string())?));
            }
            headers.sort();
            if !headers.is_empty() {
                body.insert(
                    "request_headers".into(),
                    Value::Array(
                        headers
                            .into_iter()
                            .map(|(k, v)| serde_json::json!({"name": k, "value": v}))
                            .collect(),
                    ),
                );
            }
        }
    }
    match str_field(m, "authMethod") {
        None | Some("") | Some("basic") => {}
        Some(other) => {
            return Err(format!(
                "authMethod \"{}\" is not supported (only basic)",
                printable(other, 40)
            ));
        }
    }
    if let Some(user) = str_field(m, "basic_auth_user").filter(|u| !u.is_empty()) {
        body.insert("auth_username".into(), user.into());
        if let Some(pass) = str_field(m, "basic_auth_pass") {
            body.insert("auth_password".into(), pass.into());
        }
    }
    if let Some(first) = m
        .get("accepted_statuscodes")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
    {
        let bad = || {
            format!(
                "accepted_statuscodes \"{}\" is not a status range",
                printable(first, 40)
            )
        };
        let (lo, hi) = match first.split_once('-') {
            Some((a, b)) => (a.trim().parse::<i32>(), b.trim().parse::<i32>()),
            None => (first.trim().parse::<i32>(), first.trim().parse::<i32>()),
        };
        let (lo, hi) = (lo.map_err(|_| bad())?, hi.map_err(|_| bad())?);
        body.insert("expected_status_min".into(), lo.into());
        body.insert("expected_status_max".into(), hi.into());
    }
    if let Some(ignore) = flag(m, "ignoreTls") {
        body.insert("verify_ssl".into(), (!ignore).into());
    }
    if let Some(redirects) = int_field(m, "maxredirects") {
        body.insert("follow_redirects".into(), (redirects != 0).into());
    }
    Ok(())
}
