//! Log management: turning ingested events into rows (`normalize`,
//! `pattern`), storing them in ClickHouse (`store`), and HTTP ingestion
//! (`ingest`, `msgpack`, with the `budget` that caps what a body may decode
//! to) with its per-source limits (`rate_limit`) and usage accounting
//! (`usage`).
pub mod budget;
pub mod ingest;
pub mod msgpack;
pub mod normalize;
pub mod pattern;
pub mod rate_limit;
pub mod store;
pub mod usage;

/// In-memory state of the ingest paths, one per engine instance: rate
/// limits, requests in flight and unflushed usage counters are local to the
/// instance.
#[derive(Debug, Default)]
pub struct LogsState {
    pub rate_limiter: rate_limit::TokenBuckets,
    /// How many requests each source has in flight (capped, see
    /// [`rate_limit::MAX_IN_FLIGHT_PER_SOURCE`]).
    pub in_flight: rate_limit::InFlight,
    pub usage: usage::UsageTracker,
}
