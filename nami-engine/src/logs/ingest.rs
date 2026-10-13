//! HTTP ingest bodies: authentication header, content negotiation, size
//! limits, gzip, and parsing into events. The handler is
//! `routes::logs_ingest`; everything here except [`read_body`] is
//! synchronous, and [`rows_from_body`] runs it on a blocking thread.
//!
//! Formats: JSON — one object, an array of objects, or several of those
//! separated by whitespace, which covers NDJSON (`application/x-ndjson`)
//! and shippers that send NDJSON as `application/json` — and MessagePack
//! (`application/msgpack`, `application/x-msgpack`): one map or an array of
//! maps. Either may be gzip-compressed (`Content-Encoding: gzip`).

use std::{fmt, io::Read, time::Duration};

use axum::{
    Json,
    body::{Body, Bytes},
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{
            AUTHORIZATION, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER,
            WWW_AUTHENTICATE,
        },
    },
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value, json};

use super::{
    msgpack::{self, MsgpackError},
    normalize,
    rate_limit::REQUESTS_PER_SECOND,
    store::{self, LogRow},
};
use crate::{error::AppError, models::source::IngestSource};

/// Largest accepted request body, as sent (compressed or not).
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// Largest accepted body after gzip decompression (gzip-bomb guard).
pub const MAX_DECOMPRESSED_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_EVENTS: usize = 10_000;
/// The longest a request may take to deliver its body. A request holds one of
/// the few ingest places while it does (see
/// `routes::logs_ingest::MAX_INGEST_IN_FLIGHT`), and a client that stalls
/// (a dead connection that never closes, or one that sends a byte a minute)
/// must not keep it for ever: the full 10 MiB still fits at about 1.4 Mbit/s.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// A parse error that quotes the body (serde_json does for a value of the
/// wrong type) is cut to this many bytes: a body that is one huge string
/// must not come back as one huge error message.
const MAX_ERROR_DETAIL_BYTES: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    #[error("unauthorized")]
    Unauthorized,
    /// Every ingest place is taken: answered at once, before any database
    /// work (see `routes::logs_ingest::MAX_INGEST_IN_FLIGHT`).
    #[error("busy, retry shortly")]
    Busy,
    #[error("rate limit exceeded")]
    RateLimited,
    /// The body did not arrive within [`BODY_READ_TIMEOUT`].
    #[error("timed out waiting for the request body")]
    RequestTimeout,
    #[error("daily byte quota exceeded")]
    QuotaExceeded { retry_after_seconds: u64 },
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    UnsupportedMediaType(String),
    #[error("{0}")]
    BadRequest(String),
    /// A failure already logged where it happened; the client sees a
    /// generic 500.
    #[error("internal error")]
    Internal,
    #[error(transparent)]
    App(#[from] AppError),
}

impl From<sqlx::Error> for IngestError {
    fn from(error: sqlx::Error) -> Self {
        IngestError::App(AppError::Database(error))
    }
}

impl From<crate::clickhouse::ClickHouseError> for IngestError {
    fn from(error: crate::clickhouse::ClickHouseError) -> Self {
        IngestError::App(AppError::ClickHouse(error))
    }
}

