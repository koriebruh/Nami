//! Field mapping and size caps for one ingested event. Every ingest path
//! (HTTP JSON, NDJSON and MessagePack, syslog, and later OTLP logs) ends in
//! a [`LogEvent`] built here; `store::build_row` then turns it into a row,
//! applying the size caps, the `dt` window ([`clamp_to_accepted_range`]) and
//! the level and id checks ([`level_from_name`], [`checked_id`]) once more,
//! so a row respects them whatever built or edited the event.
//!
//! Mapping of a JSON object (`from_json`):
//! - timestamp from the first of `dt` | `timestamp` | `time` that parses
//!   (RFC 3339, or unix seconds / milliseconds) and lies within
//!   [`MAX_PAST`] / [`MAX_FUTURE`] of the receive time, else the receive
//!   time (and the unusable key stays in `fields`);
//! - message from the first present of `message` | `msg` | `log`;
//! - level from the first present of `level` | `severity` | `lvl`,
//!   normalized to one of [`LEVELS`] (unknown -> `info`, and the value stays
//!   in `fields` under its own key, as sent);
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

/// A stored `trace_id` is 32 hex characters (W3C trace context), a stored
/// `span_id` 16.
pub const TRACE_ID_LEN: usize = 32;
pub const SPAN_ID_LEN: usize = 16;

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
    /// event goes through here; `level`, `trace_id` and `span_id` are taken
    /// as given, so callers outside [`from_json`] normalize them first
    /// ([`level_from_name`], [`level_from_number`], [`checked_id`]), and
    /// `store::build_row` checks them again before a row is built.
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
    let level = take_level(&mut object);
    let trace_id = take_hex_id(&mut object, "trace_id", TRACE_ID_LEN);
    let span_id = take_hex_id(&mut object, "span_id", SPAN_ID_LEN);
    LogEvent::new(dt, level, message, object, trace_id, span_id)
}

/// Removes and returns the value of the first of `keys` present.
fn take_first(object: &mut Map<String, Value>, keys: &[&'static str]) -> Option<Value> {
    take_first_entry(object, keys).map(|(_, value)| value)
}

/// [`take_first`] that also says which key it was.
fn take_first_entry(
    object: &mut Map<String, Value>,
    keys: &[&'static str],
) -> Option<(&'static str, Value)> {
    let key = *keys.iter().find(|key| object.contains_key(**key))?;
    object.remove(key).map(|value| (key, value))
}

/// The level of the first of [`LEVEL_KEYS`] present. A value that is not a
/// known level (see [`known_level`]) gives `info` and goes back into
/// `fields` under its own key, so nothing the sender wrote is lost.
fn take_level(object: &mut Map<String, Value>) -> &'static str {
    let Some((key, value)) = take_first_entry(object, &LEVEL_KEYS) else {
        return DEFAULT_LEVEL;
    };
    known_level(&value).unwrap_or_else(|| {
        object.insert(key.to_string(), value);
        DEFAULT_LEVEL
    })
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

/// Forces `dt` into the window of [`in_accepted_range`] around `now` (the
/// insert time), on the millisecond grid of the `dt` column. The bounds are
/// rounded inward, so `in_accepted_range(result, now)` always holds.
///
/// The table's TTL goes through ClickHouse's `toDateTime`, which wraps around
/// outside 1970 to 2106 (a `dt` in 1900 becomes 2036, one in 2200 becomes
/// 2063), so a wild `dt` would keep a row decades beyond its retention.
pub fn clamp_to_accepted_range(dt: DateTime<Utc>, now: DateTime<Utc>) -> DateTime<Utc> {
    let oldest = ceil_to_millis(now - MAX_PAST);
    let newest = truncate_to_millis(now + MAX_FUTURE);
    truncate_to_millis(dt).clamp(oldest, newest)
}

fn truncate_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(dt.timestamp_millis()).unwrap_or(dt)
}

fn ceil_to_millis(dt: DateTime<Utc>) -> DateTime<Utc> {
    let floor = truncate_to_millis(dt);
    if floor < dt {
        floor + Duration::milliseconds(1)
    } else {
        floor
    }
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
    known_level(value).unwrap_or(DEFAULT_LEVEL)
}

/// The stored level a level value stands for, or `None` when it is neither
/// a level name nor a level number [`normalize_level`] knows. It tells an
/// unknown level from `info`, for paths that keep what they cannot map.
pub fn known_level(value: &Value) -> Option<&'static str> {
    match value {
        Value::String(name) => parse_level_name(name),
        Value::Number(number) => number.as_u64().and_then(parse_level_number),
        _ => None,
    }
}

