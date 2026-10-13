//! Log management: turning ingested events into rows (`normalize`,
//! `pattern`), storing them in ClickHouse (`store`), and HTTP ingestion
//! (`ingest`, `msgpack`) with its per-source rate limit (`rate_limit`) and
//! usage accounting (`usage`).
pub mod ingest;
pub mod msgpack;
pub mod normalize;
pub mod pattern;
pub mod rate_limit;
pub mod store;
pub mod usage;

/// In-memory state of the ingest paths, one per engine instance: rate
/// limits and unflushed usage counters are local to the instance.
#[derive(Debug, Default)]
pub struct LogsState {
    pub rate_limiter: rate_limit::TokenBuckets,
    pub usage: usage::UsageTracker,
}
