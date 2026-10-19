//! In-memory implementations of every port, for tests of this crate and of the crates that drive its
//! use cases. They behave as the real adapters promise to: the store runs one transaction at a time,
//! keeps nothing of one that did not commit and refuses to commit records that refer to missing
//! ones, ids are never reused and stay within the range the id port allows, equal secrets hash
//! equally.

mod store;

pub use store::MemoryStore;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use time::{Duration, OffsetDateTime};

use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{EventId, ProjectId};
use dibs_domain::ref_scheme::RefSchemeRegistry;

use crate::dependencies::Dependencies;
use crate::ports::{Clock, EventNotifier, IdGen, TokenGenerator, TokenHasher};
use crate::token::{SecretToken, TokenHash};

/// A clock that stands still until moved.
#[derive(Debug)]
pub struct FixedClock {
    now: Mutex<OffsetDateTime>,
}

impl FixedClock {
    /// A clock showing `at`.
    pub fn new(at: OffsetDateTime) -> Self {
        Self {
            now: Mutex::new(at),
        }
    }

    /// Sets the time.
    pub fn set(&self, at: OffsetDateTime) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = at;
    }

    /// Moves the time forward.
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now += by;
    }
}

impl Clock for FixedClock {
    fn now(&self) -> OffsetDateTime {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Ids 1, 2, 3, … in the order asked: never 0, and far below `i64::MAX` for any test's length.
#[derive(Debug, Default)]
pub struct SequentialIds {
    last: AtomicU64,
}

impl IdGen for SequentialIds {
    fn next(&self) -> u64 {
        self.last.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Distinct, predictable secrets: the n-th one carries n in its first random bits, so each has its
/// own prefix.
#[derive(Debug, Default)]
pub struct CountingTokens {
    issued: AtomicU64,
}

impl TokenGenerator for CountingTokens {
    fn generate(&self) -> SecretToken {
        let n = self.issued.fetch_add(1, Ordering::SeqCst) + 1;
        let mut bits = [0x5a; 32];
        bits[..8].copy_from_slice(&n.to_le_bytes());
        SecretToken::from_random(&bits)
    }
}

/// A hasher whose hash is the secret written backwards behind a marker: equal secrets hash equally,
/// and no hash equals its secret.
#[derive(Debug, Default)]
pub struct ReversingHasher;

impl TokenHasher for ReversingHasher {
    fn hash(&self, token: &SecretToken) -> TokenHash {
        TokenHash::new(format!(
            "hash:{}",
            token.expose().chars().rev().collect::<String>()
        ))
    }
}

/// A notifier that remembers every call.
#[derive(Debug, Default)]
pub struct RecordingNotifier {
    calls: Mutex<Vec<(ProjectId, EventId)>>,
}

impl RecordingNotifier {
    /// Every call so far, oldest first.
    pub fn calls(&self) -> Vec<(ProjectId, EventId)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl EventNotifier for RecordingNotifier {
    fn notify(&self, project: ProjectId, last: EventId) {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((project, last));
    }
}

/// One fake of every port, kept so a test can move the clock, read the store and see the
/// notifications after driving the use cases built from [`Fakes::dependencies`].
#[derive(Clone)]
pub struct Fakes {
    /// The store.
    pub store: Arc<MemoryStore>,
    /// The clock.
    pub clock: Arc<FixedClock>,
    /// The id generator.
    pub ids: Arc<SequentialIds>,
    /// The secret generator.
    pub tokens: Arc<CountingTokens>,
    /// The hasher.
    pub hasher: Arc<ReversingHasher>,
    /// The notifier.
    pub notifier: Arc<RecordingNotifier>,
}

impl Fakes {
    /// Fresh fakes with the clock at `at`.
    pub fn new(at: OffsetDateTime) -> Self {
        Self {
            store: Arc::new(MemoryStore::default()),
            clock: Arc::new(FixedClock::new(at)),
            ids: Arc::new(SequentialIds::default()),
            tokens: Arc::new(CountingTokens::default()),
            hasher: Arc::new(ReversingHasher),
            notifier: Arc::new(RecordingNotifier::default()),
        }
    }

    /// The fakes as the use cases take them, with the standard hot-rule kinds and reference
    /// schemes.
    pub fn dependencies(&self) -> Dependencies {
        Dependencies {
            store: self.store.clone(),
            clock: self.clock.clone(),
            ids: self.ids.clone(),
            tokens: self.tokens.clone(),
            hasher: self.hasher.clone(),
            notifier: self.notifier.clone(),
            hot_rules: Arc::new(HotRuleRegistry::standard()),
            ref_schemes: Arc::new(RefSchemeRegistry::standard()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_secrets_are_never_repeated() {
        let ids = SequentialIds::default();
        assert_eq!((ids.next(), ids.next()), (1, 2));
        let tokens = CountingTokens::default();
        let (a, b) = (tokens.generate(), tokens.generate());
        assert_ne!(a.expose(), b.expose());
        assert_ne!(a.prefix(), b.prefix());
        let hasher = ReversingHasher;
        assert_eq!(
            hasher.hash(&a),
            hasher.hash(&SecretToken::parse(a.expose()).unwrap())
        );
        assert_ne!(hasher.hash(&a).as_str(), a.expose());
    }

    #[test]
    fn the_clock_moves_only_when_told() {
        let clock = FixedClock::new(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
        clock.advance(Duration::minutes(5));
        assert_eq!(
            clock.now(),
            OffsetDateTime::UNIX_EPOCH + Duration::minutes(5)
        );
        clock.set(OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.now(), OffsetDateTime::UNIX_EPOCH);
    }
}
