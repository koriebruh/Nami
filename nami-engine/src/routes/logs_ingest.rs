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
    core::clickhouse::ClickHouse,
    core::state::AppState,
    logs::{
        LogsState,
        ingest::{self, BodyEncoding, BodyPace, IngestError, Prepared},
        pipeline::CompiledPipeline,
        rate_limit::InFlightGuard,
        store,
    },
    models::source::{self, IngestSource},
};

/// How many ingest requests may be handled at once. The endpoint is public: a
/// request that finds every place taken is answered at once with `503` and
/// `Retry-After: 1` (shippers retry on their own), before any database work.
/// A request holds its place from the token check until its work ends, also
/// when the client has gone away (see `Work`), and one source may hold at
/// most [`crate::logs::rate_limit::MAX_IN_FLIGHT_PER_SOURCE`] of them and one
/// project at most [`crate::logs::rate_limit::MAX_IN_FLIGHT_PER_PROJECT`], so
/// neither one token nor one project with many sources can take them all.
///
/// The places bound the requests, and so the bodies *as received* (each up to
/// [`ingest::MAX_BODY_BYTES`]); they do not bound what a body takes once it is
/// decoded and stored. That is bounded separately:
/// - at most [`MAX_DECODES_IN_FLIGHT`] bodies are inflated, decoded and turned
///   into rows at a time, and at most
///   [`crate::logs::rate_limit::MAX_DECODES_PER_SOURCE`] of one source;
/// - a decode may build up to [`crate::logs::budget::MAX_DECODED_BYTES`] of
///   values and [`ingest::MAX_ENCODED_BYTES`] of rows and of insert body, over
///   which the request is a 413; the worst case for one decode is about 300 MiB
///   (ordinary batches take a few MiB);
/// - after the decode only the encoded insert body is kept, until ClickHouse
///   has taken it. A body above [`ingest::LARGE_BODY_BYTES`] keeps its decode
///   place meanwhile, so no more than [`MAX_DECODES_IN_FLIGHT`] of those exist
///   at once, and the smaller ones are bounded by the places of the endpoint.
///
/// All of it together stays under 2 GiB however the requests are made up (the
/// bodies as received, 32 x 10 MiB; four decodes or large bodies, 4 x 300
/// MiB; the small bodies waiting for ClickHouse, 28 x 8 MiB).
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
/// lookup, the quota check and, on a pipeline cache miss, the pipeline read;
/// the rest of a request touches only ClickHouse).
/// It has to stay below the pool (`DATABASE_MAX_CONNECTIONS`, 20 by default):
/// whatever the load on the public port, the API and the uptime worker always
/// find connections free. A request that waits for one of these places holds
/// no connection, only its place of [`MAX_INGEST_IN_FLIGHT`].
pub const MAX_INGEST_QUERIES_IN_FLIGHT: usize = 8;

/// How many bodies may be inflated, decoded and turned into rows at once:
/// that work takes memory in proportion to what a body decodes to, and the
/// places of the endpoint alone would let 32 hostile bodies do it together. A
/// body that turns out large keeps its place until it has been sent (see
/// [`ingest::LARGE_BODY_BYTES`]).
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
/// `core::router::build_public_router`). No session: the source token in
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
/// `too_many_in_flight`) -> the project's requests in flight (429
/// `too_many_in_flight`) -> the quota (429 `quota_exceeded`) -> content type
/// (415) -> body size and pace (413, 408) -> the source's compiled pipeline
/// (cached per `updated_at`; one that no longer compiles is a 500 and nothing
/// is stored) -> parse, pipeline (400, 413 for too many events or too much
/// decoded) -> store -> 202 with the events stored. Only events that were
/// stored count towards usage; what the pipeline dropped or sampled out is
/// counted apart.
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
    // The project's cap too: however many sources it has, one project never
    // holds more than its share of the endpoint's places.
    let Some(project_in_flight) = state
        .logs
        .project_limits
        .in_flight
        .try_enter(source.project_id)
    else {
        state.logs.usage.record_rate_limited(source.id, today);
        return Err(IngestError::ProjectTooManyInFlight {
            limit: state.logs.project_limits.in_flight.limit(),
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
    let pipeline = state
        .logs
        .pipelines
        .for_source(&state.db, &source, Some(&limits.queries))
        .await
        .map_err(|error| {
            // Never the rules: a pattern can hold a secret's shape (the error
            // names only the rule index and a fixed reason).
            tracing::error!(%error, source_id = %source.id, "log pipeline unavailable");
            IngestError::Internal
        })?;

    let work = Work {
        clickhouse: state.clickhouse.clone(),
        logs: state.logs.clone(),
        decodes: limits.decodes.clone(),
        source,
        pipeline,
        received_at,
        _place: place,
        _in_flight: in_flight,
        _project_in_flight: project_in_flight,
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
    pipeline: Arc<CompiledPipeline>,
    received_at: DateTime<Utc>,
    _place: OwnedSemaphorePermit,
    _in_flight: InFlightGuard,
    _project_in_flight: InFlightGuard,
}

impl Work {
    /// Decodes the body, runs the pipeline, inserts the events and counts
    /// them: usage is recorded right after the insert, with nothing that can
    /// be cancelled in between. Returns how many events were stored. A failed
    /// insert records nothing, the pipeline's counts included (the client
    /// retries the whole request).
    async fn store(self, body: Bytes, encoding: BodyEncoding) -> Result<usize, IngestError> {
        let Prepared {
            body: encoded,
            events,
            bytes,
            pipeline,
            // A large body keeps its place for decoding until the end.
            place: _held,
        } = ingest::prepare(
            body,
            encoding,
            &self.source,
            self.pipeline,
            self.received_at,
            &self.decodes,
            &self.logs.decode_shares,
            &self.logs.project_limits.decode_shares,
        )
        .await?;
        let day = self.received_at.date_naive();
        if events > 0 {
            store::insert_encoded_logs(&self.clickhouse, encoded)
                .await
                .map_err(|error| {
                    // Logged here: with the client gone there is no response to
                    // carry it. ClickHouse's text can include SQL and schema
                    // details, so the client only ever sees "internal error".
                    tracing::error!(%error, source_id = %self.source.id, "clickhouse error");
                    IngestError::Internal
                })?;
            self.logs
                .usage
                .record_accepted(self.source.id, day, events as u64, bytes);
        }
        // Zero events stored (every one dropped or sampled out) sends no
        // insert, and its counts are recorded all the same.
        for (source_id, (dropped, sampled_out)) in pipeline {
            self.logs
                .usage
                .record_pipeline(source_id, day, dropped, sampled_out);
        }
        Ok(events)
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