impl IntoResponse for IngestError {
    fn into_response(self) -> Response {
        let (status, body, retry_after) = match self {
            IngestError::Unauthorized => {
                // The same generic answer for every credential that is not a
                // live source's; the header is what RFC 9110 requires of a 401.
                let mut response = AppError::Unauthorized.into_response();
                response
                    .headers_mut()
                    .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
                return response;
            }
            IngestError::App(error) => return error.into_response(),
            IngestError::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                json!({ "error": "busy, retry shortly", "code": "busy" }),
                Some(1),
            ),
            IngestError::RequestTimeout => (
                StatusCode::REQUEST_TIMEOUT,
                json!({
                    "error": "timed out waiting for the request body",
                    "code": "request_timeout"
                }),
                None,
            ),
            IngestError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "error": format!(
                        "rate limit exceeded: at most {REQUESTS_PER_SECOND} requests per second per source"
                    ),
                    "code": "rate_limited"
                }),
                Some(1),
            ),
            IngestError::QuotaExceeded {
                retry_after_seconds,
            } => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({ "error": "this source has reached its daily byte quota", "code": "quota_exceeded" }),
                Some(retry_after_seconds),
            ),
            IngestError::PayloadTooLarge(message) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                json!({ "error": message }),
                None,
            ),
            IngestError::UnsupportedMediaType(message) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                json!({ "error": message }),
                None,
            ),
            IngestError::BadRequest(message) => {
                (StatusCode::BAD_REQUEST, json!({ "error": message }), None)
            }
            IngestError::Internal => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": "internal error" }),
                None,
            ),
        };
        let mut response = (status, Json(body)).into_response();
        if let Some(seconds) = retry_after {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyFormat {
    Json,
    MessagePack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyEncoding {
    pub format: BodyFormat,
    pub gzip: bool,
}

/// The token of an `Authorization: Bearer <token>` header (scheme
/// case-insensitive).
pub fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    let token = token.trim();
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

fn header(headers: &HeaderMap, name: axum::http::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
}

pub fn body_encoding(headers: &HeaderMap) -> Result<BodyEncoding, IngestError> {
    let gzip = match header(headers, CONTENT_ENCODING).as_deref() {
        None | Some("") | Some("identity") => false,
        Some("gzip") | Some("x-gzip") => true,
        Some(_) => {
            return Err(IngestError::UnsupportedMediaType(
                "unsupported Content-Encoding: send gzip or an uncompressed body".to_string(),
            ));
        }
    };
    let media_type = header(headers, CONTENT_TYPE)
        .map(|value| value.split(';').next().unwrap_or("").trim().to_string());
    let format = match media_type.as_deref() {
        None
        | Some("")
        | Some("application/json")
        | Some("text/json")
        | Some("text/plain")
        | Some("application/x-www-form-urlencoded")
        | Some("application/x-ndjson")
        | Some("application/ndjson")
        | Some("application/jsonl")
        | Some("application/jsonlines")
        | Some("application/x-jsonlines") => BodyFormat::Json,
        Some("application/msgpack")
        | Some("application/x-msgpack")
        | Some("application/vnd.msgpack") => BodyFormat::MessagePack,
        Some(_) => {
            return Err(IngestError::UnsupportedMediaType(
                "unsupported Content-Type: send application/json, application/x-ndjson or application/msgpack"
                    .to_string(),
            ));
        }
    };
    Ok(BodyEncoding { format, gzip })
}

fn body_too_large() -> IngestError {
    IngestError::PayloadTooLarge(format!(
        "request body exceeds {} MiB",
        MAX_BODY_BYTES / (1024 * 1024)
    ))
}

/// Reads at most [`MAX_BODY_BYTES`], all of it within `timeout` (408
/// otherwise, see [`BODY_READ_TIMEOUT`]); a larger declared `Content-Length`
/// is refused before reading anything.
pub async fn read_body(
    body: Body,
    headers: &HeaderMap,
    timeout: Duration,
) -> Result<Bytes, IngestError> {
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return Err(body_too_large());
    }
    tokio::time::timeout(timeout, axum::body::to_bytes(body, MAX_BODY_BYTES))
        .await
        .map_err(|_elapsed| IngestError::RequestTimeout)?
        .map_err(|error| {
            if is_length_limit(&error) {
                body_too_large()
            } else {
                IngestError::BadRequest("could not read the request body".to_string())
            }
        })
}

fn is_length_limit(error: &axum::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(e) = current {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        current = e.source();
    }
    false
}

/// Turns a body into the rows to store for `source`: decompress, parse,
/// normalize ([`build_rows`]), on a blocking thread. That work is CPU-bound,
/// about 50 ms for a typical batch of 10,000 events and up to a second for
/// one of large messages, and on an async worker it would stall every other
/// request that worker serves.
pub async fn rows_from_body(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
) -> Result<Vec<LogRow>, IngestError> {
    let source = source.clone();
    tokio::task::spawn_blocking(move || build_rows(body, encoding, &source, received_at))
        .await
        .map_err(|error| {
            tracing::error!(%error, "log ingest worker failed");
            IngestError::Internal
        })?
}

/// The synchronous part of [`rows_from_body`].
pub fn build_rows(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
) -> Result<Vec<LogRow>, IngestError> {
    Ok(decode_events(body, encoding)?
        .into_iter()
        .map(|object| store::build_row(normalize::from_json(object, received_at), source))
        .collect())
}

/// Decompresses (when gzip) and parses a body into event objects.
pub fn decode_events(
    body: Bytes,
    encoding: BodyEncoding,
) -> Result<Vec<Map<String, Value>>, IngestError> {
    let raw = if encoding.gzip {
        Bytes::from(gunzip(&body)?)
    } else {
        body
    };
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Err(IngestError::BadRequest("request body is empty".to_string()));
    }
    match encoding.format {
        BodyFormat::Json => parse_json_events(&raw),
        BodyFormat::MessagePack => {
            msgpack::decode_events(&raw, MAX_EVENTS).map_err(|error| match error {
                MsgpackError::TooManyEvents => too_many_events(),
                other => IngestError::BadRequest(format!("invalid MessagePack body: {other}")),
            })
        }
    }
}

