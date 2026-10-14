//! Per-source limits for HTTP ingestion, in memory and local to each engine
//! instance: a token bucket per source id (50 requests/s, burst 100) and a
//! cap on how many requests of one source may be in flight (4). Only
//! authenticated sources get an entry, so the maps are bounded by the number
//! of sources.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use uuid::Uuid;

pub const REQUESTS_PER_SECOND: f64 = 50.0;
pub const BURST: f64 = 100.0;
/// How many requests of one source may be in flight at once. A request holds
/// a place of the endpoint (`routes::logs_ingest::MAX_INGEST_IN_FLIGHT`) for as
/// long as it runs, so without this one source, with one token, could take
/// them all and leave every other source with 503. At the ClickHouse latency
/// of an async insert (10 to 250 ms) four requests at a time are about the 50
/// requests per second the rate limit allows.
pub const MAX_IN_FLIGHT_PER_SOURCE: usize = 4;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

#[derive(Debug)]
pub struct TokenBuckets {
    rate: f64,
    burst: f64,
    buckets: Mutex<HashMap<Uuid, Bucket>>,
}

impl Default for TokenBuckets {
    fn default() -> Self {
        TokenBuckets::new(REQUESTS_PER_SECOND, BURST)
    }
}

impl TokenBuckets {
    /// `rate` tokens per second refill each bucket, up to `burst`; a new
    /// bucket starts full.
    pub fn new(rate: f64, burst: f64) -> TokenBuckets {
        TokenBuckets {
            rate,
            burst,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token from `key`'s bucket; `false` when it is empty.
    pub fn try_acquire(&self, key: Uuid) -> bool {
        self.try_acquire_at(key, Instant::now())
    }

    pub fn try_acquire_at(&self, key: Uuid, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        let bucket = buckets.entry(key).or_insert(Bucket {
            tokens: self.burst,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.rate).min(self.burst);
        bucket.updated = bucket.updated.max(now);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// How many requests each source has in flight, capped at a maximum.
#[derive(Debug)]
pub struct InFlight {
    max: usize,
    counts: Arc<Mutex<HashMap<Uuid, usize>>>,
}

impl Default for InFlight {
    fn default() -> Self {
        InFlight::new(MAX_IN_FLIGHT_PER_SOURCE)
    }
}

impl InFlight {
    pub fn new(max: usize) -> InFlight {
        InFlight {
            max,
            counts: Arc::default(),
        }
    }

    /// The most requests one source may have in flight.
    pub fn limit(&self) -> usize {
        self.max
    }

    /// Counts a request of `source`, or `None` when it already has
    /// [`Self::limit`] in flight. The request stays counted until the returned
    /// guard is dropped, so hand the guard to whatever does the work.
    pub fn try_enter(&self, source: Uuid) -> Option<InFlightGuard> {
        let mut counts = lock(&self.counts);
        let count = counts.get(&source).copied().unwrap_or(0);
        if count >= self.max {
            return None;
        }
        counts.insert(source, count + 1);
        Some(InFlightGuard {
            source,
            counts: self.counts.clone(),
        })
    }

    /// How many requests `source` has in flight now.
    pub fn count(&self, source: Uuid) -> usize {
        lock(&self.counts).get(&source).copied().unwrap_or(0)
    }
}

/// One request in flight; its source's count goes down when it is dropped.
#[derive(Debug)]
pub struct InFlightGuard {
    source: Uuid,
    counts: Arc<Mutex<HashMap<Uuid, usize>>>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        let mut counts = lock(&self.counts);
        if let Some(count) = counts.get_mut(&self.source) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                counts.remove(&self.source);
            }
        }
    }
}

/// The counts stay valid whatever a panicking holder left behind: every
/// update is a single step.
fn lock(counts: &Mutex<HashMap<Uuid, usize>>) -> MutexGuard<'_, HashMap<Uuid, usize>> {
    counts
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn the_default_is_50_per_second_with_a_burst_of_100() {
        let limiter = TokenBuckets::default();
        let key = Uuid::new_v4();
        let t0 = Instant::now();
        for i in 0..100 {
            assert!(
                limiter.try_acquire_at(key, t0),
                "request {i} is within the burst"
            );
        }
        assert!(!limiter.try_acquire_at(key, t0), "the 101st is limited");
        // 50 per second: one new token every 20 ms (fractions carry over).
        assert!(!limiter.try_acquire_at(key, t0 + Duration::from_millis(19)));
        assert!(limiter.try_acquire_at(key, t0 + Duration::from_millis(30)));
        assert!(!limiter.try_acquire_at(key, t0 + Duration::from_millis(30)));
    }

    #[test]
    fn buckets_refill_up_to_the_burst_only() {
        let limiter = TokenBuckets::new(50.0, 100.0);
        let key = Uuid::new_v4();
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(key, t0));
        let later = t0 + Duration::from_secs(3600);
        for _ in 0..100 {
            assert!(limiter.try_acquire_at(key, later));
        }
        assert!(
            !limiter.try_acquire_at(key, later),
            "an hour idle still allows only the burst"
        );
    }