pub fn level_from_name(name: &str) -> &'static str {
    parse_level_name(name).unwrap_or(DEFAULT_LEVEL)
}

fn parse_level_name(name: &str) -> Option<&'static str> {
    Some(match name.trim().to_ascii_lowercase().as_str() {
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
        _ => return None,
    })
}

/// Syslog severities 0-7 (RFC 5424) and pino/bunyan levels 10-60.
pub fn level_from_number(number: u64) -> &'static str {
    parse_level_number(number).unwrap_or(DEFAULT_LEVEL)
}

fn parse_level_number(number: u64) -> Option<&'static str> {
    Some(match number {
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
        _ => return None,
    })
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

/// `id` as it may be stored: lowercase when it is `len` hex characters (see
/// [`is_hex_id`]), empty when it is empty. Anything else is no id: it moves
/// into `fields` under `key` (unless the event already has a field of that
/// name, which stays), so nothing the sender wrote is lost, and the caller
/// applies the fields cap afterwards. The stored column then holds at most
/// `len` characters whatever built the event.
pub fn checked_id(id: String, len: usize, key: &str, fields: &mut Map<String, Value>) -> String {
    if id.is_empty() {
        return id;
    }
    if is_hex_id(&id, len) {
        return id.to_ascii_lowercase();
    }
    fields.entry(key).or_insert(Value::String(id));
    String::new()
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

/// The length of `value` as JSON text, counted while it is written and
/// thrown away: measuring a field must not build a copy of it (a field the
/// size of the whole event cap, 50 MiB of decoded values, would make one as
/// large as its text).
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> usize {
    struct Count(usize);

    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut count = Count(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        Err(_) => 0,
    }
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

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // The cut point (64 KiB minus the marker) falls inside a character
        // for each of these, which a plain byte cut would panic on.
        for message in [
            format!("a{}", "é".repeat(70_000)),
            "€".repeat(30_000),
            "😀".repeat(20_000),
        ] {
            let truncated = truncate_message(message.clone());
            assert!(truncated.len() <= MAX_MESSAGE_BYTES, "{}", truncated.len());
            let body = truncated
                .strip_suffix(TRUNCATION_MARKER)
                .expect("ends with the marker");
            assert!(message.starts_with(body), "a prefix of the original");
            let next = message[body.len()..].chars().next().unwrap();
            assert!(
                truncated.len() + next.len_utf8() > MAX_MESSAGE_BYTES,
                "keeps as much as fits: the next character would still have fitted"
            );
        }
    }

    #[test]
    fn a_level_the_mapping_does_not_know_stays_in_fields_as_sent() {
        for (key, raw) in [
            ("level", json!("verbose")),
            ("severity", json!("SEV9")),
            ("lvl", json!(35)),
            ("level", json!(-1)),
            ("level", json!(1.5)),
            ("level", json!(true)),
            ("level", json!(null)),
            ("level", json!("")),
            ("level", json!({ "n": 1 })),
        ] {
            let e = event(json!({ key: raw.clone() }));
            assert_eq!(e.level, "info", "{raw}");
            assert_eq!(e.fields.get(key), Some(&raw), "{key}: the raw value stays");
        }
        for (raw, level) in [
            (json!("WARNING"), "warn"),
            (json!(50), "error"),
            (json!("info"), "info"),
        ] {
            let e = event(json!({ "level": raw.clone() }));
            assert_eq!(e.level, level, "{raw}");
            assert!(e.fields.is_empty(), "{raw}: a known level is consumed");
        }
        // Only the first level key counts: an unknown value there does not
        // let the next key win, and both stay.
        let e = event(json!({ "level": "verbose", "severity": "error" }));
        assert_eq!(e.level, "info");
        assert_eq!(
            Value::Object(e.fields),
            json!({ "level": "verbose", "severity": "error" })
        );
    }

    #[test]
    fn known_level_tells_an_unknown_level_from_info() {
        assert_eq!(known_level(&json!("info")), Some("info"));
        assert_eq!(known_level(&json!(" ERR ")), Some("error"));
        assert_eq!(known_level(&json!(6)), Some("info"));
        assert_eq!(known_level(&json!("verbose")), None);
        assert_eq!(known_level(&json!(35)), None);
        assert_eq!(known_level(&json!(null)), None);
        assert_eq!(normalize_level(&json!("verbose")), "info");
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn dt_is_clamped_into_the_accepted_window_at_millisecond_precision() {
        let now = received();
        let millisecond = Duration::milliseconds(1);
        let oldest = now - MAX_PAST;
        let newest = now + MAX_FUTURE;
        for (dt, expected) in [
            (at("1900-01-01T00:00:00Z"), oldest),
            (at("2200-01-01T00:00:00Z"), newest),
            (DateTime::<Utc>::MIN_UTC, oldest),
            (DateTime::<Utc>::MAX_UTC, newest),
            (oldest - millisecond, oldest),
            (newest + millisecond, newest),
            (oldest, oldest),
            (newest, newest),
            (now, now),
            (
                at("2026-10-13T11:59:58.250Z"),
                at("2026-10-13T11:59:58.250Z"),
            ),
            (
                at("2026-10-13T11:59:59.123456789Z"),
                at("2026-10-13T11:59:59.123Z"),
            ),
        ] {
            let clamped = clamp_to_accepted_range(dt, now);
            assert_eq!(clamped, expected, "{dt}");
            assert!(in_accepted_range(clamped, now), "{dt}");
        }
    }

    #[test]
    fn the_clamped_window_stays_inside_the_accepted_range_for_any_reference_time() {
        // A reference time with sub-millisecond digits must not push the
        // millisecond-grid bounds outside `in_accepted_range`.
        for now in [
            at("2026-10-13T12:00:00.000000001Z"),
            at("2026-10-13T12:00:00.123456789Z"),
            at("2026-10-13T12:00:00.999999999Z"),
        ] {
            for dt in [at("1900-01-01T00:00:00Z"), at("2200-01-01T00:00:00Z")] {
                let clamped = clamp_to_accepted_range(dt, now);
                assert!(
                    in_accepted_range(clamped, now),
                    "{dt} against {now}: {clamped}"
                );
                assert_eq!(clamped.timestamp_subsec_nanos() % 1_000_000, 0, "{clamped}");
            }
        }
    }

    #[test]
    fn checked_id_keeps_valid_ids_in_lowercase_and_moves_anything_else_to_fields() {
        let mut fields = Map::new();
        assert_eq!(
            checked_id(String::new(), TRACE_ID_LEN, "trace_id", &mut fields),
            ""
        );
        assert!(fields.is_empty(), "an empty id is simply empty");
        assert_eq!(
            checked_id(
                "4BF92F3577B34DA6A3CE929D0E0E4736".to_string(),
                TRACE_ID_LEN,
                "trace_id",
                &mut fields
            ),
            "4bf92f3577b34da6a3ce929d0e0e4736"
        );
        assert!(fields.is_empty(), "a valid id stays out of the fields");

        // A span id is no trace id, and a long id is no id at all.
        for (id, len, key) in [
            ("00f067aa0ba902b7", TRACE_ID_LEN, "trace_id"),
            ("4bf92f3577b34da6a3ce929d0e0e4736", SPAN_ID_LEN, "span_id"),
            ("00000000000000000000000000000000", TRACE_ID_LEN, "trace_id"),
            ("not an id", SPAN_ID_LEN, "span_id"),
        ] {
            let mut fields = Map::new();
            assert_eq!(
                checked_id(id.to_string(), len, key, &mut fields),
                "",
                "{id}"
            );
            assert_eq!(fields.get(key), Some(&json!(id)), "{id}: the value is kept");
        }

        // A field of the same name that is already there stays.
        let mut fields = Map::new();
        fields.insert("span_id".to_string(), json!("from the sender"));
        assert_eq!(
            checked_id("nope".to_string(), SPAN_ID_LEN, "span_id", &mut fields),
            ""
        );
        assert_eq!(fields["span_id"], "from the sender");
    }
}
