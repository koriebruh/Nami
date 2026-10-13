//! Field mapping and size caps for one ingested event. Every ingest path
//! (HTTP JSON, NDJSON and MessagePack, syslog, and later OTLP logs) ends in
//! a [`LogEvent`] built here; `store::build_row` then turns it into a row.
//!
//! Mapping of a JSON object (`from_json`):
//! - timestamp from the first of `dt` | `timestamp` | `time` that parses
//!   (RFC 3339, or unix seconds / milliseconds) and lies within
//!   [`MAX_PAST`] / [`MAX_FUTURE`] of the receive time, else the receive
//!   time (and the unusable key stays in `fields`);
//! - message from the first present of `message` | `msg` | `log`;
//! - level from the first present of `level` | `severity` | `lvl`,
//!   normalized to one of [`LEVELS`] (unknown -> `info`);
//! - `trace_id` / `span_id` when they are 32 / 16 hex characters and not
//!   all zeros (stored lowercase; otherwise they stay in `fields`);
//! - every other key goes into `fields`.

use chrono::{DateTime, Duration, TimeZone, Utc};
use serde_json::{Map, Value};

/// Messages are cut to at most this many bytes, marker included.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
pub const TRUNCATION_MARKER: &str = "…[truncated]";
/// `fields`, serialized, never exceeds this many bytes.
pub const MAX_FIELDS_BYTES: usize = 256 * 1024;
/// Added to `fields` when keys had to be dropped to respect the cap.
pub const FIELDS_TRUNCATED_KEY: &str = "_nami_truncated";
/// Room kept for the [`FIELDS_TRUNCATED_KEY`] entry when dropping keys.
const MARKER_RESERVE: usize = 128;

pub const TIMESTAMP_KEYS: [&str; 3] = ["dt", "timestamp", "time"];
pub const MESSAGE_KEYS: [&str; 3] = ["message", "msg", "log"];
pub const LEVEL_KEYS: [&str; 3] = ["level", "severity", "lvl"];

/// Every level a stored row can have.
pub const LEVELS: [&str; 10] = [
    "trace",
    "debug",
    "info",
    "notice",
    "warn",
    "error",
    "critical",
    "alert",
    "emergency",
    "fatal",
];
pub const DEFAULT_LEVEL: &str = "info";

/// No source keeps rows longer than 365 days; an older timestamp is
/// treated as unusable instead of creating partitions TTL drops at once.
pub const MAX_PAST: Duration = Duration::days(365);
/// Clock skew allowance for timestamps ahead of the receive time.
pub const MAX_FUTURE: Duration = Duration::days(1);

#[derive(Debug, Clone, PartialEq)]
pub struct LogEvent {
    pub dt: DateTime<Utc>,
    pub level: String,
    pub message: String,
    pub fields: Map<String, Value>,
    /// 32 lowercase hex characters, or empty.
    pub trace_id: String,
    /// 16 lowercase hex characters, or empty.
    pub span_id: String,
}

impl LogEvent {
    /// Applies the size caps (message truncation, fields cap) and the
    /// millisecond precision of the `dt` column. Every way of building an
    /// event goes through here.
    pub fn new(
        dt: DateTime<Utc>,
        level: &str,
        message: String,
        fields: Map<String, Value>,
        trace_id: String,
        span_id: String,
    ) -> LogEvent {
        LogEvent {
            dt: truncate_to_millis(dt),
            level: level.to_string(),
            message: truncate_message(message),
            fields: cap_fields(fields),
            trace_id,
            span_id,
        }
    }
}

/// Maps one ingested JSON object (see the module docs).
pub fn from_json(mut object: Map<String, Value>, received_at: DateTime<Utc>) -> LogEvent {
    let dt = take_timestamp(&mut object, received_at).unwrap_or(received_at);
    let message = take_first(&mut object, &MESSAGE_KEYS)
        .map(message_text)
        .unwrap_or_default();
    let level = take_first(&mut object, &LEVEL_KEYS)
        .map(|value| normalize_level(&value))
        .unwrap_or(DEFAULT_LEVEL);
    let trace_id = take_hex_id(&mut object, "trace_id", 32);
    let span_id = take_hex_id(&mut object, "span_id", 16);
    LogEvent::new(dt, level, message, object, trace_id, span_id)
}

/// Removes and returns the value of the first of `keys` present.
fn take_first(object: &mut Map<String, Value>, keys: &[&str]) -> Option<Value> {
    let key = keys.iter().find(|key| object.contains_key(**key))?;
    object.remove(*key)
}

