//! Log search parameters — shared by search, histogram and live tail — and
//! their translation into ClickHouse SQL.
//!
//! Injection safety: [`Sql`] only appends `&'static str` fragments and
//! numbered placeholders (`{p3:String}`); every value — the project id,
//! JSON path segments, `q`, filter values and regexes — travels as a query
//! parameter. No user input can reach the SQL text.

use std::fmt::Write as _;

use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use super::normalize::LEVELS;

pub const DEFAULT_WINDOW: Duration = Duration::hours(1);
pub const MAX_WINDOW: Duration = Duration::days(31);
pub const MAX_FILTERS: usize = 10;
pub const MAX_PATH_DEPTH: usize = 5;
pub const MAX_SEGMENT_LEN: usize = 64;
pub const MAX_REGEX_LEN: usize = 256;
/// Longest `q` and longest text filter value, in characters.
pub const MAX_TEXT_LEN: usize = 1024;
/// Most values of a repeatable list parameter (`source_id`, `level`,
/// `pattern_hash`).
pub const MAX_LIST_VALUES: usize = 100;
pub const DEFAULT_LIMIT: u32 = 100;
pub const MAX_LIMIT: u32 = 500;
/// Levels counted as errors by the histogram.
pub const ERROR_LEVELS_SQL: &str = "('error', 'critical', 'alert', 'emergency', 'fatal')";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterOp {
    Eq,
    Neq,
    Contains,
    Exists,
    Regex,
    NotRegex,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FilterValue {
    None,
    Text(String),
    Number(f64),
}

/// One `filter` parameter: `{"path":"a.b","op":"eq","value":"…"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldFilter {
    pub path: Vec<String>,
    pub op: FilterOp,
    pub value: FilterValue,
}

/// What rows to match, besides the project and the time window.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogFilters {
    pub source_ids: Vec<Uuid>,
    pub levels: Vec<String>,
    pub q: Option<String>,
    pub in_fields: bool,
    pub filters: Vec<FieldFilter>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub pattern_hashes: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

/// Opaque keyset position: the `(dt, id)` of the last row returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub dt_ms: i64,
    pub id: Uuid,
}

