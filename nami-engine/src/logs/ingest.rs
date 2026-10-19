//! HTTP ingest bodies: authentication header, content negotiation, size
//! limits, gzip, and parsing into events. The handler is
//! `routes::logs_ingest`; everything here except [`read_body`] is
//! synchronous, and [`prepare`] runs it on a blocking thread.
//!
//! Formats: JSON — one object, an array of objects, or several of those
//! separated by whitespace, which covers NDJSON (`application/x-ndjson`)
//! and shippers that send NDJSON as `application/json` — and MessagePack
//! (`application/msgpack`, `application/x-msgpack`): one map or an array of
//! maps. Either may be gzip-compressed (`Content-Encoding: gzip`).
//!
//! What a body may cost is limited at every step, on the wire and in memory:
//! 10 MiB as sent ([`read_body`], which also expects it to arrive at a decent
//! [`BodyPace`]), 50 MiB after gzip ([`gunzip`], which stops inflating at the
//! limit), 10,000 events, [`budget::MAX_DECODED_BYTES`] of what the decoders
//! build from it ([`budget`]), and [`MAX_ENCODED_BYTES`] of what is stored and
//! sent to ClickHouse (a body can grow again after it is decoded: JSON
//! escaping turns one control character into six bytes).

use std::{fmt, io::Read, sync::Arc, time::Duration};

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
use http_body_util::BodyExt;
use serde::de::{self, DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value, json};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::Instant,
};

use super::{
    budget::{self, Budget, TooLarge, VALUE_BYTES},
    msgpack::{self, MsgpackError},
    normalize,
    rate_limit::{DecodeShare, DecodeShares, REQUESTS_PER_SECOND},
    store::{self, LogRow},
};
use crate::{clickhouse::ClickHouseError, error::AppError, models::source::IngestSource};

/// Largest accepted request body, as sent (compressed or not).
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
/// Largest accepted body after gzip decompression (gzip-bomb guard).
pub const MAX_DECOMPRESSED_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_EVENTS: usize = 10_000;
/// The most a request may store, and the most its insert body (the rows as
/// JSON, [`store::encode_logs`]) may weigh: twice the inflated body. A body
/// can grow after it is decoded (a control character is one byte in a message
/// and six in the JSON body, and `fields` is escaped once as JSON and again as
/// the string it is in its row), so what a request stores is checked while the
/// rows are built and again when they are encoded, and a request over it is a
/// 413. Together with [`LARGE_BODY_BYTES`] this bounds what the bodies waiting
/// for ClickHouse take, at the same time.
pub const MAX_ENCODED_BYTES: usize = 2 * MAX_DECOMPRESSED_BYTES;
/// An insert body above this keeps its place for decoding until ClickHouse has
/// taken it ([`Prepared::place`]), so only as many of them exist at once as
/// there are such places; smaller ones give the place back as soon as they
/// are built (their number is bounded by the places of the endpoint).
pub const LARGE_BODY_BYTES: usize = 8 * 1024 * 1024;
/// A parse error that quotes the body (serde_json does for a value of the
/// wrong type) is cut to this many bytes: a body that is one huge string
/// must not come back as one huge error message.
const MAX_ERROR_DETAIL_BYTES: usize = 200;

/// How fast a request body has to arrive. A request holds one of the few
/// ingest places while its body is on the way, so a client that stalls (a dead
/// connection that never closes) or sends a byte now and then must not keep it
/// for long: the body gets `grace` to get going, then has to keep up
/// `min_bytes_per_second` (the time it may take grows with what it has
/// delivered), and all of it has to be in within `cap`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyPace {
    pub grace: Duration,
    pub min_bytes_per_second: u64,
    pub cap: Duration,
}

impl BodyPace {
    /// 5 s to start, then 64 KiB/s, at most 60 s in all: a small body that
    /// stalls is cut off after about 5 s, and the full 10 MiB still fits at
    /// about 1.4 Mbit/s.
    pub const DEFAULT: BodyPace = BodyPace {
        grace: Duration::from_secs(5),
        min_bytes_per_second: 64 * 1024,
        cap: Duration::from_secs(60),
    };

    /// How long after the start a body that has delivered `received` bytes may
    /// take to deliver its next piece.
    pub fn allowed(&self, received: usize) -> Duration {
        let earned = received as f64 / self.min_bytes_per_second.max(1) as f64;
        (self.grace + Duration::from_secs_f64(earned)).min(self.cap)
    }
}

