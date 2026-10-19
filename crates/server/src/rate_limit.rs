//! The limit on failed sign-ins per client address: a token bucket per address, of a fixed capacity
//! refilled at an even pace. Only failures spend from it; while an address's bucket is empty, every
//! attempt from it is turned away before its token is looked at, a good one included.
//!
//! The map of buckets is bounded: at most `max_addresses` entries, the least recently used one
//! evicted to make room, and every entry idle for `idle_after` dropped. A dropped bucket is a full
//! one, so eviction forgives at most what time would have forgiven anyway.

use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use lru::LruCache;

/// The limiter's numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitPolicy {
    /// Failures an address may have in a row before it is turned away.
    pub capacity: u32,
    /// The time in which a whole bucket refills.
    pub refill: Duration,
    /// The most addresses kept.
    pub max_addresses: usize,
    /// How long an address may go unseen before its bucket is dropped.
    pub idle_after: Duration,
}

impl Default for LimitPolicy {
    /// Ten failures a minute, for at most ten thousand addresses, each forgotten after ten idle
    /// minutes.
    fn default() -> Self {
        Self {
            capacity: 10,
            refill: Duration::from_secs(60),
            max_addresses: 10_000,
            idle_after: Duration::from_secs(600),
        }
    }
}

/// One address's bucket, kept as the time it will be full again: each failure pushes that time one
/// refill interval further, and the bucket is empty while it lies more than a whole bucket's refill
/// minus one interval ahead.
#[derive(Debug, Clone, Copy)]
struct Bucket {
    full_at: Instant,
    seen: Instant,
}

/// The failed sign-ins of every address.
#[derive(Debug)]
pub struct AuthLimiter {
    policy: LimitPolicy,
    interval: Duration,
    buckets: Mutex<LruCache<IpAddr, Bucket>>,
}

impl AuthLimiter {
    /// A limiter with no failures recorded.
    pub fn new(policy: LimitPolicy) -> Self {
        let interval = policy.refill / policy.capacity.max(1);
        let size = NonZeroUsize::new(policy.max_addresses).unwrap_or(NonZeroUsize::MIN);
        Self {
            policy,
            interval,
            buckets: Mutex::new(LruCache::new(size)),
        }
    }

    /// Whether an attempt from `address` may be checked at `now`.
    ///
    /// # Errors
    /// The time to wait before the next attempt, when the address's bucket is empty.
    pub fn admit(&self, address: IpAddr, now: Instant) -> Result<(), Duration> {
        let mut buckets = self.lock();
        self.forget_idle(&mut buckets, now);
        let Some(bucket) = buckets.peek(&address) else {
            return Ok(());
        };
        let ahead = bucket.full_at.saturating_duration_since(now);
        let allowed = self.policy.refill.saturating_sub(self.interval);
        if ahead > allowed {
            Err(ahead - allowed)
        } else {
            Ok(())
        }
    }

    /// Records a failed attempt from `address` at `now`.
    pub fn record_failure(&self, address: IpAddr, now: Instant) {
        let mut buckets = self.lock();
        self.forget_idle(&mut buckets, now);
        let start = buckets
            .peek(&address)
            .map_or(now, |bucket| bucket.full_at.max(now));
        let full_at = start.checked_add(self.interval).unwrap_or(start);
        buckets.put(address, Bucket { full_at, seen: now });
    }

    /// How many addresses the limiter keeps.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// Whether the limiter keeps no address.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, LruCache<IpAddr, Bucket>> {
        self.buckets.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn forget_idle(&self, buckets: &mut LruCache<IpAddr, Bucket>, now: Instant) {
        while let Some((_, bucket)) = buckets.peek_lru() {
            if now.saturating_duration_since(bucket.seen) < self.policy.idle_after {
                break;
            }
            buckets.pop_lru();
        }
    }
}

/// Whole seconds to tell a client to wait, rounded up and at least one.
pub fn retry_after_secs(wait: Duration) -> u64 {
    let seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    seconds.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(n: u32) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::from_bits(0x0a00_0000 + n))
    }

    #[test]
    fn ten_failures_are_allowed_and_the_eleventh_attempt_waits() {
        let limiter = AuthLimiter::new(LimitPolicy::default());
        let t0 = Instant::now();
        let a = address(1);
        for _ in 0..10 {
            assert_eq!(limiter.admit(a, t0), Ok(()));
            limiter.record_failure(a, t0);
        }
        let wait = limiter.admit(a, t0).unwrap_err();
        assert_eq!(wait, Duration::from_secs(6));
        assert_eq!(retry_after_secs(wait), 6);
        assert_eq!(limiter.admit(address(2), t0), Ok(()));
        assert_eq!(limiter.admit(a, t0 + Duration::from_secs(6)), Ok(()));
    }

    #[test]
    fn the_bucket_refills_at_an_even_pace() {
        let limiter = AuthLimiter::new(LimitPolicy::default());
        let t0 = Instant::now();
        let a = address(1);
        for _ in 0..10 {
            limiter.record_failure(a, t0);
        }
        let later = t0 + Duration::from_secs(30);
        for _ in 0..5 {
            assert_eq!(limiter.admit(a, later), Ok(()));
            limiter.record_failure(a, later);
        }
        assert!(limiter.admit(a, later).is_err());
    }

    #[test]
    fn the_map_stays_within_its_bound() {
        let policy = LimitPolicy {
            max_addresses: 100,
            ..LimitPolicy::default()
        };
        let limiter = AuthLimiter::new(policy);
        let t0 = Instant::now();
        for n in 0..5_000 {
            limiter.record_failure(address(n), t0);
            assert!(limiter.len() <= 100);
        }
        assert_eq!(limiter.len(), 100);
    }

    #[test]
    fn idle_addresses_are_forgotten() {
        let limiter = AuthLimiter::new(LimitPolicy::default());
        let t0 = Instant::now();
        for n in 0..50 {
            limiter.record_failure(address(n), t0);
        }
        limiter.record_failure(address(999), t0 + Duration::from_secs(300));
        assert_eq!(limiter.len(), 51);
        assert_eq!(
            limiter.admit(address(1), t0 + Duration::from_secs(600)),
            Ok(())
        );
        assert_eq!(limiter.len(), 1);
        assert!(!limiter.is_empty());
    }

    #[test]
    fn retry_after_rounds_up() {
        assert_eq!(retry_after_secs(Duration::from_millis(1)), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(5_001)), 6);
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
    }
}
