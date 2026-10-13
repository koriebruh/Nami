use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::Semaphore;

use crate::{
    logs::{
        ingest::{self, IngestError},
        store::{self, LogRow},
    },
    models::source,
    state::AppState,
};

/// How many ingest requests may be handled at once. The endpoint is public
/// and every well-formed token costs a database query: a request that finds
/// every place taken is answered at once with `503` and `Retry-After: 1`
/// (shippers retry on their own) instead of waiting for a pooled connection,
/// so a flood of ingest requests can never starve the API or the uptime
/// worker of the pool they share. A request holds its place from the token
/// check to its reply, which also bounds the memory the bodies in flight can
/// take (each up to [`ingest::MAX_BODY_BYTES`], inflated up to
/// [`ingest::MAX_DECOMPRESSED_BYTES`]). Every phase of a request has a
/// deadline (its body [`ingest::BODY_READ_TIMEOUT`], Postgres its acquire and
/// statement timeouts, ClickHouse the client's 30 s), so a stalled client or
/// backend gives its place back in the end.
///
/// With ClickHouse answering an insert in 10 to 250 ms (it waits for the
/// server-side flush of the async insert), 32 places serve a few hundred
/// requests a second per engine instance; a burst of more concurrent senders
/// than that gets 503 and retries.
pub const MAX_INGEST_IN_FLIGHT: usize = 32;

/// What limits the ingest endpoint besides the sizes of bodies.
#[derive(Clone)]
pub struct IngestLimits {
    /// The places an ingest request holds while it runs (see
    /// [`MAX_INGEST_IN_FLIGHT`]); tests hold some to fill the endpoint up.
    pub places: Arc<Semaphore>,
    /// How long a request may take to deliver its body, see
    /// [`ingest::BODY_READ_TIMEOUT`] (tests shorten it).
    pub body_read_timeout: Duration,
}

impl Default for IngestLimits {
    fn default() -> Self {
        IngestLimits {
            places: Arc::new(Semaphore::new(MAX_INGEST_IN_FLIGHT)),
            body_read_timeout: ingest::BODY_READ_TIMEOUT,
        }
    }
}

/// `POST /api/v1/logs`, served on the public listener only (see
/// `router::build_public_router`). No session: the source token in
/// `Authorization: Bearer` is the credential, and shippers call the engine
/// directly. The token is only ever hashed, never logged. A request must hold
/// one of `limits.places` while it runs.
pub fn public_router(limits: IngestLimits) -> Router<AppState> {
    Router::new()
        .route("/api/v1/logs", post(ingest_logs))
        .layer(Extension(limits))
}

/// Order of checks, each cheaper than the next: the token's shape (401, no
/// query) -> a free place (503, no query) -> the token (401) -> the rate limit
/// (429 `rate_limited`) -> the quota (429 `quota_exceeded`) -> content type
/// (415) -> body size (413) -> parse (400, 413 for too many events) -> store
/// -> 202. Only events that were stored count towards usage.
async fn ingest_logs(
    State(state): State<AppState>,
    Extension(limits): Extension<IngestLimits>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Value>), IngestError> {
    let token = ingest::bearer_token(&headers)
        .filter(|token| source::is_well_formed_token(token))
        .ok_or(IngestError::Unauthorized)?;
    // Held until the handler returns.
    let _place = limits.places.try_acquire().map_err(|_| IngestError::Busy)?;
    let source = source::find_by_token(&state.db, token)
        .await?
        .ok_or(IngestError::Unauthorized)?;
    let received_at = Utc::now();
    let today = received_at.date_naive();

    if !state.logs.rate_limiter.try_acquire(source.id) {
        state.logs.usage.record_rate_limited(source.id, today);
        return Err(IngestError::RateLimited);
    }
    if let Some(quota) = source.daily_byte_quota
        && state
            .logs
            .usage
            .bytes_today(&state.db, source.id, today)
            .await?
            >= quota
    {
        state.logs.usage.record_over_quota(source.id, today, 1);
        return Err(IngestError::QuotaExceeded {
            retry_after_seconds: ingest::seconds_until_utc_midnight(received_at),
        });
    }

    let encoding = ingest::body_encoding(&headers)?;
    let body = ingest::read_body(body, &headers, limits.body_read_timeout).await?;
    let rows = ingest::rows_from_body(body, encoding, &source, received_at).await?;

    if !rows.is_empty() {
        store::insert_logs(&state.clickhouse, &rows).await?;
        let bytes = rows.iter().map(LogRow::stored_bytes).sum();
        state
            .logs
            .usage
            .record_accepted(source.id, today, rows.len() as u64, bytes);
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "accepted": rows.len() })),
    ))
}