fn too_many_events() -> IngestError {
    IngestError::PayloadTooLarge(format!("too many events: at most {MAX_EVENTS} per request"))
}

/// gzip (multi-member) with the output capped at [`MAX_DECOMPRESSED_BYTES`].
pub fn gunzip(compressed: &[u8]) -> Result<Vec<u8>, IngestError> {
    let mut out = Vec::new();
    flate2::read::MultiGzDecoder::new(compressed)
        .take(MAX_DECOMPRESSED_BYTES as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| IngestError::BadRequest(format!("invalid gzip body: {e}")))?;
    if out.len() > MAX_DECOMPRESSED_BYTES {
        return Err(IngestError::PayloadTooLarge(format!(
            "decompressed request body exceeds {} MiB",
            MAX_DECOMPRESSED_BYTES / (1024 * 1024)
        )));
    }
    Ok(out)
}

/// A whitespace-separated sequence of JSON objects or arrays of objects,
/// failing fast on the event after the [`MAX_EVENTS`]th.
pub fn parse_json_events(raw: &[u8]) -> Result<Vec<Map<String, Value>>, IngestError> {
    let mut events = Vec::new();
    let mut overflow = false;
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    // `end` succeeds once only whitespace is left; otherwise it has skipped
    // the whitespace before the next value.
    while deserializer.end().is_err() {
        let seed = TopLevel {
            events: &mut events,
            overflow: &mut overflow,
        };
        if let Err(error) = seed.deserialize(&mut deserializer) {
            return Err(if overflow {
                too_many_events()
            } else {
                invalid_json(&error)
            });
        }
    }
    Ok(events)
}

/// The 400 for a body serde_json could not read. Its message is cut (see
/// [`MAX_ERROR_DETAIL_BYTES`]); a cut message keeps the position, which
/// serde_json puts last.
fn invalid_json(error: &serde_json::Error) -> IngestError {
    let text = error.to_string();
    let detail = if text.len() > MAX_ERROR_DETAIL_BYTES {
        let mut end = MAX_ERROR_DETAIL_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}… at line {} column {}",
            &text[..end],
            error.line(),
            error.column()
        )
    } else {
        text
    };
    IngestError::BadRequest(format!("invalid JSON body: {detail}"))
}

