//! The compiled pipeline of each source, cached per engine instance and keyed
//! on `sources.updated_at` (R-P2-6): every write to a source moves that value
//! forward, and both token lookups return it, so a hit costs no query and an
//! edit applies to the very next request or syslog batch. A miss reads
//! `sources.pipeline` (one indexed query) and compiles it off the async
//! runtime. The cache holds at most [`MAX_CACHED_PIPELINES`] entries and is
//! cleared when full; with at most 20 regexes of bounded size per pipeline
//! (`pipeline::MAX_REGEXES`, R-P2-26) that bounds its memory. Clearing is
//! deliberately simple, not LRU: an instance ingesting for more than 1,024
//! active sources at once keeps clearing and recompiling (a performance
//! cliff, not a correctness one). Raise the bound or switch to LRU before
//! such deployments.
//!
//! A pipeline that cannot be had is an error, never "no rules": a stored one
//! that no longer compiles, a source deleted between its token lookup and the
//! pipeline read, or a database error. A redact rule that silently stopped
//! applying would leak, so ingestion for that source fails closed.

use std::{collections::HashMap, sync::Arc, sync::Mutex};

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

use super::pipeline::{CompiledPipeline, compile, parse_rules};
use crate::logs::source::model::{self as source, IngestSource};

/// Most sources whose compiled pipeline is kept (R-P2-6, R-P2-26).
pub const MAX_CACHED_PIPELINES: usize = 1024;

type Entries = HashMap<Uuid, (DateTime<Utc>, Arc<CompiledPipeline>)>;

#[derive(Debug, Default)]
pub struct PipelineCache {
    entries: Mutex<Entries>,
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineLoadError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("the source's stored pipeline does not compile: {0}")]
    Invalid(String),
    /// The source was deleted after its token was looked up.
    #[error("the source no longer exists")]
    Gone,
}

impl PipelineCache {
    /// The cached rules when they belong to `updated_at` exactly.
    pub fn get(&self, source_id: Uuid, updated_at: DateTime<Utc>) -> Option<Arc<CompiledPipeline>> {
        let entries = self.entries.lock().expect("pipeline cache lock poisoned");
        entries
            .get(&source_id)
            .filter(|(at, _)| *at == updated_at)
            .map(|(_, pipeline)| Arc::clone(pipeline))
    }

    /// Stores the rules of `source_id` as of `updated_at`, replacing what was
    /// cached for it unless that is newer (a slow compile of older rules must
    /// not push out a newer version). A full cache is cleared first.
    pub fn put(&self, source_id: Uuid, updated_at: DateTime<Utc>, pipeline: Arc<CompiledPipeline>) {
        let mut entries = self.entries.lock().expect("pipeline cache lock poisoned");
        if let Some((at, _)) = entries.get(&source_id) {
            if *at > updated_at {
                return;
            }
        } else if entries.len() >= MAX_CACHED_PIPELINES {
            entries.clear();
        }
        entries.insert(source_id, (updated_at, pipeline));
    }

    pub fn len(&self) -> usize {
        self.entries
            .lock()
            .expect("pipeline cache lock poisoned")
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The compiled rules of `source`: from the cache when they belong to its
    /// `updated_at`, otherwise loaded, compiled and cached (under the
    /// `updated_at` the load returned, which may be newer than the token
    /// lookup's). `db_permits` (HTTP: `IngestLimits::queries`; syslog: `None`)
    /// guards only the Postgres read; the compile runs in `spawn_blocking`
    /// after the permit is released. A source deleted meanwhile is
    /// [`PipelineLoadError::Gone`] (fail closed: its events are not stored
    /// without its rules).
    pub async fn for_source(
        &self,
        pool: &PgPool,
        source: &IngestSource,
        db_permits: Option<&Semaphore>,
    ) -> Result<Arc<CompiledPipeline>, PipelineLoadError> {
        if let Some(pipeline) = self.get(source.id, source.updated_at) {
            return Ok(pipeline);
        }
        let loaded = {
            let _permit = match db_permits {
                Some(permits) => Some(permits.acquire().await.map_err(|_| {
                    PipelineLoadError::Invalid("the ingest query places are closed".to_string())
                })?),
                None => None,
            };
            source::load_pipeline(pool, source.id).await?
        };
        let Some((rules, updated_at)) = loaded else {
            return Err(PipelineLoadError::Gone);
        };
        if let Some(pipeline) = self.get(source.id, updated_at) {
            return Ok(pipeline);
        }
        let compiled = tokio::task::spawn_blocking(move || {
            parse_rules(&rules)
                .and_then(|rules| compile(&rules))
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| PipelineLoadError::Invalid(format!("compile task failed: {e}")))?
        .map_err(PipelineLoadError::Invalid)?;
        let compiled = Arc::new(compiled);
        self.put(source.id, updated_at, Arc::clone(&compiled));
        Ok(compiled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_used_only_for_its_own_updated_at() {
        let cache = PipelineCache::default();
        let id = Uuid::new_v4();
        let t0 = Utc::now();
        let t1 = t0 + chrono::Duration::microseconds(1);
        cache.put(id, t0, Arc::new(CompiledPipeline::empty()));
        assert!(cache.get(id, t0).is_some());
        assert!(cache.get(id, t1).is_none(), "an edited source misses");
        assert!(cache.get(Uuid::new_v4(), t0).is_none());
        cache.put(id, t1, Arc::new(CompiledPipeline::empty()));
        assert!(cache.get(id, t0).is_none(), "the older rules are replaced");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn an_entry_is_never_replaced_by_older_rules() {
        let cache = PipelineCache::default();
        let id = Uuid::new_v4();
        let t0 = Utc::now();
        let t1 = t0 + chrono::Duration::microseconds(1);
        cache.put(id, t1, Arc::new(CompiledPipeline::empty()));
        cache.put(id, t0, Arc::new(CompiledPipeline::empty()));
        assert!(cache.get(id, t1).is_some(), "the newer rules stay");
        assert!(cache.get(id, t0).is_none());
    }

    #[test]
    fn the_cache_is_cleared_when_full() {
        let cache = PipelineCache::default();
        let t = Utc::now();
        for _ in 0..MAX_CACHED_PIPELINES {
            cache.put(Uuid::new_v4(), t, Arc::new(CompiledPipeline::empty()));
        }
        assert_eq!(cache.len(), MAX_CACHED_PIPELINES);
        let last = Uuid::new_v4();
        cache.put(last, t, Arc::new(CompiledPipeline::empty()));
        assert_eq!(cache.len(), 1, "cleared, then the new entry stored");
        assert!(cache.get(last, t).is_some());
    }
}