impl Default for BodyPace {
    fn default() -> Self {
        BodyPace::DEFAULT
    }
}

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
    /// The source already has `limit` requests in flight (see
    /// `rate_limit::MAX_IN_FLIGHT_PER_SOURCE`).
    #[error("too many requests in flight for this source")]
    TooManyInFlight { limit: usize },
    /// The source's project already has its share of requests in flight
    /// (`rate_limit::MAX_IN_FLIGHT_PER_PROJECT`).
    #[error("too many requests in flight for this project")]
    ProjectTooManyInFlight { limit: usize },
    /// The body did not arrive at the pace of [`BodyPace`].
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
            IngestError::TooManyInFlight { limit } => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "error": format!(
                        "too many requests in flight for this source: at most {limit} at once"
                    ),
                    "code": "too_many_in_flight"
                }),
                Some(1),
            ),
            IngestError::ProjectTooManyInFlight { limit } => (
                StatusCode::TOO_MANY_REQUESTS,
                json!({
                    "error": format!(
                        "too many requests in flight for this project: at most {limit} at once"
                    ),
                    "code": "too_many_in_flight"
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

fn unsupported_content_encoding() -> IngestError {
    IngestError::UnsupportedMediaType(
        "unsupported Content-Encoding: send one gzip coding, or an uncompressed body".to_string(),
    )
}

pub fn body_encoding(headers: &HeaderMap) -> Result<BodyEncoding, IngestError> {
    // One coding in one header. A second `Content-Encoding` line (or a list,
    // `gzip, br`) is a stack of codings, or a line a proxy added: reading only
    // the first would decode the body as something it is not.
    let mut codings = headers.get_all(CONTENT_ENCODING).iter();
    let coding = match (codings.next(), codings.next()) {
        (_, Some(_)) => return Err(unsupported_content_encoding()),
        (None, None) => None,
        (Some(value), None) => Some(
            value
                .to_str()
                .map_err(|_| unsupported_content_encoding())?
                .trim()
                .to_ascii_lowercase(),
        ),
    };
    let gzip = match coding.as_deref() {
        None | Some("") | Some("identity") => false,
        Some("gzip") | Some("x-gzip") => true,
        Some(_) => return Err(unsupported_content_encoding()),
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

/// Reads at most [`MAX_BODY_BYTES`], at the pace of `pace` (408 when the
/// body stalls or crawls); a larger declared `Content-Length` is refused
/// before reading anything.
pub async fn read_body(
    mut body: Body,
    headers: &HeaderMap,
    pace: &BodyPace,
) -> Result<Bytes, IngestError> {
    let declared = headers
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|len| len > MAX_BODY_BYTES as u64) {
        return Err(body_too_large());
    }
    let started = Instant::now();
    let mut pieces: Vec<Bytes> = Vec::new();
    let mut received = 0usize;
    loop {
        let wait = pace.allowed(received).saturating_sub(started.elapsed());
        let frame = match tokio::time::timeout(wait, body.frame()).await {
            Err(_elapsed) => return Err(IngestError::RequestTimeout),
            Ok(None) => break,
            Ok(Some(Err(_))) => {
                return Err(IngestError::BadRequest(
                    "could not read the request body".to_string(),
                ));
            }
            Ok(Some(Ok(frame))) => frame,
        };
        // Trailers, the other kind of frame, say nothing we need.
        if let Ok(data) = frame.into_data() {
            received += data.len();
            if received > MAX_BODY_BYTES {
                return Err(body_too_large());
            }
            pieces.push(data);
        }
    }
    if pieces.len() <= 1 {
        return Ok(pieces.pop().unwrap_or_default());
    }
    let mut whole = Vec::with_capacity(received);
    for piece in pieces {
        whole.extend_from_slice(&piece);
    }
    Ok(Bytes::from(whole))
}

/// A place for decoding a body: one of the engine's few
/// (`routes::logs_ingest::IngestLimits::decodes`), one of its source's and one
/// of its project's ([`DecodeShares`]). Given back when dropped.
#[derive(Debug)]
pub struct DecodePlace {
    _engine: OwnedSemaphorePermit,
    _source: DecodeShare,
    _project: DecodeShare,
}

/// A batch of events as the ingest path sends it on: the rows as the body of
/// the ClickHouse insert, and what they count for.
#[derive(Debug)]
pub struct Prepared {
    /// The `JSONEachRow` body of the rows ([`store::encode_logs`]).
    pub body: String,
    pub events: usize,
    /// What the rows count against usage and quotas ([`LogRow::stored_bytes`]).
    pub bytes: u64,
    /// The place the body was made in, still held when the body is larger than
    /// [`LARGE_BODY_BYTES`]: keep it until the body has been sent, so that no
    /// more than a few large bodies exist at the same time. `None` for a small
    /// body, whose place went back when it was made.
    pub place: Option<DecodePlace>,
}

/// Turns a body into what to send to ClickHouse for `source`: decompress,
/// parse, normalize ([`build_rows`]) and encode the insert body
/// ([`store::encode_logs`]), on a blocking thread, with a place of `decodes`
/// (and one of the source's `shares` and of its project's `project_shares`)
/// held for as long as that runs. The work
/// is CPU-bound (about 50 ms for a typical batch of 10,000 events, up to a
/// second for one of large messages) and takes memory in proportion (see
/// [`budget`]): on an async worker it would stall every other request that
/// worker serves, and left to the places of the endpoint alone, 32 hostile
/// bodies could be decoded at once.
///
/// The place is moved into the blocking task: a blocking task cannot be
/// cancelled, so it must keep the place even if whoever awaits it is gone. The
/// rows are dropped there too, so only the encoded body stays in memory while
/// ClickHouse is waited for, and a body that is large ([`LARGE_BODY_BYTES`])
/// takes the place with it ([`Prepared::place`]).
pub async fn prepare(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
    decodes: &Arc<Semaphore>,
    shares: &DecodeShares,
    project_shares: &DecodeShares,
) -> Result<Prepared, IngestError> {
    // The project's and the source's own gates first, so that what they have
    // beyond their shares waits there and not in the queue for the engine's
    // places.
    let project_share = project_shares.acquire(source.project_id).await;
    let share = shares.acquire(source.id).await;
    let engine = decodes
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| IngestError::Internal)?;
    let place = DecodePlace {
        _engine: engine,
        _source: share,
        _project: project_share,
    };
    let source = source.clone();
    tokio::task::spawn_blocking(move || {
        let batch = build_batch(body, encoding, &source, received_at)?;
        // Only the body is kept (the rows are gone); a large one keeps the
        // place too.
        let place = (batch.body.len() > LARGE_BODY_BYTES).then_some(place);
        Ok(Prepared {
            body: batch.body,
            events: batch.events,
            bytes: batch.bytes,
            place,
        })
    })
    .await
    .map_err(|error| {
        tracing::error!(%error, "log ingest worker failed");
        IngestError::Internal
    })?
}

/// What [`prepare`] makes of a body, before any place is attached.
#[derive(Debug)]
pub struct Batch {
    /// The `JSONEachRow` body of the rows ([`store::encode_logs`]), in a
    /// buffer of exactly its size.
    pub body: String,
    pub events: usize,
    /// What the rows count against usage and quotas ([`LogRow::stored_bytes`]).
    pub bytes: u64,
}

/// The work of [`prepare`] without its thread and its places: decompress,
/// parse, build the rows ([`build_rows`], refused over [`MAX_ENCODED_BYTES`]
/// stored) and encode the insert body (refused over [`MAX_ENCODED_BYTES`]
/// encoded, before anything is built). The rows are gone when it returns.
pub fn build_batch(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
) -> Result<Batch, IngestError> {
    let rows = build_rows(body, encoding, source, received_at)?;
    let bytes = rows.iter().map(LogRow::stored_bytes).sum();
    let encoded = match store::encode_logs(&rows, MAX_ENCODED_BYTES) {
        Ok(encoded) => encoded,
        Err(ClickHouseError::TooLarge { .. }) => return Err(stored_too_large()),
        Err(error) => {
            tracing::error!(%error, "could not encode a log batch");
            return Err(IngestError::Internal);
        }
    };
    Ok(Batch {
        body: encoded,
        events: rows.len(),
        bytes,
    })
}

/// The synchronous part of [`prepare`]: the rows of a body. Gives up with a
/// 413 as soon as what they store passes [`MAX_ENCODED_BYTES`] (the insert body
/// cannot be smaller than what it stores), rather than building the rest.
pub fn build_rows(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
) -> Result<Vec<LogRow>, IngestError> {
    build_rows_within(body, encoding, source, received_at, MAX_ENCODED_BYTES)
}

/// [`build_rows`] with the cap of the rows' stored bytes as a parameter.
pub fn build_rows_within(
    body: Bytes,
    encoding: BodyEncoding,
    source: &IngestSource,
    received_at: DateTime<Utc>,
    max_stored_bytes: usize,
) -> Result<Vec<LogRow>, IngestError> {
    let events = decode_events(body, encoding)?;
    let mut rows = Vec::with_capacity(events.len());
    let mut stored = 0u64;
    // Each event is dropped as its row is made: the values it held go as the
    // rows grow.
    for object in events {
        let row = store::build_row(normalize::from_json(object, received_at), source);
        stored += row.stored_bytes();
        if stored > max_stored_bytes as u64 {
            return Err(stored_too_large());
        }
        rows.push(row);
    }
    Ok(rows)
}

/// Decompresses (when gzip) and parses a body into event objects.
pub fn decode_events(
    body: Bytes,
    encoding: BodyEncoding,
) -> Result<Vec<Map<String, Value>>, IngestError> {
    let raw = if encoding.gzip {
        let inflated = gunzip(&body)?;
        // The compressed body is not needed any more.
        drop(body);
        Bytes::from(inflated)
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
                MsgpackError::TooLarge => decoded_too_large(),
                other => IngestError::BadRequest(format!("invalid MessagePack body: {other}")),
            })
        }
    }
}

