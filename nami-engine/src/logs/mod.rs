//! Log management: turning ingested events into rows (`normalize`,
//! `pattern`), storing them in ClickHouse (`store`), and HTTP ingestion
//! (`ingest`, `msgpack`, with the `budget` that caps what a body may decode
//! to) with its per-source limits (`rate_limit`) and usage accounting
//! (`usage`), syslog ingestion over TCP and UDP (`syslog`), and search
//! (`query`, `search`) and the live tail (`tail`).
pub mod budget;
pub mod ingest;
pub mod msgpack;
pub mod normalize;
pub mod pattern;
pub mod query;
pub mod rate_limit;
pub mod search;
pub mod store;
pub mod syslog;
pub mod tail;
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
    /// How many bodies of each source are decoded at once (see
    /// [`rate_limit::MAX_DECODES_PER_SOURCE`]).
    pub decode_shares: rate_limit::DecodeShares,
    /// The same two limits per project (see
    /// [`rate_limit::MAX_IN_FLIGHT_PER_PROJECT`]).
    pub project_limits: rate_limit::ProjectLimits,
    pub usage: usage::UsageTracker,
    /// Live-tail timings (tests shorten them) and open-stream counts.
    pub tail: tail::TailTimings,
    pub tail_slots: tail::TailSlots,
    /// Searches and histograms running per user (see
    /// [`search::MAX_QUERIES_PER_USER`]).
    pub query_slots: search::QuerySlots,
}
