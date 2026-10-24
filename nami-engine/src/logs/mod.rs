//! Log management: sources and their pipelines (`source`), saved views
//! (`view`), log alerts (`alert`), ingestion (`ingest`: HTTP bodies, normalization, per-source
//! pipelines, rate limits, storage in ClickHouse and usage accounting),
//! syslog ingestion over TCP and UDP (`syslog`), and reading (`query`:
//! search, patterns, the live tail and the streamed export).
pub mod alert;
pub mod ingest;
pub mod query;
pub mod source;
pub mod syslog;
pub mod view;

use ingest::{pipeline_cache, rate_limit, usage};
use query::{export, search, slots, tail};

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
    /// Whole-row reads running per user (see
    /// [`search::MAX_ENTRY_READS_PER_USER`]); apart from `query_slots` so a
    /// page's row clicks never compete with its searches.
    pub entry_slots: std::sync::Arc<slots::UserSlots>,
    /// The compiled pipeline of each source, keyed on its `updated_at`.
    pub pipelines: pipeline_cache::PipelineCache,
    /// Exports running per user and in all (see
    /// [`export::MAX_EXPORTS_PER_USER`], [`export::MAX_EXPORTS_PER_INSTANCE`]).
    pub export_slots: std::sync::Arc<slots::UserSlots>,
    /// The caps of every export (tests shrink them).
    pub export_limits: export::ExportLimits,
}
