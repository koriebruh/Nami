//! Per-source and per-project limits for HTTP ingestion, in memory and local
//! to each engine instance: a token bucket per source id (50 requests/s, burst
//! 100), a cap on how many requests of one source (4) and of one project (8)
//! may be in flight, and a cap on how many bodies of one source (2) and of one
//! project (2) are decoded at once. The project caps keep a project with many
//! sources from taking the whole endpoint. Only authenticated sources get an
//! entry, and an entry is dropped when its source (or project) has nothing in
//! flight, so the maps do not grow with the sources seen.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
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
/// How many bodies of one source may be decoded at once. The engine decodes
/// only a few bodies at a time (`routes::logs_ingest::MAX_DECODES_IN_FLIGHT`),
/// which is what bounds the memory they take; without this, the four requests
/// one source may have in flight could hold every one of those places, and
/// leave the other sources waiting behind them.
pub const MAX_DECODES_PER_SOURCE: usize = 2;

/// How many requests of one project may be in flight at once, whatever
/// number of sources it has: the endpoint has
/// `routes::logs_ingest::MAX_INGEST_IN_FLIGHT` places, so without this, eight
/// sources of one project (four requests each) could take every place and
/// leave every other project with 503.
pub const MAX_IN_FLIGHT_PER_PROJECT: usize = 8;
/// How many bodies of one project may be decoded at once: half of the
/// engine's `routes::logs_ingest::MAX_DECODES_IN_FLIGHT`, however many sources
/// the project has.
pub const MAX_DECODES_PER_PROJECT: usize = 2;

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
    /// How many buckets are kept now.
    pub fn len(&self) -> usize {
        self.buckets
            .lock()
            .expect("rate limiter lock poisoned")
            .len()
    }

    /// Whether no bucket is kept.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops every bucket that has refilled to `burst` by `now`. Such a bucket
    /// behaves exactly like a missing one (a new bucket starts full), so this
    /// changes no answer; it only bounds the map for callers whose keys are
    /// many and short-lived.
    pub fn prune_full(&self, now: Instant) {
        let mut buckets = self.buckets.lock().expect("rate limiter lock poisoned");
        buckets.retain(|_, bucket| {
            let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
            bucket.tokens + elapsed * self.rate < self.burst
        });
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

/// The limits of [`MAX_IN_FLIGHT_PER_PROJECT`] and
/// [`MAX_DECODES_PER_PROJECT`], keyed by project id.
#[derive(Debug)]
pub struct ProjectLimits {
    pub in_flight: InFlight,
    pub decode_shares: DecodeShares,
}

impl Default for ProjectLimits {
    fn default() -> Self {
        ProjectLimits {
            in_flight: InFlight::new(MAX_IN_FLIGHT_PER_PROJECT),
            decode_shares: DecodeShares::new(MAX_DECODES_PER_PROJECT),
        }
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

/// Places for decoding bodies, a few per source: a source whose bodies are
/// being decoded waits at a gate of its own, so it cannot fill the engine's
/// decode places (and the queue for them) with its own requests.
#[derive(Debug)]
pub struct DecodeShares {
    max: usize,
    gates: Arc<Mutex<HashMap<Uuid, Gate>>>,
}

/// One source's gate, and how many requests use it (hold a place or wait for
/// one): it is removed when the last of them is done.
#[derive(Debug)]
struct Gate {
    places: Arc<Semaphore>,
    users: usize,
}

impl Default for DecodeShares {
    fn default() -> Self {
        DecodeShares::new(MAX_DECODES_PER_SOURCE)
    }
}

impl DecodeShares {
    pub fn new(max: usize) -> DecodeShares {
        DecodeShares {
            max,
            gates: Arc::default(),
        }
    }

    /// The most bodies of one source decoded at once.
    pub fn limit(&self) -> usize {
        self.max
    }

    /// A place for decoding a body of `source`; waits while the source already
    /// has [`Self::limit`] bodies being decoded. The request counts as a user
    /// of the source's gate from the moment this is called, also while it
    /// waits, and until the returned share is dropped (or this future is).
    pub async fn acquire(&self, source: Uuid) -> DecodeShare {
        let (places, user) = {
            let mut gates = lock(&self.gates);
            let gate = gates.entry(source).or_insert_with(|| Gate {
                places: Arc::new(Semaphore::new(self.max)),
                users: 0,
            });
            gate.users += 1;
            (
                gate.places.clone(),
                GateUser {
                    source,
                    gates: self.gates.clone(),
                },
            )
        };
        let place = places
            .acquire_owned()
            .await
            .expect("the semaphore of a gate is never closed");
        DecodeShare {
            _place: place,
            _user: user,
        }
    }

    /// How many requests of `source` are decoding a body or waiting to.
    pub fn count(&self, source: Uuid) -> usize {
        lock(&self.gates).get(&source).map_or(0, |gate| gate.users)
    }

    /// How many sources have a gate now.
    pub fn tracked(&self) -> usize {
        lock(&self.gates).len()
    }
}

/// A place for decoding one body of a source; given back when dropped.
#[derive(Debug)]
pub struct DecodeShare {
    // The place goes back before the gate can be removed.
    _place: OwnedSemaphorePermit,
    _user: GateUser,
}

#[derive(Debug)]
struct GateUser {
    source: Uuid,
    gates: Arc<Mutex<HashMap<Uuid, Gate>>>,
}

impl Drop for GateUser {
    fn drop(&mut self) {
        let mut gates = lock(&self.gates);
        if let Some(gate) = gates.get_mut(&self.source) {
            gate.users = gate.users.saturating_sub(1);
            if gate.users == 0 {
                gates.remove(&self.source);
            }
        }
    }
}

/// The counts stay valid whatever a panicking holder left behind: every
/// update is a single step.
fn lock<T>(counts: &Mutex<T>) -> MutexGuard<'_, T> {
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
    fn pruning_drops_only_full_buckets() {
        let limiter = TokenBuckets::new(1.0, 2.0);
        let (drained, refilled) = (Uuid::new_v4(), Uuid::new_v4());
        let t0 = Instant::now();
        assert!(limiter.try_acquire_at(refilled, t0));
        let t1 = t0 + Duration::from_secs(10);
        assert!(limiter.try_acquire_at(drained, t1));
        assert!(limiter.try_acquire_at(drained, t1));
        assert_eq!(limiter.len(), 2);

        // At t1 the first key has refilled to the burst, the second is empty.
        limiter.prune_full(t1);
        assert_eq!(limiter.len(), 1, "only the drained bucket survives");
        assert!(!limiter.try_acquire_at(drained, t1), "still drained");

        // A removed key behaves like a fresh, full bucket.
        assert!(limiter.try_acquire_at(refilled, t1));
        assert!(limiter.try_acquire_at(refilled, t1));
        assert!(!limiter.try_acquire_at(refilled, t1));

        // Once everything has refilled, nothing is kept.
        limiter.prune_full(t1 + Duration::from_secs(60));
        assert!(limiter.is_empty());
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

    #[tokio::test]
    async fn a_source_decodes_at_most_its_share_at_once_and_the_rest_wait() {
        let shares = DecodeShares::new(2);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let first = shares.acquire(a).await;
        let second = shares.acquire(a).await;
        assert_eq!(shares.count(a), 2);

        // A third of the same source waits; another source does not.
        let third = tokio::time::timeout(Duration::from_millis(100), shares.acquire(a)).await;
        assert!(third.is_err(), "a has both its places");
        let other = tokio::time::timeout(Duration::from_millis(100), shares.acquire(b)).await;
        assert!(other.is_ok(), "b is not held back by a");
        assert_eq!(shares.count(a), 2, "the waiter that gave up is not counted");

        let waiting = tokio::spawn({
            let shares = Arc::new(shares);
            async move {
                let share = shares.acquire(a).await;
                (shares.count(a), share)
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiting.is_finished());
        drop(first);
        let (count, _share) = waiting.await.unwrap();
        assert_eq!(count, 2, "the waiter took the place that came free");
        drop(second);
    }

    #[tokio::test]
    async fn gates_are_removed_when_their_source_is_done_waiting_ones_included() {
        let shares = DecodeShares::new(1);
        let sources: Vec<Uuid> = (0..50).map(|_| Uuid::new_v4()).collect();
        let mut held = Vec::new();
        for source in &sources {
            held.push(shares.acquire(*source).await);
        }
        assert_eq!(shares.tracked(), 50);
        // A waiter that is dropped before it is served leaves nothing behind.
        let impatient = tokio::time::timeout(Duration::from_millis(50), shares.acquire(sources[0]));
        assert!(impatient.await.is_err());
        assert_eq!(shares.count(sources[0]), 1);
        drop(held);
        assert_eq!(shares.tracked(), 0);
        assert_eq!(shares.count(sources[0]), 0);
    }
}