    #[test]
    fn sources_have_independent_buckets() {
        let limiter = TokenBuckets::new(1.0, 1.0);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(a, t0));
        assert!(!limiter.try_acquire_at(a, t0));
        assert!(
            limiter.try_acquire_at(b, t0),
            "another source is unaffected"
        );
    }

    #[test]
    fn a_clock_going_backwards_never_adds_tokens() {
        let limiter = TokenBuckets::new(50.0, 1.0);
        let key = Uuid::new_v4();
        let t0 = Instant::now() + Duration::from_secs(10);
        assert!(limiter.try_acquire_at(key, t0));
        assert!(!limiter.try_acquire_at(key, t0 - Duration::from_secs(5)));
    }
    #[test]
    fn a_source_holds_at_most_its_maximum_and_gets_a_place_back_when_a_guard_drops() {
        let in_flight = InFlight::new(2);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = in_flight.try_enter(a).expect("first");
        let second = in_flight.try_enter(a).expect("second");
        assert!(
            in_flight.try_enter(a).is_none(),
            "a third of the same source"
        );
        assert_eq!(in_flight.count(a), 2);
        assert!(
            in_flight.try_enter(b).is_some(),
            "another source is unaffected"
        );

        drop(first);
        assert_eq!(in_flight.count(a), 1);
        let third = in_flight.try_enter(a).expect("a place came back");
        assert!(in_flight.try_enter(a).is_none());
        drop((second, third));
        assert_eq!(in_flight.count(a), 0);
    }

    #[test]
    fn sources_that_are_done_leave_nothing_behind() {
        let in_flight = InFlight::default();
        assert_eq!(in_flight.limit(), MAX_IN_FLIGHT_PER_SOURCE);
        let guards: Vec<_> = (0..100)
            .map(|_| in_flight.try_enter(Uuid::new_v4()).unwrap())
            .collect();
        assert_eq!(in_flight.counts.lock().unwrap().len(), 100);
        drop(guards);
        assert!(
            in_flight.counts.lock().unwrap().is_empty(),
            "the map does not grow with the sources seen"
        );
        // A limit of zero admits nobody and still leaves nothing behind.
        let closed = InFlight::new(0);
        assert!(closed.try_enter(Uuid::new_v4()).is_none());
        assert!(closed.counts.lock().unwrap().is_empty());
    }

    #[test]
    fn guards_released_on_other_threads_are_counted_exactly() {
        let in_flight = Arc::new(InFlight::new(1_000_000));
        let source = Uuid::new_v4();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let in_flight = in_flight.clone();
                std::thread::spawn(move || {
                    for _ in 0..2_000 {
                        let guard = in_flight.try_enter(source).unwrap();
                        drop(guard);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(in_flight.count(source), 0);
    }
}
