//! Per-source ingestion stats and the daily byte quota. Each engine
//! instance counts in memory ([`UsageTracker`]) and adds its deltas to
//! Postgres `source_usage_daily` every [`FLUSH_INTERVAL`] (additive upsert,
//! so instances never overwrite each other); rows are kept
//! [`RETENTION_DAYS`] days.
//!
//! The quota check reads the day's flushed bytes (cached for at most
//! [`FLUSHED_CACHE_TTL`]) plus this instance's unflushed delta. A request
//! that crosses the quota is accepted whole, so the overshoot is bounded by
//! one request plus about one flush interval of traffic per instance.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use chrono::{NaiveDate, Utc};
use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

pub const FLUSH_INTERVAL: Duration = Duration::from_secs(10);
pub const RETENTION_DAYS: i64 = 400;
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);
pub const FLUSHED_CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct UsageCounters {
    pub events: i64,
    pub bytes: i64,
    pub events_dropped: i64,
    pub events_sampled_out: i64,
    pub requests_rate_limited: i64,
    pub requests_over_quota: i64,
}

impl UsageCounters {
    fn add(&mut self, other: &UsageCounters) {
        self.events += other.events;
        self.bytes += other.bytes;
        self.events_dropped += other.events_dropped;
        self.events_sampled_out += other.events_sampled_out;
        self.requests_rate_limited += other.requests_rate_limited;
        self.requests_over_quota += other.requests_over_quota;
    }
}

/// One day of `GET /projects/{id}/sources/{source_id}/usage`.
#[derive(Debug, Clone, PartialEq, Serialize, sqlx::FromRow)]
pub struct DailyUsage {
    pub day: NaiveDate,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub counters: UsageCounters,
}

#[derive(Debug, Clone, Copy)]
struct FlushedBytes {
    day: NaiveDate,
    bytes: i64,
    fetched_at: Instant,
}

#[derive(Debug, Default)]
pub struct UsageTracker {
    pending: Mutex<HashMap<(Uuid, NaiveDate), UsageCounters>>,
    flushed: Mutex<HashMap<Uuid, FlushedBytes>>,
}

impl UsageTracker {
    pub fn record(&self, source_id: Uuid, day: NaiveDate, delta: UsageCounters) {
        let mut pending = self.pending.lock().expect("usage lock poisoned");
        pending.entry((source_id, day)).or_default().add(&delta);
    }

    pub fn record_accepted(&self, source_id: Uuid, day: NaiveDate, events: u64, bytes: u64) {
        let delta = UsageCounters {
            events: events as i64,
            bytes: bytes as i64,
            ..UsageCounters::default()
        };
        self.record(source_id, day, delta);
    }

    pub fn record_rate_limited(&self, source_id: Uuid, day: NaiveDate) {
        self.record(
            source_id,
            day,
            UsageCounters {
                requests_rate_limited: 1,
                ..UsageCounters::default()
            },
        );
    }

    pub fn record_over_quota(&self, source_id: Uuid, day: NaiveDate, rejected: u64) {
        let delta = UsageCounters {
            requests_over_quota: rejected as i64,
            ..UsageCounters::default()
        };
        self.record(source_id, day, delta);
    }

    /// This instance's counters not yet flushed.
    pub fn pending(&self, source_id: Uuid, day: NaiveDate) -> UsageCounters {
        let pending = self.pending.lock().expect("usage lock poisoned");
        pending.get(&(source_id, day)).copied().unwrap_or_default()
    }

    /// Bytes accepted for the source on `day`: flushed (all instances) plus
    /// this instance's unflushed delta.
    pub async fn bytes_today(
        &self,
        pool: &PgPool,
        source_id: Uuid,
        day: NaiveDate,
    ) -> Result<i64, sqlx::Error> {
        let cached = {
            let flushed = self.flushed.lock().expect("usage lock poisoned");
            flushed
                .get(&source_id)
                .filter(|entry| entry.day == day && entry.fetched_at.elapsed() < FLUSHED_CACHE_TTL)
                .map(|entry| entry.bytes)
        };
        let flushed_bytes = match cached {
            Some(bytes) => bytes,
            None => {
                let bytes: i64 = sqlx::query_scalar(
                    "select coalesce((select bytes from source_usage_daily where source_id = $1 and day = $2), 0)",
                )
                .bind(source_id)
                .bind(day)
                .fetch_one(pool)
                .await?;
                self.remember_flushed(source_id, day, bytes);
                bytes
            }
        };
        Ok(flushed_bytes + self.pending(source_id, day).bytes)
    }

    fn remember_flushed(&self, source_id: Uuid, day: NaiveDate, bytes: i64) {
        let mut flushed = self.flushed.lock().expect("usage lock poisoned");
        let newer = flushed.get(&source_id).is_none_or(|entry| entry.day <= day);
        if newer {
            flushed.insert(
                source_id,
                FlushedBytes {
                    day,
                    bytes,
                    fetched_at: Instant::now(),
                },
            );
        }
    }