impl Cursor {
    pub fn encode(&self) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!("{}:{}", self.dt_ms, self.id))
    }

    pub fn decode(raw: &str) -> Option<Cursor> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .ok()?;
        let text = String::from_utf8(bytes).ok()?;
        let (dt_ms, id) = text.split_once(':')?;
        Some(Cursor {
            dt_ms: dt_ms.parse().ok()?,
            id: id.parse().ok()?,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchParams {
    pub window: Window,
    pub filters: LogFilters,
    pub limit: u32,
    pub cursor: Option<Cursor>,
}

/// The decoded query string; repeated keys are kept in order.
pub fn query_pairs(raw: Option<&str>) -> Vec<(String, String)> {
    form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .into_owned()
        .collect()
}

fn values<'a>(pairs: &'a [(String, String)], key: &str) -> impl Iterator<Item = &'a str> {
    pairs
        .iter()
        .filter(move |(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn last<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    values(pairs, key).last()
}

/// `source_id`, `level`, `q`, `in_fields`, `filter`, `trace_id`, `span_id`
/// and `pattern_hash`; other keys are ignored.
pub fn parse_filters(pairs: &[(String, String)]) -> Result<LogFilters, String> {
    let mut filters = LogFilters::default();
    for raw in values(pairs, "source_id") {
        let id = Uuid::parse_str(raw.trim()).map_err(|_| "source_id must be a UUID".to_string())?;
        filters.source_ids.push(id);
    }
    for raw in values(pairs, "level") {
        let level = raw.trim().to_ascii_lowercase();
        if !LEVELS.contains(&level.as_str()) {
            return Err(format!("level must be one of {}", LEVELS.join(", ")));
        }
        filters.levels.push(level);
    }
    for raw in values(pairs, "pattern_hash") {
        let hash = raw
            .trim()
            .parse::<u64>()
            .map_err(|_| "pattern_hash must be an unsigned 64-bit integer".to_string())?;
        filters.pattern_hashes.push(hash);
    }
    if [
        filters.source_ids.len(),
        filters.levels.len(),
        filters.pattern_hashes.len(),
    ]
    .iter()
    .any(|len| *len > MAX_LIST_VALUES)
    {
        return Err(format!(
            "at most {MAX_LIST_VALUES} values per source_id, level or pattern_hash"
        ));
    }
    if let Some(q) = last(pairs, "q").filter(|q| !q.is_empty()) {
        if q.chars().count() > MAX_TEXT_LEN {
            return Err(format!("q must be at most {MAX_TEXT_LEN} characters"));
        }
        filters.q = Some(q.to_string());
    }
    filters.in_fields = match last(pairs, "in_fields") {
        None | Some("") | Some("false") | Some("0") => false,
        Some("true") | Some("1") => true,
        Some(_) => return Err("in_fields must be true or false".to_string()),
    };
    if let Some(raw) = last(pairs, "trace_id").filter(|v| !v.is_empty()) {
        filters.trace_id = Some(hex_id(raw, 32, "trace_id")?);
    }
    if let Some(raw) = last(pairs, "span_id").filter(|v| !v.is_empty()) {
        filters.span_id = Some(hex_id(raw, 16, "span_id")?);
    }
    for (index, raw) in values(pairs, "filter").enumerate() {
        if index == MAX_FILTERS {
            return Err(format!("at most {MAX_FILTERS} filters"));
        }
        filters
            .filters
            .push(parse_filter(raw).map_err(|e| format!("filter {}: {e}", index + 1))?);
    }
    Ok(filters)
}

fn hex_id(raw: &str, len: usize, name: &str) -> Result<String, String> {
    let id = raw.trim().to_ascii_lowercase();
    if id.len() == len && id.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(id)
    } else {
        Err(format!("{name} must be {len} hex characters"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFilter {
    path: String,
    op: FilterOp,
    #[serde(default)]
    value: Option<Value>,
}

/// A dotted JSON path into `fields` (`a.b.c`): at most [`MAX_PATH_DEPTH`]
/// segments of 1-[`MAX_SEGMENT_LEN`] characters of A-Z, a-z, 0-9, `_` or
/// `-`. The rule of search filters, shared with the ingest pipeline's paths.
pub fn parse_path(raw: &str) -> Result<Vec<String>, String> {
    let path: Vec<String> = raw.split('.').map(str::to_string).collect();
    if path.len() > MAX_PATH_DEPTH {
        return Err(format!("path may have at most {MAX_PATH_DEPTH} segments"));
    }
    let valid_segment = |segment: &String| {
        (1..=MAX_SEGMENT_LEN).contains(&segment.len())
            && segment
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if !path.iter().all(valid_segment) {
        return Err("path segments must be 1-64 characters of A-Z, a-z, 0-9, _ or -".to_string());
    }
    Ok(path)
}

pub fn parse_filter(raw: &str) -> Result<FieldFilter, String> {
    let raw: RawFilter = serde_json::from_str(raw).map_err(|_| {
        "must be JSON like {\"path\":\"a.b\",\"op\":\"eq\",\"value\":\"x\"} with op one of eq, neq, contains, \
         exists, regex, not_regex, gt, gte, lt, lte"
            .to_string()
    })?;
    let path = parse_path(&raw.path)?;
    let value = match raw.op {
        FilterOp::Exists => FilterValue::None,
        FilterOp::Eq | FilterOp::Neq | FilterOp::Contains => {
            let text = match raw.value {
                Some(Value::String(text)) => text,
                Some(value @ (Value::Number(_) | Value::Bool(_))) => value.to_string(),
                _ => return Err("value must be a string, number or boolean".to_string()),
            };
            if text.chars().count() > MAX_TEXT_LEN {
                return Err(format!("value must be at most {MAX_TEXT_LEN} characters"));
            }
            FilterValue::Text(text)
        }
        FilterOp::Regex | FilterOp::NotRegex => {
            let Some(Value::String(pattern)) = raw.value else {
                return Err("value must be a regular expression string".to_string());
            };
            if pattern.chars().count() > MAX_REGEX_LEN {
                return Err(format!("regex must be at most {MAX_REGEX_LEN} characters"));
            }
            regex::RegexBuilder::new(&pattern)
                .size_limit(1 << 20)
                .build()
                .map_err(|_| "regex is not a valid regular expression".to_string())?;
            FilterValue::Text(pattern)
        }
        FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte => {
            let number = match raw.value {
                Some(Value::Number(number)) => number.as_f64(),
                Some(Value::String(text)) => text.trim().parse::<f64>().ok(),
                _ => None,
            };
            match number {
                Some(number) if number.is_finite() => FilterValue::Number(number),
                _ => {
                    return Err("value must be a finite number for gt, gte, lt and lte".to_string());
                }
            }
        }
    };
    Ok(FieldFilter {
        path,
        op: raw.op,
        value,
    })
}

/// `from` / `to` (RFC 3339): `to` defaults to now and `from` to one hour
/// before `to`; at most 31 days apart.
pub fn parse_window(pairs: &[(String, String)], now: DateTime<Utc>) -> Result<Window, String> {
    let instant = |key: &str| -> Result<Option<DateTime<Utc>>, String> {
        match last(pairs, key).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(raw) => DateTime::parse_from_rfc3339(raw.trim())
                .map(|dt| Some(dt.with_timezone(&Utc)))
                .map_err(|_| format!("{key} must be an RFC 3339 timestamp")),
        }
    };
    let to = instant("to")?.unwrap_or(now);
    let from = instant("from")?.unwrap_or(to - DEFAULT_WINDOW);
    if from >= to {
        return Err("from must be before to".to_string());
    }
    if to - from > MAX_WINDOW {
        return Err("the time window may span at most 31 days".to_string());
    }
    Ok(Window { from, to })
}

pub fn parse_search(
    pairs: &[(String, String)],
    now: DateTime<Utc>,
) -> Result<SearchParams, String> {
    let window = parse_window(pairs, now)?;
    let filters = parse_filters(pairs)?;
    let limit = match last(pairs, "limit").filter(|v| !v.is_empty()) {
        None => DEFAULT_LIMIT,
        Some(raw) => match raw.trim().parse::<u32>() {
            Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => limit,
            _ => return Err(format!("limit must be between 1 and {MAX_LIMIT}")),
        },
    };
    let cursor = match last(pairs, "cursor").filter(|v| !v.is_empty()) {
        None => None,
        Some(raw) => Some(Cursor::decode(raw.trim()).ok_or_else(|| "invalid cursor".to_string())?),
    };
    Ok(SearchParams {
        window,
        filters,
        limit,
        cursor,
    })
}

/// SQL text built from fixed fragments plus numbered parameters.
#[derive(Debug, Default)]
pub struct Sql {
    text: String,
    params: Vec<(String, String)>,
}

impl Sql {
    pub fn new() -> Sql {
        Sql::default()
    }

    pub fn push(&mut self, fragment: &'static str) -> &mut Sql {
        self.text.push_str(fragment);
        self
    }

    /// Adds a parameter and returns its index (it can be used several times).
    pub fn bind(&mut self, value: impl Into<String>) -> usize {
        let index = self.params.len();
        self.params.push((format!("p{index}"), value.into()));
        index
    }

    /// Appends the placeholder `{p<index>:<ty>}`.
    pub fn placeholder(&mut self, index: usize, ty: &'static str) -> &mut Sql {
        write!(self.text, "{{p{index}:{ty}}}").expect("writing to a String cannot fail");
        self
    }

    /// Binds `value` and appends its placeholder.
    pub fn value(&mut self, ty: &'static str, value: impl Into<String>) -> &mut Sql {
        let index = self.bind(value);
        self.placeholder(index, ty)
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// The parameters in the shape `ClickHouse::query_rows` takes.
    pub fn params(&self) -> Vec<(&str, String)> {
        self.params
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone()))
            .collect()
    }

    /// Binds each of `items` and appends their placeholders, comma-separated.
    pub fn list<T: ToString>(&mut self, ty: &'static str, items: &[T]) {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.push(", ");
            }
            self.value(ty, item.to_string());
        }
    }
}

/// ` WHERE project_id = … AND <filters>`: never rows of another project.
pub fn push_where(sql: &mut Sql, project_id: Uuid, filters: &LogFilters) {
    sql.push(" WHERE project_id = ")
        .value("UUID", project_id.to_string());
    if !filters.source_ids.is_empty() {
        sql.push(" AND source_id IN (");
        sql.list("UUID", &filters.source_ids);
        sql.push(")");
    }
    if !filters.levels.is_empty() {
        sql.push(" AND level IN (");
        sql.list("String", &filters.levels);
        sql.push(")");
    }
    if let Some(q) = &filters.q {
        let q = sql.bind(q.clone());
        sql.push(" AND (positionCaseInsensitiveUTF8(message, ")
            .placeholder(q, "String")
            .push(") > 0");
        if filters.in_fields {
            sql.push(" OR positionCaseInsensitiveUTF8(fields, ")
                .placeholder(q, "String")
                .push(") > 0");
        }
        sql.push(")");
    }
    if let Some(trace_id) = &filters.trace_id {
        sql.push(" AND trace_id = ")
            .value("String", trace_id.clone());
    }
    if let Some(span_id) = &filters.span_id {
        sql.push(" AND span_id = ").value("String", span_id.clone());
    }
    if !filters.pattern_hashes.is_empty() {
        sql.push(" AND pattern_hash IN (");
        sql.list("UInt64", &filters.pattern_hashes);
        sql.push(")");
    }
    for filter in &filters.filters {
        sql.push(" AND ");
        push_filter(sql, filter);
    }
}

/// ` AND dt >= from AND dt <= to`.
pub fn push_window(sql: &mut Sql, window: &Window) {
    sql.push(" AND dt >= fromUnixTimestamp64Milli(")
        .value("Int64", window.from.timestamp_millis().to_string())
        .push(", 'UTC') AND dt <= fromUnixTimestamp64Milli(")
        .value("Int64", window.to.timestamp_millis().to_string())
        .push(", 'UTC')");
}

fn push_path(sql: &mut Sql, segments: &[usize]) {
    for segment in segments {
        sql.push(", ").placeholder(*segment, "String");
    }
}

/// The value at the path as text: strings unquoted, anything else as its
/// JSON text (`200`, `true`, `{"a":1}`).
fn push_text(sql: &mut Sql, segments: &[usize]) {
    sql.push("if(JSONType(fields");
    push_path(sql, segments);
    sql.push(") = 'String', JSONExtractString(fields");
    push_path(sql, segments);
    sql.push("), JSONExtractRaw(fields");
    push_path(sql, segments);
    sql.push("))");
}

/// The value at the path as a number: JSON numbers, and strings that parse
/// as one; NULL (never matching) for anything else or a missing field.
fn push_number(sql: &mut Sql, segments: &[usize]) {
    sql.push("multiIf(JSONType(fields");
    push_path(sql, segments);
    sql.push(") IN ('Int64', 'UInt64', 'Double'), JSONExtractFloat(fields");
    push_path(sql, segments);
    sql.push("), JSONType(fields");
    push_path(sql, segments);
    sql.push(") = 'String', toFloat64OrNull(JSONExtractString(fields");
    push_path(sql, segments);
    sql.push(")), NULL)");
}

/// `neq` and `not_regex` also match rows without the field; the numeric
/// operators never match a missing or non-numeric field.
fn push_filter(sql: &mut Sql, filter: &FieldFilter) {
    let segments: Vec<usize> = filter
        .path
        .iter()
        .map(|segment| sql.bind(segment.clone()))
        .collect();
    match (&filter.op, &filter.value) {
        (FilterOp::Exists, _) => {
            sql.push("JSONHas(fields");
            push_path(sql, &segments);
            sql.push(")");
        }
        (
            FilterOp::Gt | FilterOp::Gte | FilterOp::Lt | FilterOp::Lte,
            FilterValue::Number(number),
        ) => {
            let value = sql.bind(number.to_string());
            push_number(sql, &segments);
            sql.push(match filter.op {
                FilterOp::Gt => " > ",
                FilterOp::Gte => " >= ",
                FilterOp::Lt => " < ",
                _ => " <= ",
            });
            sql.placeholder(value, "Float64");
        }
        (op, FilterValue::Text(text)) => {
            let value = sql.bind(text.clone());
            if matches!(op, FilterOp::Neq | FilterOp::NotRegex) {
                sql.push("NOT ");
            }
            sql.push("(JSONHas(fields");
            push_path(sql, &segments);
            sql.push(") AND ");
            match op {
                FilterOp::Contains => {
                    sql.push("positionCaseInsensitiveUTF8(");
                    push_text(sql, &segments);
                    sql.push(", ").placeholder(value, "String").push(") > 0");
                }
                FilterOp::Regex | FilterOp::NotRegex => {
                    sql.push("match(");
                    push_text(sql, &segments);
                    sql.push(", ").placeholder(value, "String").push(")");
                }
                _ => {
                    push_text(sql, &segments);
                    sql.push(" = ").placeholder(value, "String");
                }
            }
            sql.push(")");
        }
        // `parse_filter` pairs every operator with its value kind.
        _ => {
            sql.push("0");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(query: &str) -> Vec<(String, String)> {
        query_pairs(Some(query))
    }

    fn now() -> DateTime<Utc> {
        "2026-10-13T12:00:00Z".parse().unwrap()
    }

    #[test]
    fn the_window_defaults_to_the_last_hour_and_is_capped_at_31_days() {
        let window = parse_window(&pairs(""), now()).unwrap();
        assert_eq!(
            (window.from, window.to),
            (now() - Duration::hours(1), now())
        );
        let custom = parse_window(
            &pairs("from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:00Z"),
            now(),
        )
        .unwrap();
        assert_eq!(custom.to - custom.from, Duration::days(31));
        for (query, message) in [
            (
                "from=2026-10-01T00:00:00Z&to=2026-11-01T00:00:01Z",
                "at most 31 days",
            ),
            (
                "from=2026-10-13T12:00:00Z&to=2026-10-13T12:00:00Z",
                "from must be before to",
            ),
            ("from=yesterday", "from must be an RFC 3339 timestamp"),
            ("to=2026-13-01", "to must be an RFC 3339 timestamp"),
        ] {
            let error = parse_window(&pairs(query), now()).unwrap_err();
            assert!(error.contains(message), "{query}: {error}");
        }
    }

    #[test]
    fn list_parameters_repeat_and_are_validated() {
        let id = Uuid::new_v4();
        let f = parse_filters(&pairs(&format!(
            "source_id={id}&level=ERROR&level=warn&pattern_hash=18446744073709551615"
        )))
        .unwrap();
        assert_eq!(f.source_ids, vec![id]);
        assert_eq!(f.levels, vec!["error", "warn"]);
        assert_eq!(f.pattern_hashes, vec![u64::MAX]);
        for (query, message) in [
            ("source_id=x", "source_id must be a UUID"),
            ("level=verbose", "level must be one of"),
            ("level=error%27%20OR%201%3D1", "level must be one of"),
            (
                "pattern_hash=-1",
                "pattern_hash must be an unsigned 64-bit integer",
            ),
            ("trace_id=abc", "trace_id must be 32 hex characters"),
            (
                "span_id=zzzzzzzzzzzzzzzz",
                "span_id must be 16 hex characters",
            ),
            ("in_fields=maybe", "in_fields must be true or false"),
        ] {
            let error = parse_filters(&pairs(query)).unwrap_err();
            assert!(error.contains(message), "{query}: {error}");
        }
        let many = (0..101).map(|_| "level=info").collect::<Vec<_>>().join("&");
        assert!(
            parse_filters(&pairs(&many))
                .unwrap_err()
                .contains("at most 100 values")
        );
    }

    #[test]
    fn q_trace_and_span_ids() {
        let f = parse_filters(&pairs("q=Disk%20Full&in_fields=true&trace_id=4BF92F3577B34DA6A3CE929D0E0E4736&span_id=00f067aa0ba902b7")).unwrap();
        assert_eq!(f.q.as_deref(), Some("Disk Full"));
        assert!(f.in_fields);
        assert_eq!(
            f.trace_id.as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
        assert_eq!(f.span_id.as_deref(), Some("00f067aa0ba902b7"));
        assert_eq!(parse_filters(&pairs("q=")).unwrap().q, None);
        assert!(
            parse_filters(&pairs(&format!("q={}", "x".repeat(1025))))
                .unwrap_err()
                .contains("at most 1024")
        );
    }

    #[test]
    fn filters_are_validated() {
        let ok = parse_filter(r#"{"path":"user.tier-1_x","op":"eq","value":"gold"}"#).unwrap();
        assert_eq!(ok.path, vec!["user", "tier-1_x"]);
        assert_eq!(ok.value, FilterValue::Text("gold".to_string()));
        assert_eq!(
            parse_filter(r#"{"path":"status","op":"eq","value":200}"#)
                .unwrap()
                .value,
            FilterValue::Text("200".to_string())
        );
        assert_eq!(
            parse_filter(r#"{"path":"ms","op":"gte","value":"12.5"}"#)
                .unwrap()
                .value,
            FilterValue::Number(12.5)
        );
        assert_eq!(
            parse_filter(r#"{"path":"a","op":"exists"}"#).unwrap().value,
            FilterValue::None
        );
        for (raw, message) in [
            (
                r#"{"path":"a","op":"like","value":"x"}"#,
                "must be JSON like",
            ),
            (
                r#"{"path":"a","op":"eq","value":"x","extra":1}"#,
                "must be JSON like",
            ),
            ("not json", "must be JSON like"),
            (r#"{"path":"a..b","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a b","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"a'","op":"exists"}"#, "path segments must be"),
            (r#"{"path":"","op":"exists"}"#, "path segments must be"),
            (
                r#"{"path":"a.b.c.d.e.f","op":"exists"}"#,
                "at most 5 segments",
            ),
            (r#"{"path":"é","op":"exists"}"#, "path segments must be"),
            (
                r#"{"path":"a","op":"eq"}"#,
                "value must be a string, number or boolean",
            ),
            (
                r#"{"path":"a","op":"eq","value":[1]}"#,
                "value must be a string, number or boolean",
            ),
            (
                r#"{"path":"a","op":"regex","value":"(a"}"#,
                "regex is not a valid regular expression",
            ),
            (
                r#"{"path":"a","op":"regex","value":5}"#,
                "value must be a regular expression string",
            ),
            (r#"{"path":"a","op":"gt","value":"abc"}"#, "finite number"),
            (r#"{"path":"a","op":"gt","value":"NaN"}"#, "finite number"),
            (r#"{"path":"a","op":"lt","value":"inf"}"#, "finite number"),
            (r#"{"path":"a","op":"lte","value":true}"#, "finite number"),
        ] {
            let error = parse_filter(raw).unwrap_err();
            assert!(error.contains(message), "{raw}: {error}");
        }
        let long = format!(
            r#"{{"path":"a","op":"regex","value":"{}"}}"#,
            "a".repeat(257)
        );
        assert!(parse_filter(&long).unwrap_err().contains("at most 256"));
        let segment = format!(r#"{{"path":"{}","op":"exists"}}"#, "a".repeat(65));
        assert!(
            parse_filter(&segment)
                .unwrap_err()
                .contains("path segments must be")
        );
        let eleven = (0..11)
            .map(|_| r#"filter={"path":"a","op":"exists"}"#)
            .collect::<Vec<_>>()
            .join("&");
        assert!(
            parse_filters(&pairs(&eleven))
                .unwrap_err()
                .contains("at most 10 filters")
        );
        assert!(
            parse_filters(&pairs(r#"filter={"path":"a","op":"zz"}"#))
                .unwrap_err()
                .starts_with("filter 1:")
        );
    }

    #[test]
    fn limit_and_cursor() {
        let cursor = Cursor {
            dt_ms: 1_791_892_800_123,
            id: Uuid::new_v4(),
        };
        let params = parse_search(
            &pairs(&format!("limit=500&cursor={}", cursor.encode())),
            now(),
        )
        .unwrap();
        assert_eq!((params.limit, params.cursor), (500, Some(cursor)));
        assert_eq!(parse_search(&pairs(""), now()).unwrap().limit, 100);
        for query in [
            "limit=0",
            "limit=501",
            "limit=x",
            "cursor=not-a-cursor",
            "cursor=MTIzOm5vdC1hLXV1aWQ",
        ] {
            assert!(parse_search(&pairs(query), now()).is_err(), "{query}");
        }
    }

    /// Security: whatever the input, the SQL text holds only fragments and
    /// placeholders; values arrive as parameters.
    #[test]
    fn user_input_never_reaches_the_sql_text() {
        let evil = "'; DROP TABLE logs; -- {p0:String} \\ sleep(3)\n";
        let query = format!(
            "q={q}&filter={f1}&filter={f2}&filter={f3}",
            q = urlencode(evil),
            f1 = urlencode(
                &serde_json::json!({ "path": "a.b", "op": "eq", "value": evil }).to_string()
            ),
            f2 = urlencode(
                &serde_json::json!({ "path": "c", "op": "regex", "value": "'; DROP" }).to_string()
            ),
            f3 = urlencode(
                &serde_json::json!({ "path": "n", "op": "gt", "value": 1e300 }).to_string()
            ),
        );
        let filters = parse_filters(&pairs(&query)).unwrap();
        let mut sql = Sql::new();
        sql.push("SELECT count() FROM logs");
        push_where(&mut sql, Uuid::nil(), &filters);
        for fragment in ["DROP", "sleep", "'; ", "\n", "\\"] {
            assert!(
                !sql.text().contains(fragment),
                "{fragment:?} leaked into {}",
                sql.text()
            );
        }
        let values: Vec<String> = sql.params().into_iter().map(|(_, value)| value).collect();
        assert!(values.contains(&evil.to_string()));
        assert!(values.contains(&"a".to_string()) && values.contains(&"b".to_string()));
    }

    fn urlencode(text: &str) -> String {
        form_urlencoded::byte_serialize(text.as_bytes()).collect()
    }

    #[test]
    fn placeholders_are_numbered_and_reusable() {
        let mut sql = Sql::new();
        let index = sql.bind("x");
        sql.push("a = ")
            .placeholder(index, "String")
            .push(" OR b = ")
            .placeholder(index, "String");
        sql.push(" AND c = ").value("UInt32", "7");
        assert_eq!(
            sql.text(),
            "a = {p0:String} OR b = {p0:String} AND c = {p1:UInt32}"
        );
        assert_eq!(
            sql.params(),
            vec![("p0", "x".to_string()), ("p1", "7".to_string())]
        );
    }
}
