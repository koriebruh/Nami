use std::sync::Arc;

use axum::{
    Extension, Json, Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{
    clickhouse::ClickHouse,
    logs::{
        LogsState,
        ingest::{self, BodyEncoding, BodyPace, IngestError},
        rate_limit::InFlightGuard,
        store,
    },
    models::source::{self, IngestSource},
    state::AppState,
};

/// How many ingest requests may be handled at once. The endpoint is public: a
/// request that finds every place taken is answered at once with `503` and
/// `Retry-After: 1` (shippers retry on their own), before any database work.
/// A request holds its place from the token check until its work ends, also
/// when the client has gone away (see `Work`), and one source may hold at
/// most [`crate::logs::rate_limit::MAX_IN_FLIGHT_PER_SOURCE`] of them, so one
/// token cannot take them all.
///
/// The places bound the requests, and so the bodies *as received* (each up to
/// [`ingest::MAX_BODY_BYTES`]); they do not bound what a body takes once it is
/// decoded. That is bounded separately: at most [`MAX_DECODES_IN_FLIGHT`]
/// bodies are inflated, decoded and turned into rows at a time, each decode is
/// capped by [`crate::logs::budget::MAX_DECODED_BYTES`], and after it a
/// request keeps only the encoded insert body until ClickHouse has taken it.
/// Ordinary batches take a few MiB; the worst case per decode is a few hundred
/// MiB (the budget, the inflated body of up to 50 MiB, and the rows).
///
/// Every phase has a deadline, so a stalled client or backend gives its place
/// back in the end: the body arrives at the pace of [`BodyPace`], Postgres has
/// its acquire and statement timeouts, ClickHouse the client's 30 s.
///
/// With ClickHouse answering an insert in 10 to 250 ms (it waits for the
/// server-side flush of the async insert), 32 places serve a few hundred
/// requests a second per engine instance; a burst of more concurrent senders
/// than that gets 503 and retries.
pub const MAX_INGEST_IN_FLIGHT: usize = 32;

/// How many Postgres statements ingest may have running at once (the token
/// lookup and the quota check; the rest of a request touches only ClickHouse).
/// It has to stay below the pool (`DATABASE_MAX_CONNECTIONS`, 20 by default):
/// whatever the load on the public port, the API and the uptime worker always
/// find connections free. A request that waits for one of these places holds
/// no connection, only its place of [`MAX_INGEST_IN_FLIGHT`].
pub const MAX_INGEST_QUERIES_IN_FLIGHT: usize = 8;

/// How many bodies may be inflated, decoded and turned into rows at once:
/// that work takes memory in proportion to what a body decodes to, and the
/// places of the endpoint alone would let 32 hostile bodies do it together.
pub const MAX_DECODES_IN_FLIGHT: usize = 4;

/// What limits the ingest endpoint besides the sizes of bodies.
#[derive(Clone)]
pub struct IngestLimits {
    /// The places a request holds while it runs ([`MAX_INGEST_IN_FLIGHT`]);
    /// tests hold some to fill the endpoint up.
    pub places: Arc<Semaphore>,
    /// The Postgres statements ingest has running ([`MAX_INGEST_QUERIES_IN_FLIGHT`]).
    pub queries: Arc<Semaphore>,
    /// The bodies being decoded ([`MAX_DECODES_IN_FLIGHT`]).
    pub decodes: Arc<Semaphore>,
    /// How fast a body has to arrive (tests shorten it).
    pub body_pace: BodyPace,
}

impl IngestLimits {
    /// The limits for an engine whose Postgres pool holds `max_connections`:
    /// ingest never takes more than half of them, so the rest are always the
    /// API's and the uptime worker's.
    pub fn for_pool(max_connections: u32) -> IngestLimits {
        let queries = MAX_INGEST_QUERIES_IN_FLIGHT
            .min((max_connections / 2) as usize)
            .max(1);
        IngestLimits {
            places: Arc::new(Semaphore::new(MAX_INGEST_IN_FLIGHT)),
            queries: Arc::new(Semaphore::new(queries)),
            decodes: Arc::new(Semaphore::new(MAX_DECODES_IN_FLIGHT)),
            body_pace: BodyPace::DEFAULT,
        }
    }
}

impl Default for IngestLimits {
    /// For the default pool of 20 connections.
    fn default() -> Self {
        IngestLimits::for_pool(20)
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
/// (429 `rate_limited`) -> the source's requests in flight (429
/// `too_many_in_flight`) -> the quota (429 `quota_exceeded`) -> content type
/// (415) -> body size and pace (413, 408) -> parse (400, 413 for too many
/// events or too much decoded) -> store -> 202. Only events that were stored
/// count towards usage.
async fn ingest_logs(
    State(state): State<AppState>,
    Extension(limits): Extension<IngestLimits>,
    headers: HeaderMap,
    body: Body,
) -> Result<(StatusCode, Json<Value>), IngestError> {
    let token = ingest::bearer_token(&headers)
        .filter(|token| source::is_well_formed_token(token))
        .ok_or(IngestError::Unauthorized)?;
    // Owned, so that it can travel with the work (see `Work`).
    let place = limits
        .places
        .clone()
        .try_acquire_owned()
        .map_err(|_| IngestError::Busy)?;
    let source = {
        let _query = limits
            .queries
            .acquire()
            .await
            .map_err(|_| IngestError::Internal)?;
        source::find_by_token(&state.db, token).await?
    }
    .ok_or(IngestError::Unauthorized)?;
    let received_at = Utc::now();
    let today = received_at.date_naive();

    if !state.logs.rate_limiter.try_acquire(source.id) {
        state.logs.usage.record_rate_limited(source.id, today);
        return Err(IngestError::RateLimited);
    }
    let Some(in_flight) = state.logs.in_flight.try_enter(source.id) else {
        state.logs.usage.record_rate_limited(source.id, today);
        return Err(IngestError::TooManyInFlight {
            limit: state.logs.in_flight.limit(),
        });
    };
    if let Some(quota) = source.daily_byte_quota {
        let used = {
            let _query = limits
                .queries
                .acquire()
                .await
                .map_err(|_| IngestError::Internal)?;
            state
                .logs
                .usage
                .bytes_today(&state.db, source.id, today)
                .await?
        };
        if used >= quota {
            state.logs.usage.record_over_quota(source.id, today, 1);
            return Err(IngestError::QuotaExceeded {
                retry_after_seconds: ingest::seconds_until_utc_midnight(received_at),
            });
        }
    }

    let encoding = ingest::body_encoding(&headers)?;
    let body = match ingest::read_body(body, &headers, &limits.body_pace).await {
        Err(IngestError::RequestTimeout) => {
            // A stalled or crawling upload: worth a line, with the source and
            // never its token.
            tracing::warn!(source_id = %source.id, "ingest request body did not arrive in time");
            return Err(IngestError::RequestTimeout);
        }
        other => other?,
    };

    let work = Work {
        clickhouse: state.clickhouse.clone(),
        logs: state.logs.clone(),
        decodes: limits.decodes.clone(),
        source,
        received_at,
        _place: place,
        _in_flight: in_flight,
    };
    let accepted = tokio::spawn(work.store(body, encoding))
        .await
        .map_err(|error| {
            tracing::error!(%error, "log ingest task failed");
            IngestError::Internal
        })??;
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": accepted }))))
}

/// A request whose body is in, with what has to travel with it until its work
/// ends. The work runs in a task of its own, which keeps all of this (so the
/// place of the endpoint and the source's count stay taken) whatever becomes
/// of the client: hyper drops the handler as soon as the client hangs up, but
/// a blocking decode cannot be cancelled and an insert that has been sent
/// lands whether or not anyone waits for it. Run in the handler, the place
/// would be free while the work still used its memory, and the events of such
/// a request would be stored without ever counting towards usage and quotas.
struct Work {
    clickhouse: ClickHouse,
    logs: Arc<LogsState>,
    decodes: Arc<Semaphore>,
    source: IngestSource,
    received_at: DateTime<Utc>,
    _place: OwnedSemaphorePermit,
    _in_flight: InFlightGuard,
}

impl Work {
    /// Decodes the body, inserts the events and counts them: usage is
    /// recorded right after the insert, with nothing that can be cancelled
    /// in between. Returns how many events were stored.
    async fn store(self, body: Bytes, encoding: BodyEncoding) -> Result<usize, IngestError> {
        let prepared = ingest::prepare(
            body,
            encoding,
            &self.source,
            self.received_at,
            &self.decodes,
        )
        .await?;
        if prepared.events > 0 {
            store::insert_encoded_logs(&self.clickhouse, prepared.body)
                .await
                .map_err(|error| {
                    // Logged here: with the client gone there is no response to
                    // carry it. ClickHouse's text can include SQL and schema
                    // details, so the client only ever sees "internal error".
                    tracing::error!(%error, "clickhouse error");
                    IngestError::Internal
                })?;
            self.logs.usage.record_accepted(
                self.source.id,
                self.received_at.date_naive(),
                prepared.events as u64,
                prepared.bytes,
            );
        }
        Ok(prepared.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ingest never has more Postgres statements running than half the pool
    /// (and at most [`MAX_INGEST_QUERIES_IN_FLIGHT`], and at least one), so the
    /// API and the uptime worker always find connections free.
    #[test]
    fn ingest_leaves_at_least_half_the_pool_to_the_rest() {
        let queries = |pool| IngestLimits::for_pool(pool).queries.available_permits();
        assert_eq!(queries(20), MAX_INGEST_QUERIES_IN_FLIGHT);
        assert_eq!(queries(100), MAX_INGEST_QUERIES_IN_FLIGHT);
        assert_eq!(queries(16), 8);
        assert_eq!(queries(10), 5);
        assert_eq!(queries(4), 2);
        assert_eq!(queries(2), 1);
        assert_eq!(queries(1), 1);
        for pool in 1..=64u32 {
            let own = queries(pool) as u32;
            assert!(own <= pool.div_ceil(2), "pool {pool}: ingest takes {own}");
        }
    }

    #[test]
    fn the_default_places_are_the_documented_ones() {
        let limits = IngestLimits::default();
        assert_eq!(limits.places.available_permits(), MAX_INGEST_IN_FLIGHT);
        assert_eq!(limits.decodes.available_permits(), MAX_DECODES_IN_FLIGHT);
        assert_eq!(
            limits.queries.available_permits(),
            MAX_INGEST_QUERIES_IN_FLIGHT
        );
        assert_eq!(limits.body_pace, BodyPace::DEFAULT);
    }
}