/// One top-level JSON value: an object (one event) or an array of objects.
struct TopLevel<'a> {
    events: &'a mut Vec<Map<String, Value>>,
    overflow: &'a mut bool,
}

impl TopLevel<'_> {
    fn push<E: de::Error>(&mut self, event: Map<String, Value>) -> Result<(), E> {
        if self.events.len() >= MAX_EVENTS {
            *self.overflow = true;
            return Err(E::custom("too many events"));
        }
        self.events.push(event);
        Ok(())
    }
}

impl<'de> DeserializeSeed<'de> for TopLevel<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for TopLevel<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object or an array of objects")
    }

    fn visit_map<A: MapAccess<'de>>(mut self, map: A) -> Result<(), A::Error> {
        let event = serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
        self.push(event)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while let Some(event) = seq.next_element::<Map<String, Value>>()? {
            self.push(event)?;
        }
        Ok(())
    }
}

/// Seconds until the next UTC midnight (at least 1), for `Retry-After` on a
/// quota rejection.
pub fn seconds_until_utc_midnight(now: DateTime<Utc>) -> u64 {
    let next_midnight = now
        .date_naive()
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|midnight| midnight.and_utc())
        .unwrap_or(now);
    (next_midnight - now).num_seconds().max(1) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn bearer_tokens_are_read_case_insensitively() {
        assert_eq!(
            bearer_token(&headers(&[("authorization", "Bearer abc")])),
            Some("abc")
        );
        assert_eq!(
            bearer_token(&headers(&[("authorization", "bearer   abc  ")])),
            Some("abc")
        );
        for value in ["Basic abc", "Bearer", "Bearer ", "abc", "Token abc"] {
            assert_eq!(
                bearer_token(&headers(&[("authorization", value)])),
                None,
                "{value}"
            );
        }
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn content_types_and_encodings_select_the_parser() {
        let json = BodyEncoding {
            format: BodyFormat::Json,
            gzip: false,
        };
        for content_type in [
            None,
            Some("application/json; charset=utf-8"),
            Some("application/x-ndjson"),
            Some("text/plain"),
        ] {
            let h = content_type
                .map(|ct| headers(&[("content-type", ct)]))
                .unwrap_or_default();
            assert_eq!(body_encoding(&h).unwrap(), json, "{content_type:?}");
        }
        for content_type in ["application/msgpack", "Application/X-MsgPack"] {
            let h = headers(&[("content-type", content_type), ("content-encoding", "gzip")]);
            assert_eq!(
                body_encoding(&h).unwrap(),
                BodyEncoding {
                    format: BodyFormat::MessagePack,
                    gzip: true
                }
            );
        }
        for (name, value) in [
            ("content-type", "application/x-protobuf"),
            ("content-encoding", "br"),
        ] {
            assert!(matches!(
                body_encoding(&headers(&[(name, value)])),
                Err(IngestError::UnsupportedMediaType(_))
            ));
        }
    }

    #[test]
    fn json_bodies_may_be_objects_arrays_or_ndjson() {
        let events = parse_json_events(br#"{"a":1}"#).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(parse_json_events(br#"[{"a":1},{"b":2}]"#).unwrap().len(), 2);
        let ndjson = b"{\"a\":1}\n\n{\"b\":2}\r\n[{\"c\":3}]\n";
        assert_eq!(parse_json_events(ndjson).unwrap().len(), 3);
        assert_eq!(parse_json_events(b"[]").unwrap().len(), 0);
    }

    #[test]
    fn malformed_json_bodies_are_bad_requests() {
        for body in [
            &b"{"[..],
            b"\"text\"",
            b"42",
            b"[1]",
            b"[{\"a\":1}, 2]",
            b"{\"a\":1} x",
            b"{\"a\":1}}",
        ] {
            match parse_json_events(body) {
                Err(IngestError::BadRequest(message)) => {
                    assert!(message.starts_with("invalid JSON body"), "{message}")
                }
                other => panic!("{:?}: {other:?}", String::from_utf8_lossy(body)),
            }
        }
    }

    #[test]
    fn a_json_error_that_quotes_the_body_is_cut_but_keeps_its_position() {
        let long = format!("\"{}\"", "é".repeat(100_000));
        let Err(IngestError::BadRequest(message)) = parse_json_events(long.as_bytes()) else {
            panic!("not a bad request");
        };
        assert!(message.starts_with("invalid JSON body: invalid type: string"));
        assert!(message.len() < 300, "{} bytes", message.len());
        assert!(
            message.ends_with("at line 1 column 200002"),
            "the position survives the cut: {message}"
        );
        // A short message is not touched.
        let Err(IngestError::BadRequest(message)) = parse_json_events(b"\"short\"") else {
            panic!("not a bad request");
        };
        assert_eq!(
            message,
            "invalid JSON body: invalid type: string \"short\", expected a JSON object or an array of objects at line 1 column 7"
        );
    }

    #[test]
    fn the_event_after_the_limit_is_413_whatever_the_framing() {
        let array = format!("[{}]", vec!["{}"; MAX_EVENTS].join(","));
        assert_eq!(
            parse_json_events(array.as_bytes()).unwrap().len(),
            MAX_EVENTS
        );
        let over = format!("[{}]", vec!["{}"; MAX_EVENTS + 1].join(","));
        assert!(matches!(
            parse_json_events(over.as_bytes()),
            Err(IngestError::PayloadTooLarge(_))
        ));
        let ndjson = "{}\n".repeat(MAX_EVENTS + 1);
        assert!(matches!(
            parse_json_events(ndjson.as_bytes()),
            Err(IngestError::PayloadTooLarge(_))
        ));
    }

    #[test]
    fn json_nesting_follows_the_shared_limit() {
        let nested = |depth: usize| {
            format!(
                "{{\"a\":{}{}}}",
                "[".repeat(depth - 1),
                "]".repeat(depth - 1)
            )
        };
        assert!(parse_json_events(nested(msgpack::MAX_NESTING).as_bytes()).is_ok());
        assert!(matches!(
            parse_json_events(nested(msgpack::MAX_NESTING + 1).as_bytes()),
            Err(IngestError::BadRequest(_))
        ));
    }

    #[test]
    fn gzip_is_capped_after_decompression() {
        assert_eq!(gunzip(&gzip(b"hello")).unwrap(), b"hello");
        // 51 gzip members of 1 MiB of zeros: about 50 KiB that inflate to 51 MiB.
        let bomb = gzip(&vec![0u8; 1024 * 1024]).repeat(51);
        assert!(bomb.len() < 100 * 1024, "{}", bomb.len());
        assert!(matches!(
            gunzip(&bomb),
            Err(IngestError::PayloadTooLarge(_))
        ));
        assert!(matches!(
            gunzip(b"not gzip"),
            Err(IngestError::BadRequest(_))
        ));
        let mut multi = gzip(b"{\"a\":1}\n");
        multi.extend(gzip(b"{\"b\":2}\n"));
        assert_eq!(
            gunzip(&multi).unwrap(),
            b"{\"a\":1}\n{\"b\":2}\n",
            "multi-member gzip"
        );
    }

    #[test]
    fn empty_bodies_are_rejected_for_every_format() {
        for format in [BodyFormat::Json, BodyFormat::MessagePack] {
            let encoding = BodyEncoding {
                format,
                gzip: false,
            };
            assert!(matches!(
                decode_events(Bytes::from_static(b" \n"), encoding),
                Err(IngestError::BadRequest(_))
            ));
        }
    }

    #[test]
    fn retry_after_counts_down_to_utc_midnight() {
        let at = |s: &str| s.parse::<DateTime<Utc>>().unwrap();
        assert_eq!(seconds_until_utc_midnight(at("2026-10-13T23:00:00Z")), 3600);
        assert_eq!(
            seconds_until_utc_midnight(at("2026-10-13T23:59:59.900Z")),
            1
        );
        assert_eq!(
            seconds_until_utc_midnight(at("2026-10-13T00:00:00Z")),
            86_400
        );
    }

    #[tokio::test]
    async fn every_rejection_has_its_status_and_headers() {
        let cases = [
            (IngestError::Unauthorized, 401, None, Some("Bearer")),
            (IngestError::Busy, 503, Some("1"), None),
            (IngestError::RequestTimeout, 408, None, None),
            (IngestError::RateLimited, 429, Some("1"), None),
            (
                IngestError::QuotaExceeded {
                    retry_after_seconds: 777,
                },
                429,
                Some("777"),
                None,
            ),
            (IngestError::PayloadTooLarge(String::new()), 413, None, None),
            (
                IngestError::UnsupportedMediaType(String::new()),
                415,
                None,
                None,
            ),
            (IngestError::BadRequest(String::new()), 400, None, None),
            (IngestError::Internal, 500, None, None),
        ];
        for (error, status, retry_after, authenticate) in cases {
            let label = format!("{error:?}");
            let response = error.into_response();
            assert_eq!(response.status().as_u16(), status, "{label}");
            let header = |name| {
                response
                    .headers()
                    .get(name)
                    .map(|v| v.to_str().unwrap().to_string())
            };
            assert_eq!(header(RETRY_AFTER).as_deref(), retry_after, "{label}");
            assert_eq!(header(WWW_AUTHENTICATE).as_deref(), authenticate, "{label}");
        }
    }

    /// Normalization is CPU-bound; on an async worker it would stall every
    /// other request the worker serves. With a ticker task sharing the
    /// (single-threaded) runtime, the longest wait between two ticks must be
    /// well below what the same work costs when run inline.
    #[tokio::test]
    async fn rows_are_built_on_a_blocking_thread_not_on_the_runtime() {
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, Ordering},
            },
            time::{Duration, Instant},
        };

        let source = IngestSource {
            id: uuid::Uuid::new_v4(),
            project_id: uuid::Uuid::new_v4(),
            retention_days: 30,
            daily_byte_quota: None,
        };
        let events: Vec<Value> = (0..5_000)
            .map(|n| {
                json!({
                    "message": format!("request {n} from 10.0.{}.{} took {n}.5 ms {}", n % 200, n % 250, "x".repeat(1000)),
                    "level": "warn",
                    "user": { "id": n, "tags": ["a", "b"] }
                })
            })
            .collect();
        let body = Bytes::from(serde_json::to_vec(&events).unwrap());
        let encoding = BodyEncoding {
            format: BodyFormat::Json,
            gzip: false,
        };

        let started = Instant::now();
        let inline = build_rows(body.clone(), encoding, &source, Utc::now()).unwrap();
        let inline_time = started.elapsed();
        assert_eq!(inline.len(), events.len());
        assert!(
            inline_time > Duration::from_millis(100),
            "the batch is too small to tell anything: {inline_time:?}"
        );

        let done = Arc::new(AtomicBool::new(false));
        let ticker = tokio::spawn({
            let done = done.clone();
            async move {
                let mut longest = Duration::ZERO;
                let mut last = Instant::now();
                while !done.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    longest = longest.max(last.elapsed());
                    last = Instant::now();
                }
                longest
            }
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        let rows = rows_from_body(body, encoding, &source, Utc::now())
            .await
            .unwrap();
        done.store(true, Ordering::Relaxed);
        let longest_wait = ticker.await.unwrap();

        assert_eq!(rows.len(), events.len());
        assert!(
            longest_wait < inline_time / 2,
            "the runtime stood still for {longest_wait:?}; the work takes {inline_time:?}"
        );
    }
}