fn too_many_events() -> IngestError {
    IngestError::PayloadTooLarge(format!("too many events: at most {MAX_EVENTS} per request"))
}

fn stored_too_large() -> IngestError {
    IngestError::PayloadTooLarge(format!(
        "request body takes more than {} MiB once stored: send fewer or smaller events",
        MAX_ENCODED_BYTES / (1024 * 1024)
    ))
}

fn decoded_too_large() -> IngestError {
    IngestError::PayloadTooLarge(format!(
        "request body expands to more than {} MiB once decoded: send fewer or smaller events",
        budget::MAX_DECODED_BYTES / (1024 * 1024)
    ))
}

/// gzip (multi-member) with the output capped at [`MAX_DECOMPRESSED_BYTES`]:
/// inflating stops at the cap, so a few MiB that would inflate to gigabytes
/// cost about 50 MiB of work and memory, no more.
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
/// failing fast on the event after the [`MAX_EVENTS`]th and on a body that
/// decodes to more than the [`Budget::for_request`] budget.
pub fn parse_json_events(raw: &[u8]) -> Result<Vec<Map<String, Value>>, IngestError> {
    parse_json_events_within(raw, &mut Budget::for_request())
}

/// [`parse_json_events`] charging `budget`, which the caller keeps: it shows
/// what the parsing cost, also when it stopped.
pub fn parse_json_events_within(
    raw: &[u8],
    budget: &mut Budget,
) -> Result<Vec<Map<String, Value>>, IngestError> {
    let mut events = Vec::new();
    let mut refusal = None;
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    // `end` succeeds once only whitespace is left; otherwise it has skipped
    // the whitespace before the next value.
    while deserializer.end().is_err() {
        let seed = TopLevel {
            events: &mut events,
            spend: Spend {
                budget: &mut *budget,
                refusal: &mut refusal,
            },
        };
        if let Err(error) = seed.deserialize(&mut deserializer) {
            return Err(match refusal {
                Some(Refusal::TooManyEvents) => too_many_events(),
                Some(Refusal::TooLarge) => decoded_too_large(),
                None => invalid_json(&error),
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

/// Why a parse stopped although the body is well-formed JSON. serde errors
/// carry a message only, and a 413 must not be taken for a 400.
#[derive(Debug, Clone, Copy)]
enum Refusal {
    TooManyEvents,
    TooLarge,
}

/// The budget a parse charges, and where it notes that it refused to go on.
struct Spend<'a> {
    budget: &'a mut Budget,
    refusal: &'a mut Option<Refusal>,
}

impl Spend<'_> {
    fn reborrow(&mut self) -> Spend<'_> {
        Spend {
            budget: &mut *self.budget,
            refusal: &mut *self.refusal,
        }
    }

    fn charge<E: de::Error>(
        &mut self,
        cost: impl FnOnce(&mut Budget) -> Result<(), TooLarge>,
    ) -> Result<(), E> {
        cost(&mut *self.budget).map_err(|TooLarge| {
            *self.refusal = Some(Refusal::TooLarge);
            E::custom("decoded size over the limit")
        })
    }

    fn value<E: de::Error>(&mut self) -> Result<(), E> {
        self.charge(|budget| budget.charge(VALUE_BYTES))
    }
}

/// Builds a `serde_json::Value` as `Value::deserialize` does, charging the
/// budget for each piece before it allocates it (what `serde_json` itself
/// buffers to unescape a string aside, see [`budget`]): a body of 50 MiB of
/// `0,` stops after a few million values instead of building them all.
struct Counted<'a>(Spend<'a>);

impl<'de> DeserializeSeed<'de> for Counted<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Counted<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any valid JSON value")
    }

    fn visit_bool<E: de::Error>(mut self, value: bool) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(mut self, value: i64) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Value::from(value))
    }

    fn visit_u64<E: de::Error>(mut self, value: u64) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Value::from(value))
    }

    fn visit_f64<E: de::Error>(mut self, value: f64) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Number::from_f64(value).map_or(Value::Null, Value::Number))
    }

    fn visit_str<E: de::Error>(mut self, value: &str) -> Result<Value, E> {
        self.0.charge::<E>(|budget| budget.string(value.len()))?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: de::Error>(mut self, value: String) -> Result<Value, E> {
        self.0.charge::<E>(|budget| budget.string(value.len()))?;
        Ok(Value::String(value))
    }

    fn visit_none<E: de::Error>(mut self) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Value::Null)
    }

    fn visit_unit<E: de::Error>(mut self) -> Result<Value, E> {
        self.0.value::<E>()?;
        Ok(Value::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Value, A::Error> {
        self.0.charge::<A::Error>(|budget| budget.array())?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(Counted(self.0.reborrow()))? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(mut self, map: A) -> Result<Value, A::Error> {
        read_object(map, &mut self.0).map(Value::Object)
    }
}

/// The entries of an object, each key and value charged before it is built.
fn read_object<'de, A: MapAccess<'de>>(
    mut map: A,
    spend: &mut Spend<'_>,
) -> Result<Map<String, Value>, A::Error> {
    spend.charge::<A::Error>(|budget| budget.object())?;
    let mut object = Map::new();
    while let Some(key) = map.next_key_seed(Key(&mut *spend))? {
        let value = map.next_value_seed(Counted(spend.reborrow()))?;
        object.insert(key, value);
    }
    Ok(object)
}

/// An object's key, charged before it is copied out of the input (a key with
/// escapes has already been unescaped into `serde_json`'s buffer, which is
/// bounded by the body).
struct Key<'a, 'b>(&'a mut Spend<'b>);

impl<'de> DeserializeSeed<'de> for Key<'_, '_> {
    type Value = String;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<String, D::Error> {
        deserializer.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Key<'_, '_> {
    type Value = String;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, key: &str) -> Result<String, E> {
        self.0.charge::<E>(|budget| budget.key(key.len()))?;
        Ok(key.to_owned())
    }
}

/// One event, an object, added to `events`; refused when there are
/// [`MAX_EVENTS`] already, before any of it is read.
fn read_event<'de, A: MapAccess<'de>>(
    map: A,
    events: &mut Vec<Map<String, Value>>,
    spend: &mut Spend<'_>,
) -> Result<(), A::Error> {
    if events.len() >= MAX_EVENTS {
        *spend.refusal = Some(Refusal::TooManyEvents);
        return Err(<A::Error as de::Error>::custom("too many events"));
    }
    events.push(read_object(map, spend)?);
    Ok(())
}

/// One top-level JSON value: an object (one event) or an array of objects.
struct TopLevel<'a> {
    events: &'a mut Vec<Map<String, Value>>,
    spend: Spend<'a>,
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
        read_event(map, self.events, &mut self.spend)
    }

    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<(), A::Error> {
        while seq
            .next_element_seed(Event {
                events: &mut *self.events,
                spend: self.spend.reborrow(),
            })?
            .is_some()
        {}
        Ok(())
    }
}

/// An element of a top-level array: it must be an object.
struct Event<'a> {
    events: &'a mut Vec<Map<String, Value>>,
    spend: Spend<'a>,
}