fn take_timestamp(
    object: &mut Map<String, Value>,
    received_at: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    for key in TIMESTAMP_KEYS {
        let Some(value) = object.get(key) else {
            continue;
        };
        if let Some(dt) = parse_timestamp(value).filter(|dt| in_accepted_range(*dt, received_at)) {
            object.remove(key);
            return Some(dt);
        }
    }
    None
}

/// RFC 3339 strings, and numbers as unix seconds (below 10^11, fractions
/// allowed) or milliseconds.
pub fn parse_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => DateTime::parse_from_rfc3339(text.trim())
            .ok()
            .map(|dt| dt.with_timezone(&Utc)),
        Value::Number(number) => {
            let raw = number.as_f64().filter(|v| v.is_finite())?;
            let millis = if raw.abs() < 1e11 { raw * 1000.0 } else { raw };
            // Far outside any accepted range; also keeps the cast exact.
            if millis.abs() > 1e15 {
                return None;
            }
            Utc.timestamp_millis_opt(millis.round() as i64).single()
        }
        _ => None,
    }
}

pub fn in_accepted_range(dt: DateTime<Utc>, received_at: DateTime<Utc>) -> bool {
    dt >= received_at - MAX_PAST && dt <= received_at + MAX_FUTURE
}

fn truncate_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(dt.timestamp_millis()).unwrap_or(dt)
}

/// Strings as they are; `null` as empty; anything else as its JSON text.
fn message_text(value: Value) -> String {
    match value {
        Value::String(text) => text,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Level names (case-insensitive, common aliases) and numbers (syslog
/// severities 0-7, pino/bunyan levels 10-60); anything else is `info`.
pub fn normalize_level(value: &Value) -> &'static str {
    match value {
        Value::String(name) => level_from_name(name),
        Value::Number(number) => number
            .as_u64()
            .map(level_from_number)
            .unwrap_or(DEFAULT_LEVEL),
        _ => DEFAULT_LEVEL,
    }
}

pub fn level_from_name(name: &str) -> &'static str {
    match name.trim().to_ascii_lowercase().as_str() {
        "trace" | "trc" => "trace",
        "debug" | "dbg" => "debug",
        "info" | "information" | "informational" => "info",
        "notice" => "notice",
        "warn" | "warning" => "warn",
        "error" | "err" => "error",
        "critical" | "crit" => "critical",
        "alert" => "alert",
        "emergency" | "emerg" => "emergency",
        "fatal" | "panic" => "fatal",
        _ => DEFAULT_LEVEL,
    }
}

/// Syslog severities 0-7 (RFC 5424) and pino/bunyan levels 10-60.
pub fn level_from_number(number: u64) -> &'static str {
    match number {
        0 => "emergency",
        1 => "alert",
        2 => "critical",
        3 => "error",
        4 => "warn",
        5 => "notice",
        6 => "info",
        7 => "debug",
        10 => "trace",
        20 => "debug",
        30 => "info",
        40 => "warn",
        50 => "error",
        60 => "fatal",
        _ => DEFAULT_LEVEL,
    }
}

/// `len` hex characters, not all zeros (W3C trace context rules).
pub fn is_hex_id(text: &str, len: usize) -> bool {
    text.len() == len
        && text.bytes().all(|b| b.is_ascii_hexdigit())
        && text.bytes().any(|b| b != b'0')
}