    /// Adds every pending delta to `source_usage_daily` in one statement and
    /// returns how many (source, day) rows it touched. Counters of deleted
    /// sources are dropped; on a database error every delta is kept for the
    /// next flush.
    pub async fn flush(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let batch = std::mem::take(&mut *self.pending.lock().expect("usage lock poisoned"));
        if batch.is_empty() {
            return Ok(0);
        }
        let mut source_ids = Vec::with_capacity(batch.len());
        let mut days = Vec::with_capacity(batch.len());
        let mut columns: [Vec<i64>; 6] = Default::default();
        for ((source_id, day), c) in &batch {
            source_ids.push(*source_id);
            days.push(*day);
            for (column, value) in columns.iter_mut().zip([
                c.events,
                c.bytes,
                c.events_dropped,
                c.events_sampled_out,
                c.requests_rate_limited,
                c.requests_over_quota,
            ]) {
                column.push(value);
            }
        }
        let [
            events,
            bytes,
            dropped,
            sampled_out,
            rate_limited,
            over_quota,
        ] = columns;
        let result: Result<Vec<(Uuid, NaiveDate, i64)>, sqlx::Error> = sqlx::query_as(
            "insert into source_usage_daily as u
                 (source_id, day, events, bytes, events_dropped, events_sampled_out,
                  requests_rate_limited, requests_over_quota)
             select d.source_id, d.day, d.events, d.bytes, d.events_dropped, d.events_sampled_out,
                    d.requests_rate_limited, d.requests_over_quota
             from unnest($1::uuid[], $2::date[], $3::bigint[], $4::bigint[], $5::bigint[], $6::bigint[],
                         $7::bigint[], $8::bigint[])
                  as d(source_id, day, events, bytes, events_dropped, events_sampled_out,
                       requests_rate_limited, requests_over_quota)
             where exists (select 1 from sources s where s.id = d.source_id)
             on conflict (source_id, day) do update set
                 events = u.events + excluded.events,
                 bytes = u.bytes + excluded.bytes,
                 events_dropped = u.events_dropped + excluded.events_dropped,
                 events_sampled_out = u.events_sampled_out + excluded.events_sampled_out,
                 requests_rate_limited = u.requests_rate_limited + excluded.requests_rate_limited,
                 requests_over_quota = u.requests_over_quota + excluded.requests_over_quota
             returning source_id, day, bytes",
        )
        .bind(&source_ids)
        .bind(&days)
        .bind(&events)
        .bind(&bytes)
        .bind(&dropped)
        .bind(&sampled_out)
        .bind(&rate_limited)
        .bind(&over_quota)
        .fetch_all(pool)
        .await;
        match result {
            Ok(rows) => {
                for (source_id, day, total) in &rows {
                    self.remember_flushed(*source_id, *day, *total);
                }
                Ok(rows.len())
            }
            Err(e) => {
                let mut pending = self.pending.lock().expect("usage lock poisoned");
                for (key, counters) in batch {
                    pending.entry(key).or_default().add(&counters);
                }
                Err(e)
            }
        }
    }
}

/// Keeps the last [`RETENTION_DAYS`] days (today included); deletes the rest.
pub async fn prune(pool: &PgPool, today: NaiveDate) -> Result<u64, sqlx::Error> {
    let cutoff = today - chrono::Duration::days(RETENTION_DAYS - 1);
    let result = sqlx::query("delete from source_usage_daily where day < $1")
        .bind(cutoff)
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}

/// The last `days` days up to `today`, oldest first, zero-filled.
pub async fn daily_usage(
    pool: &PgPool,
    source_id: Uuid,
    days: i64,
    today: NaiveDate,
) -> Result<Vec<DailyUsage>, sqlx::Error> {
    sqlx::query_as(
        "select d.day::date as day,
                coalesce(u.events, 0) as events, coalesce(u.bytes, 0) as bytes,
                coalesce(u.events_dropped, 0) as events_dropped,
                coalesce(u.events_sampled_out, 0) as events_sampled_out,
                coalesce(u.requests_rate_limited, 0) as requests_rate_limited,
                coalesce(u.requests_over_quota, 0) as requests_over_quota
         from generate_series($2::date - ($3::int - 1), $2::date, interval '1 day') as d(day)
         left join source_usage_daily u on u.source_id = $1 and u.day = d.day::date
         order by d.day",
    )
    .bind(source_id)
    .bind(today)
    .bind(days as i32)
    .fetch_all(pool)
    .await
}

/// Flushes every [`FLUSH_INTERVAL`] and prunes hourly, forever. Runs on
/// every instance that ingests.
pub async fn run_forever(state: AppState) {
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_prune: Option<Instant> = None;
    loop {
        ticker.tick().await;
        if let Err(e) = state.logs.usage.flush(&state.db).await {
            tracing::error!(error = %e, "failed to flush log ingestion usage");
        }
        if last_prune.is_none_or(|at| at.elapsed() >= PRUNE_INTERVAL) {
            match prune(&state.db, Utc::now().date_naive()).await {
                Ok(_) => last_prune = Some(Instant::now()),
                Err(e) => tracing::error!(error = %e, "failed to prune log ingestion usage"),
            }
        }
    }
}