impl<'de> DeserializeSeed<'de> for Event<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        deserializer.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Event<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(mut self, map: A) -> Result<(), A::Error> {
        read_event(map, self.events, &mut self.spend)
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
    use crate::logs::budget::{ARRAY_BYTES, KEY_BYTES, MAP_BYTES, MAX_DECODED_BYTES, STRING_BYTES};
    use crate::logs::rate_limit::ProjectLimits;
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

    fn source() -> IngestSource {
        IngestSource {
            id: uuid::Uuid::new_v4(),
            project_id: uuid::Uuid::new_v4(),
            retention_days: 30,
            daily_byte_quota: None,
        }
    }

    const JSON: BodyEncoding = BodyEncoding {
        format: BodyFormat::Json,
        gzip: false,
    };

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
        for content_type in [
            None,
            Some("application/json; charset=utf-8"),
            Some("application/x-ndjson"),
            Some("text/plain"),
        ] {
            let h = content_type
                .map(|ct| headers(&[("content-type", ct)]))
                .unwrap_or_default();
            assert_eq!(body_encoding(&h).unwrap(), JSON, "{content_type:?}");
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

    /// One coding, in one header line: a second line, a list, or a header that
    /// is not text is a stack of codings we do not unwrap, or something a proxy
    /// added, and is not read as its first line alone.
    #[test]
    fn only_a_single_content_encoding_is_accepted() {
        let with = |values: &[&[u8]]| {
            let mut map = HeaderMap::new();
            for value in values {
                map.append(CONTENT_ENCODING, HeaderValue::from_bytes(value).unwrap());
            }
            body_encoding(&map)
        };
        assert!(with(&[b"gzip"]).unwrap().gzip);
        assert!(with(&[b"x-gzip"]).unwrap().gzip);
        assert!(!with(&[b"identity"]).unwrap().gzip);
        assert!(!with(&[]).unwrap().gzip);
        for values in [
            &[&b"gzip"[..], b"gzip"][..],
            &[b"gzip", b"br"],
            &[b"identity", b"gzip"],
            &[b"gzip", b"identity"],
            &[b"gzip, gzip"],
            &[b"gzip, identity"],
            &[b"gzip\xff"],
        ] {
            assert!(
                matches!(with(values), Err(IngestError::UnsupportedMediaType(_))),
                "{values:?}"
            );
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

    /// What a document costs, by the model of [`budget`], worked out apart
    /// from either decoder: the object of an event, then for each entry its
    /// key and its value; an array and each of its items; a string and its
    /// characters; every other value a slot.
    fn expected_cost(event: &Value) -> usize {
        fn value(v: &Value) -> usize {
            match v {
                Value::String(s) => VALUE_BYTES + STRING_BYTES + s.len(),
                Value::Array(items) => {
                    VALUE_BYTES + ARRAY_BYTES + items.iter().map(value).sum::<usize>()
                }
                Value::Object(entries) => {
                    VALUE_BYTES
                        + MAP_BYTES
                        + entries
                            .iter()
                            .map(|(k, v)| KEY_BYTES + STRING_BYTES + k.len() + value(v))
                            .sum::<usize>()
                }
                _ => VALUE_BYTES,
            }
        }
        value(event)
    }

    /// Both decoders charge the same for the same document, and what the model
    /// says: a body costs the same budget as JSON and as MessagePack.
    #[test]
    fn both_decoders_charge_what_the_model_says() {
        for event in [
            json!({}),
            json!({ "message": "hello", "n": 1, "f": 1.5, "t": true, "z": null }),
            json!({ "a": [1, "xy", null, { "b": [[], {}] }], "": "" }),
            json!({ "long": "é".repeat(5000), "k".repeat(300): vec![0; 100] }),
        ] {
            let expected = expected_cost(&event);

            let mut budget = Budget::new(usize::MAX);
            let events = parse_json_events_within(event.to_string().as_bytes(), &mut budget);
            assert_eq!(events.unwrap().len(), 1);
            assert_eq!(budget.spent(), expected, "JSON {event}");

            let mut budget = Budget::new(usize::MAX);
            let packed = rmp_serde::to_vec(&event).unwrap();
            msgpack::decode_events_within(&packed, MAX_EVENTS, &mut budget).unwrap();
            assert_eq!(budget.spent(), expected, "MessagePack {event}");
        }
    }

    /// 50 MB of `0,` is a 25-million-element array: 1.6 GB of values without a
    /// budget. With one, the parse stops after as many values as the budget
    /// pays for, and says 413.
    #[test]
    fn a_json_body_that_decodes_to_more_than_the_budget_is_refused_where_it_runs_out() {
        let limit = 4 * 1024 * 1024;
        let body = format!("{{\"a\":[{}0]}}", "0,".repeat(3_000_000));
        let mut budget = Budget::new(limit);
        let refused = parse_json_events_within(body.as_bytes(), &mut budget);
        assert!(
            matches!(&refused, Err(IngestError::PayloadTooLarge(m)) if m.contains("expands to more than")),
            "{refused:?}"
        );
        assert!(
            budget.spent() <= limit && budget.spent() > limit - VALUE_BYTES,
            "{} of {limit}: stopped as soon as the budget ran out",
            budget.spent()
        );

        // Tiny objects are the dearest per byte: 8 bytes of JSON, a B-tree node.
        let body = format!("{{\"a\":[{}{{\"b\":1}}]}}", "{\"b\":1},".repeat(100_000));
        let mut budget = Budget::new(limit);
        assert!(matches!(
            parse_json_events_within(body.as_bytes(), &mut budget),
            Err(IngestError::PayloadTooLarge(_))
        ));
        assert!(budget.spent() <= limit);

        // Keys and strings are charged by their length too.
        let body = format!("{{\"a\":\"{}\"}}", "x".repeat(limit));
        assert!(matches!(
            parse_json_events_within(body.as_bytes(), &mut Budget::new(limit)),
            Err(IngestError::PayloadTooLarge(_))
        ));
        let body = format!("{{\"{}\":1}}", "k".repeat(limit));
        assert!(matches!(
            parse_json_events_within(body.as_bytes(), &mut Budget::new(limit)),
            Err(IngestError::PayloadTooLarge(_))
        ));
    }

    /// The same bomb in MessagePack: a one-byte value is the same 64 charged
    /// bytes, so 52 million of them stop at the budget, however the 50 MiB were
    /// delivered.
    #[test]
    fn a_messagepack_body_that_decodes_to_more_than_the_budget_is_refused_too() {
        let limit = 4 * 1024 * 1024;
        // { "a": [0 x 5,000,000] }
        let mut body = vec![0x81, 0xa1, b'a', 0xdd];
        body.extend(5_000_000u32.to_be_bytes());
        body.resize(body.len() + 5_000_000, 0);
        let mut budget = Budget::new(limit);
        assert_eq!(
            msgpack::decode_events_within(&body, MAX_EVENTS, &mut budget),
            Err(MsgpackError::TooLarge)
        );
        assert!(budget.spent() <= limit && budget.spent() > limit - VALUE_BYTES);
        // And through the body decoder: a 413, not a 400.
        let refused = decode_events(
            Bytes::from(body),
            BodyEncoding {
                format: BodyFormat::MessagePack,
                gzip: false,
            },
        );
        assert!(
            matches!(refused, Err(IngestError::PayloadTooLarge(_))),
            "{refused:?}"
        );
    }

    /// The budget is one for the whole body, not one per event: events that are
    /// each far under it add up to a refusal.
    #[test]
    fn the_budget_is_shared_by_all_the_events_of_a_body() {
        let limit = 2 * 1024 * 1024;
        let event = format!("{{\"message\":\"{}\"}}", "x".repeat(100_000));
        let mut budget = Budget::new(limit);
        let one = parse_json_events_within(event.as_bytes(), &mut budget);
        assert_eq!(one.unwrap().len(), 1);
        assert!(
            budget.spent() < limit / 10,
            "one event is a small part of it"
        );

        // NDJSON, an array and MessagePack alike.
        let ndjson = vec![event.as_str(); 30].join("\n");
        assert!(matches!(
            parse_json_events_within(ndjson.as_bytes(), &mut Budget::new(limit)),
            Err(IngestError::PayloadTooLarge(_))
        ));
        let array = format!("[{}]", vec![event.as_str(); 30].join(","));
        assert!(matches!(
            parse_json_events_within(array.as_bytes(), &mut Budget::new(limit)),
            Err(IngestError::PayloadTooLarge(_))
        ));
        let one: Value = serde_json::from_str(&event).unwrap();
        let packed = rmp_serde::to_vec(&vec![one; 30]).unwrap();
        assert_eq!(
            msgpack::decode_events_within(&packed, MAX_EVENTS, &mut Budget::new(limit)),
            Err(MsgpackError::TooLarge)
        );
    }

    /// A batch that is big but sensible fits the budget with room to spare:
    /// 10,000 events of 40 fields with strings of 40 characters.
    #[test]
    fn a_large_legitimate_batch_fits_the_budget() {
        let event = |n: usize| {
            let mut fields = Map::new();
            for i in 0..40 {
                fields.insert(
                    format!("field_{i:02}"),
                    json!(format!("value {n} {i} {}", "x".repeat(24))),
                );
            }
            Value::Object(fields)
        };
        let events: Vec<Value> = (0..MAX_EVENTS).map(event).collect();
        let body = serde_json::to_vec(&events).unwrap();
        let mut budget = Budget::for_request();
        let parsed = parse_json_events_within(&body, &mut budget).unwrap();
        assert_eq!(parsed.len(), MAX_EVENTS);
        assert!(
            budget.spent() < MAX_DECODED_BYTES * 3 / 4,
            "{} MiB of {} MiB for a {} MiB body",
            budget.spent() >> 20,
            MAX_DECODED_BYTES >> 20,
            body.len() >> 20
        );
    }

    /// Inflating stops at the cap: a few MiB on the wire that would inflate to
    /// gigabytes are refused after about 50 MiB of work. A check made after
    /// inflating everything would need that many gigabytes of memory (and
    /// minutes) here, which the time limit would show.
    #[test]
    fn a_gzip_body_that_inflates_to_gigabytes_is_stopped_at_the_cap() {
        // 3,000 members of 1 MiB of zeros: ~3 MiB on the wire, ~3 GiB inflated.
        let bomb = gzip(&vec![0u8; 1024 * 1024]).repeat(3_000);
        assert!(
            bomb.len() < 4 * 1024 * 1024,
            "{} bytes on the wire",
            bomb.len()
        );
        let started = std::time::Instant::now();
        assert!(matches!(
            gunzip(&bomb),
            Err(IngestError::PayloadTooLarge(_))
        ));
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "{:?}: it was not stopped at the cap",
            started.elapsed()
        );
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

    #[test]
    fn a_body_may_take_longer_the_more_it_has_delivered_up_to_the_cap() {
        let pace = BodyPace::DEFAULT;
        assert_eq!(pace.allowed(0), Duration::from_secs(5));
        // 64 KiB/s: another second for every 64 KiB.
        assert_eq!(pace.allowed(64 * 1024), Duration::from_secs(6));
        assert_eq!(pace.allowed(640 * 1024), Duration::from_secs(15));
        // The cap holds however much was delivered, and a rate of zero
        // cannot divide by zero.
        assert_eq!(pace.allowed(MAX_BODY_BYTES), Duration::from_secs(60));
        assert_eq!(pace.allowed(usize::MAX), Duration::from_secs(60));
        let stalled = BodyPace {
            min_bytes_per_second: 0,
            ..pace
        };
        assert_eq!(
            stalled.allowed(1),
            Duration::from_secs(6),
            "a rate of 0 counts as 1"
        );
        assert_eq!(stalled.allowed(1000), Duration::from_secs(60));
    }

    #[tokio::test]
    async fn every_rejection_has_its_status_and_headers() {
        let cases = [
            (IngestError::Unauthorized, 401, None, Some("Bearer")),
            (IngestError::Busy, 503, Some("1"), None),
            (IngestError::RequestTimeout, 408, None, None),
            (IngestError::RateLimited, 429, Some("1"), None),
            (
                IngestError::TooManyInFlight { limit: 4 },
                429,
                Some("1"),
                None,
            ),
            (
                IngestError::ProjectTooManyInFlight { limit: 8 },
                429,
                Some("1"),
                None,
            ),
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

    /// Events dense in what the pattern step replaces (numbers, addresses,
    /// UUIDs), which costs the most CPU per byte.
    fn workload(events: usize) -> Bytes {
        let events: Vec<Value> = (0..events)
            .map(|n| {
                json!({
                    "message": format!(
                        "request {n} from 10.0.{}.{} for 550e8400-e29b-41d4-a716-{n:012} took {n}.5 ms {}",
                        n % 200,
                        n % 250,
                        "retry 12 of 3 at 192.168.1.1; ".repeat(16)
                    ),
                    "level": "warn",
                    "user": { "id": n, "tags": ["a", "b"] }
                })
            })
            .collect();
        Bytes::from(serde_json::to_vec(&events).unwrap())
    }

    /// Normalization is CPU-bound; on an async worker it would stall every
    /// other request the worker serves. With a ticker task sharing the
    /// (single-threaded) runtime, the longest wait between two ticks must be
    /// well below what the same work costs when run inline. The batch grows
    /// until the work takes long enough to tell (a debug build needs a few
    /// thousand events, an optimized one more).
    #[tokio::test]
    async fn rows_are_built_on_a_blocking_thread_not_on_the_runtime() {
        use std::{
            sync::atomic::{AtomicBool, Ordering},
            time::Instant,
        };

        let source = source();
        let mut events = 250;
        let (body, inline_time) = loop {
            let body = workload(events);
            let started = Instant::now();
            let rows = build_rows(body.clone(), JSON, &source, Utc::now()).unwrap();
            assert_eq!(rows.len(), events);
            let took = started.elapsed();
            if took >= Duration::from_millis(150) || events >= MAX_EVENTS {
                break (body, took);
            }
            events = (events * 2).min(MAX_EVENTS);
        };
        if inline_time < Duration::from_millis(50) {
            eprintln!(
                "skipped: even {events} events take only {inline_time:?} here, too little to tell a blocked runtime"
            );
            return;
        }

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
        let prepared = prepare(
            body,
            JSON,
            &source,
            Utc::now(),
            &Arc::new(Semaphore::new(1)),
            &DecodeShares::default(),
            &DecodeShares::default(),
        )
        .await
        .unwrap();
        done.store(true, Ordering::Relaxed);
        let longest_wait = ticker.await.unwrap();

        assert_eq!(prepared.events, events);
        assert!(
            longest_wait < inline_time / 2,
            "the runtime stood still for {longest_wait:?}; the work takes {inline_time:?}"
        );
    }

    /// The batch that goes on to ClickHouse: one line per event, the same
    /// rows `store::insert_logs` would send, and what they count for.
    #[tokio::test]
    async fn a_prepared_batch_is_the_insert_body_and_what_it_counts_for() {
        let source = source();
        let decodes = Arc::new(Semaphore::new(1));
        let shares = DecodeShares::default();
        let projects = ProjectLimits::default().decode_shares;
        let body = Bytes::from_static(br#"[{"message":"one","a":1},{"message":"two"}]"#);
        let prepared = prepare(
            body.clone(),
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert_eq!(prepared.events, 2);
        assert_eq!(prepared.body.lines().count(), 2);
        assert!(prepared.body.ends_with('\n'));
        assert!(
            prepared.place.is_none(),
            "a small body gives its place back"
        );
        let rows = build_rows(body, JSON, &source, Utc::now()).unwrap();
        assert_eq!(
            prepared.bytes,
            rows.iter().map(LogRow::stored_bytes).sum::<u64>()
        );
        let first: Value = serde_json::from_str(prepared.body.lines().next().unwrap()).unwrap();
        assert_eq!(first["message"], "one");
        assert_eq!(first["source_id"], source.id.to_string());

        // No events, nothing to send.
        let empty = prepare(
            Bytes::from_static(b"[]"),
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert_eq!((empty.events, empty.bytes, empty.body.as_str()), (0, 0, ""));
    }

    /// A decode takes a place of `decodes` (and one of its source's) and gives
    /// it back when it ends, whether it worked or not; with none free it waits.
    #[tokio::test]
    async fn decoding_waits_for_a_place_and_gives_it_back() {
        let source = source();
        let decodes = Arc::new(Semaphore::new(1));
        let shares = DecodeShares::default();
        let projects = ProjectLimits::default().decode_shares;
        let good = Bytes::from_static(br#"{"message":"m"}"#);

        let held = decodes.clone().acquire_owned().await.unwrap();
        let waiting = tokio::time::timeout(
            Duration::from_millis(150),
            prepare(
                good.clone(),
                JSON,
                &source,
                Utc::now(),
                &decodes,
                &shares,
                &projects,
            ),
        )
        .await;
        assert!(waiting.is_err(), "no place is free: it waits");
        assert_eq!(shares.count(source.id), 0, "and its share goes with it");
        drop(held);

        prepare(
            good,
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert_eq!(decodes.available_permits(), 1, "given back after a success");
        assert_eq!(shares.count(source.id), 0);
        let bad = prepare(
            Bytes::from_static(b"{"),
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await;
        assert!(matches!(bad, Err(IngestError::BadRequest(_))));
        assert_eq!(decodes.available_permits(), 1, "and after a failure");
        assert_eq!(shares.count(source.id), 0);
    }

    /// The source's own gate: a source that already has its places decoding
    /// waits, however many places the engine has free, and another source does
    /// not.
    #[tokio::test]
    async fn a_source_cannot_decode_more_bodies_at_once_than_its_share() {
        let (busy, other) = (source(), source());
        let decodes = Arc::new(Semaphore::new(4));
        let shares = DecodeShares::new(1);
        let projects = ProjectLimits::default().decode_shares;
        let good = Bytes::from_static(br#"{"message":"m"}"#);

        let share = shares.acquire(busy.id).await;
        let waiting = tokio::time::timeout(
            Duration::from_millis(150),
            prepare(
                good.clone(),
                JSON,
                &busy,
                Utc::now(),
                &decodes,
                &shares,
                &projects,
            ),
        )
        .await;
        assert!(waiting.is_err(), "the source's only place is taken");
        assert_eq!(
            decodes.available_permits(),
            4,
            "no engine place was held meanwhile"
        );

        prepare(
            good.clone(),
            JSON,
            &other,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        drop(share);
        prepare(good, JSON, &busy, Utc::now(), &decodes, &shares, &projects)
            .await
            .unwrap();
    }

    /// The project's gate: two sources of one project that already has its
    /// share decoding wait, while a source of another project does not.
    #[tokio::test]
    async fn a_project_cannot_decode_more_bodies_at_once_than_its_share() {
        let first = source();
        let sibling = IngestSource {
            id: uuid::Uuid::new_v4(),
            ..first.clone()
        };
        let stranger = source();
        let decodes = Arc::new(Semaphore::new(4));
        let shares = DecodeShares::default();
        let projects = DecodeShares::new(1);
        let good = Bytes::from_static(br#"{"message":"m"}"#);

        let held = projects.acquire(first.project_id).await;
        let waiting = tokio::time::timeout(
            Duration::from_millis(150),
            prepare(
                good.clone(),
                JSON,
                &sibling,
                Utc::now(),
                &decodes,
                &shares,
                &projects,
            ),
        )
        .await;
        assert!(waiting.is_err(), "the project's only place is taken");
        assert_eq!(decodes.available_permits(), 4);
        prepare(
            good.clone(),
            JSON,
            &stranger,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        drop(held);
        prepare(
            good,
            JSON,
            &sibling,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert_eq!(projects.tracked(), 0);
        let defaults = ProjectLimits::default();
        assert_eq!(
            defaults.in_flight.limit(),
            crate::logs::rate_limit::MAX_IN_FLIGHT_PER_PROJECT
        );
        assert_eq!(
            defaults.decode_shares.limit(),
            crate::logs::rate_limit::MAX_DECODES_PER_PROJECT
        );
    }

    /// A body that is large when encoded keeps its place until it has been
    /// sent, so no more of them than there are places exist at once; a small
    /// one gives it back as soon as it is made.
    #[tokio::test]
    async fn a_large_body_keeps_its_place_until_it_is_dropped() {
        let source = source();
        let decodes = Arc::new(Semaphore::new(2));
        let shares = DecodeShares::default();
        let projects = ProjectLimits::default().decode_shares;
        let events = |count: usize| {
            let events: Vec<Value> = (0..count)
                .map(|n| json!({ "message": format!("{n} {}", "x".repeat(60_000)) }))
                .collect();
            Bytes::from(serde_json::to_vec(&events).unwrap())
        };

        let small = prepare(
            events(20),
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert!(small.body.len() < LARGE_BODY_BYTES);
        assert!(small.place.is_none());
        assert_eq!(decodes.available_permits(), 2);
        assert_eq!(shares.count(source.id), 0);

        let large = prepare(
            events(180),
            JSON,
            &source,
            Utc::now(),
            &decodes,
            &shares,
            &projects,
        )
        .await
        .unwrap();
        assert!(large.body.len() > LARGE_BODY_BYTES, "{}", large.body.len());
        assert!(large.place.is_some());
        assert_eq!(decodes.available_permits(), 1, "the engine's place is kept");
        assert_eq!(shares.count(source.id), 1, "and the source's");
        drop(large);
        assert_eq!(decodes.available_permits(), 2);
        assert_eq!(shares.count(source.id), 0);
    }

    /// What a request stores is capped as the rows are built, and the rows
    /// are built no further than the cap.
    #[test]
    fn rows_stop_being_built_at_the_cap() {
        let source = source();
        let events: Vec<Value> = (0..200)
            .map(|n| json!({ "message": format!("{n} {}", "x".repeat(60_000)) }))
            .collect();
        let body = Bytes::from(serde_json::to_vec(&events).unwrap());
        assert!(build_rows(body.clone(), JSON, &source, Utc::now()).is_ok());
        let refused = build_rows_within(body, JSON, &source, Utc::now(), 5 * 1024 * 1024);
        match refused {
            Err(IngestError::PayloadTooLarge(message)) => {
                assert!(message.contains("once stored"), "{message}")
            }
            other => panic!("{:?}", other.map(|rows| rows.len())),
        }
    }
}