fn take_hex_id(object: &mut Map<String, Value>, key: &str, len: usize) -> String {
    let valid = matches!(object.get(key), Some(Value::String(text)) if is_hex_id(text, len));
    if !valid {
        return String::new();
    }
    match object.remove(key) {
        Some(Value::String(text)) => text.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Cuts the message to [`MAX_MESSAGE_BYTES`] (on a character boundary),
/// ending in [`TRUNCATION_MARKER`].
pub fn truncate_message(message: String) -> String {
    if message.len() <= MAX_MESSAGE_BYTES {
        return message;
    }
    let cut = message.floor_char_boundary(MAX_MESSAGE_BYTES - TRUNCATION_MARKER.len());
    let mut truncated = message;
    truncated.truncate(cut);
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

/// Keeps serialized `fields` within [`MAX_FIELDS_BYTES`]: when over, the
/// largest top-level keys are dropped first and [`FIELDS_TRUNCATED_KEY`]
/// says how many went.
pub fn cap_fields(mut fields: Map<String, Value>) -> Map<String, Value> {
    let total = json_len(&fields);
    if total <= MAX_FIELDS_BYTES {
        return fields;
    }
    // Each entry costs `"key":value` plus one separator.
    let mut entries: Vec<(usize, String)> = fields
        .iter()
        .map(|(key, value)| (json_len(key) + 1 + json_len(value) + 1, key.clone()))
        .collect();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let mut size = total;
    let mut dropped = 0usize;
    for (entry_len, key) in entries {
        if size + MARKER_RESERVE <= MAX_FIELDS_BYTES {
            break;
        }
        fields.remove(&key);
        size = size.saturating_sub(entry_len);
        dropped += 1;
    }
    fields.insert(
        FIELDS_TRUNCATED_KEY.to_string(),
        Value::String(format!(
            "{dropped} field(s) dropped: event fields are capped at 256 KiB"
        )),
    );
    fields
}

fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn received() -> DateTime<Utc> {
        "2026-10-13T12:00:00Z".parse().unwrap()
    }

    fn event(value: Value) -> LogEvent {
        from_json(value.as_object().unwrap().clone(), received())
    }

    #[test]
    fn maps_the_standard_keys_and_keeps_everything_else_in_fields() {
        let e = event(json!({
            "dt": "2026-10-13T11:59:58.250+02:00",
            "message": "user 42 logged in",
            "level": "WARNING",
            "user": { "id": 42 },
            "status": 200
        }));
        assert_eq!(
            e.dt,
            "2026-10-13T09:59:58.250Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(e.message, "user 42 logged in");
        assert_eq!(e.level, "warn");
        assert_eq!(
            Value::Object(e.fields),
            json!({ "user": { "id": 42 }, "status": 200 })
        );
        assert_eq!((e.trace_id.as_str(), e.span_id.as_str()), ("", ""));
    }

    #[test]
    fn timestamp_keys_are_tried_in_order_and_unix_seconds_or_millis_are_accepted() {
        assert_eq!(
            event(json!({ "timestamp": 1_791_892_799 })).dt,
            "2026-10-13T11:59:59Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            event(json!({ "time": 1_791_892_799_123_u64 })).dt,
            "2026-10-13T11:59:59.123Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            event(json!({ "time": 1_791_892_799.5 })).dt,
            "2026-10-13T11:59:59.500Z".parse::<DateTime<Utc>>().unwrap()
        );
        let both = event(json!({ "dt": "not a time", "timestamp": "2026-10-13T11:00:00Z" }));
        assert_eq!(
            both.dt,
            "2026-10-13T11:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            Value::Object(both.fields),
            json!({ "dt": "not a time" }),
            "the unusable key stays"
        );
    }

    #[test]
    fn missing_unparsable_or_out_of_range_timestamps_fall_back_to_the_receive_time() {
        for value in [
            json!({}),
            json!({ "dt": "yesterday" }),
            json!({ "dt": true }),
            json!({ "dt": "2024-01-01T00:00:00Z" }),
            json!({ "dt": "2026-10-15T00:00:00Z" }),
            json!({ "time": 1e300 }),
            json!({ "time": -5 }),
        ] {
            let e = event(value.clone());
            assert_eq!(e.dt, received(), "{value}");
            if let Some(key) = value.as_object().unwrap().keys().next() {
                assert!(
                    e.fields.contains_key(key),
                    "{value}: the original stays in fields"
                );
            }
        }
    }

    #[test]
    fn dt_is_kept_at_millisecond_precision() {
        let e = event(json!({ "dt": "2026-10-13T11:59:59.123456789Z" }));
        assert_eq!(e.dt.timestamp_subsec_nanos(), 123_000_000);
    }

    #[test]
    fn message_keys_are_tried_in_order_and_non_strings_become_json_text() {
        assert_eq!(event(json!({ "msg": "a" })).message, "a");
        assert_eq!(event(json!({ "log": "b\n" })).message, "b\n");
        let both = event(json!({ "msg": "second", "message": "first" }));
        assert_eq!(both.message, "first");
        assert_eq!(Value::Object(both.fields), json!({ "msg": "second" }));
        assert_eq!(
            event(json!({ "message": { "a": 1 } })).message,
            r#"{"a":1}"#
        );
        assert_eq!(event(json!({ "message": 7 })).message, "7");
        assert_eq!(event(json!({ "message": null })).message, "");
        assert_eq!(event(json!({ "other": 1 })).message, "");
    }

    #[test]
    fn levels_are_normalized_and_unknown_levels_are_info() {
        for (raw, expected) in [
            (json!("TRACE"), "trace"),
            (json!("dbg"), "debug"),
            (json!(" Info "), "info"),
            (json!("notice"), "notice"),
            (json!("Warning"), "warn"),
            (json!("ERR"), "error"),
            (json!("crit"), "critical"),
            (json!("alert"), "alert"),
            (json!("emerg"), "emergency"),
            (json!("panic"), "fatal"),
            (json!("verbose"), "info"),
            (json!(3), "error"),
            (json!(50), "error"),
            (json!(60), "fatal"),
            (json!(35), "info"),
            (json!(-1), "info"),
            (json!(true), "info"),
            (json!(null), "info"),
        ] {
            assert_eq!(
                event(json!({ "level": raw.clone() })).level,
                expected,
                "{raw}"
            );
        }
        let e = event(json!({ "severity": "error", "lvl": "debug" }));
        assert_eq!(e.level, "error");
        assert_eq!(Value::Object(e.fields), json!({ "lvl": "debug" }));
        assert_eq!(event(json!({})).level, "info");
        for level in LEVELS {
            assert_eq!(
                level_from_name(level),
                level,
                "every stored level maps to itself"
            );
        }
    }

    #[test]
    fn trace_and_span_ids_are_taken_when_valid_and_lowercased() {
        let e = event(json!({
            "trace_id": "4BF92F3577B34DA6A3CE929D0E0E4736",
            "span_id": "00f067aa0ba902b7"
        }));
        assert_eq!(e.trace_id, "4bf92f3577b34da6a3ce929d0e0e4736");
        assert_eq!(e.span_id, "00f067aa0ba902b7");
        assert!(e.fields.is_empty());
        for (trace_id, span_id) in [
            (
                json!("00000000000000000000000000000000"),
                json!("0000000000000000"),
            ),
            (
                json!("4bf92f3577b34da6a3ce929d0e0e473"),
                json!("00f067aa0ba902b"),
            ),
            (
                json!("4bf92f3577b34da6a3ce929d0e0e473g"),
                json!("00f067aa0ba902bz"),
            ),
            (json!(42), json!(null)),
        ] {
            let e = event(json!({ "trace_id": trace_id.clone(), "span_id": span_id.clone() }));
            assert_eq!(
                (e.trace_id.as_str(), e.span_id.as_str()),
                ("", ""),
                "{trace_id} {span_id}"
            );
            assert_eq!(
                e.fields.get("trace_id"),
                Some(&trace_id),
                "invalid ids stay in fields"
            );
            assert_eq!(e.fields.get("span_id"), Some(&span_id));
        }
    }

    #[test]
    fn long_messages_are_truncated_on_a_char_boundary_with_a_marker() {
        let exact = "a".repeat(MAX_MESSAGE_BYTES);
        assert_eq!(truncate_message(exact.clone()), exact, "64 KiB fits");
        let long = "é".repeat(MAX_MESSAGE_BYTES);
        let truncated = event(json!({ "message": long })).message;
        assert!(truncated.len() <= MAX_MESSAGE_BYTES, "{}", truncated.len());
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        assert!(
            truncated
                .trim_end_matches(TRUNCATION_MARKER)
                .chars()
                .all(|c| c == 'é')
        );
    }

    #[test]
    fn oversized_fields_drop_their_largest_keys_and_say_so() {
        let e = event(json!({
            "message": "m",
            "stack": "x".repeat(300 * 1024),
            "user_id": 42,
            "path": "/api"
        }));
        assert!(!e.fields.contains_key("stack"));
        assert_eq!(e.fields["user_id"], 42);
        assert_eq!(e.fields["path"], "/api");
        assert_eq!(
            e.fields[FIELDS_TRUNCATED_KEY],
            "1 field(s) dropped: event fields are capped at 256 KiB"
        );
        assert!(serde_json::to_vec(&e.fields).unwrap().len() <= MAX_FIELDS_BYTES);
    }

    #[test]
    fn many_small_fields_are_capped_too() {
        let mut object = Map::new();
        for i in 0..40_000 {
            object.insert(format!("key{i:05}"), json!(i));
        }
        let e = from_json(object, received());
        let len = serde_json::to_vec(&e.fields).unwrap().len();
        assert!(len <= MAX_FIELDS_BYTES, "{len}");
        assert!(
            len > MAX_FIELDS_BYTES - 1024,
            "keeps as much as fits: {len}"
        );
        assert!(e.fields.contains_key(FIELDS_TRUNCATED_KEY));
    }

    #[test]
    fn fields_under_the_cap_are_untouched() {
        let e = event(json!({ "big": "x".repeat(200 * 1024) }));
        assert_eq!(e.fields.len(), 1);
        assert!(!e.fields.contains_key(FIELDS_TRUNCATED_KEY));
    }
}
