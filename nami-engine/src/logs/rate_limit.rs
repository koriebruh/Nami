//! Per-source request rate limit for HTTP ingestion: an in-memory token
//! bucket per source id (50 requests/s, burst 100), local to each engine
//! instance. Only authenticated sources get a bucket, so the map is bounded
//! by the number of sources.

use std::{collections::HashMap, sync::Mutex, time::Instant};

use uuid::Uuid;

pub const REQUESTS_PER_SECOND: f64 = 50.0;
pub const BURST: f64 = 100.0;

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
}
