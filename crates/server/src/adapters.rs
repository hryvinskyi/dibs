//! The production adapters of the ports that are not the store: the system clock, ids, secrets from
//! the operating system's random source, SHA-256 hashes, and the broadcast that wakes the followers
//! of a project's event log.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rand::Rng;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::sync::broadcast;

use dibs_app::ports::{Clock, EventNotifier, IdGen, TokenGenerator, TokenHasher};
use dibs_app::token::{SecretToken, TokenHash};
use dibs_domain::ids::{EventId, ProjectId};

use crate::fields::MAX_RECORD_ID;

/// The system's clock, in UTC.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// Ids taken from the microseconds since 1970, never handed out twice by one process: each id is
/// the current microsecond or one more than the last id, whichever is larger.
///
/// A new process starts at the current microsecond, past every id an earlier one handed out, as
/// long as the system clock was not set back and no earlier process drew ids faster than one per
/// microsecond until the moment it stopped. Every id stays below 2^53, so a JSON reader that keeps
/// numbers as doubles reads it exactly, until the year 2255; it stays within `1..=i64::MAX` for far
/// longer.
#[derive(Debug, Default)]
pub struct TimeIds {
    last: AtomicU64,
}

impl TimeIds {
    fn now_micros() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| {
                u64::try_from(since.as_micros()).unwrap_or(u64::MAX)
            })
    }
}

impl IdGen for TimeIds {
    fn next(&self) -> u64 {
        let now = Self::now_micros().clamp(1, MAX_RECORD_ID);
        let mut last = self.last.load(Ordering::SeqCst);
        loop {
            let id = now.max(last.saturating_add(1)).min(MAX_RECORD_ID);
            match self
                .last
                .compare_exchange(last, id, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return id,
                Err(seen) => last = seen,
            }
        }
    }
}

/// Secrets of 256 bits from the thread-local generator, which the operating system's random source
/// seeds and reseeds.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsTokens;

impl TokenGenerator for OsTokens {
    fn generate(&self) -> SecretToken {
        let mut bits = [0u8; 32];
        rand::rng().fill_bytes(&mut bits);
        SecretToken::from_random(&bits)
    }
}

/// SHA-256 of the secret's text, in lower-case hex. Secrets carry 256 random bits, so a fast hash
/// is the right one: there is nothing to guess.
#[derive(Debug, Default, Clone, Copy)]
pub struct Sha256Hasher;

impl TokenHasher for Sha256Hasher {
    fn hash(&self, token: &SecretToken) -> TokenHash {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = Sha256::digest(token.expose().as_bytes());
        let mut text = String::with_capacity(64);
        for byte in digest.iter() {
            text.push(char::from(HEX[usize::from(byte >> 4)]));
            text.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        TokenHash::new(text)
    }
}

/// A project's log grew up to `last`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wakeup {
    /// The project.
    pub project: ProjectId,
    /// Its newest event.
    pub last: EventId,
}

/// Wakes every subscriber after a commit that appended events. A subscriber that falls behind
/// misses wake-ups, not events: it reads the events from the store by cursor.
#[derive(Debug, Clone)]
pub struct BroadcastNotifier {
    sender: broadcast::Sender<Wakeup>,
}

impl BroadcastNotifier {
    /// A notifier that holds up to `capacity` wake-ups for a slow subscriber.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self { sender }
    }

    /// A new subscriber, woken by every notification from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<Wakeup> {
        self.sender.subscribe()
    }
}

impl EventNotifier for BroadcastNotifier {
    fn notify(&self, project: ProjectId, last: EventId) {
        // Nobody listening is the normal case between subscribers; the wake-up is then not needed.
        let _ = self.sender.send(Wakeup { project, last });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn ids_grow_and_fit_in_63_bits() {
        let ids = TimeIds::default();
        let mut previous = 0;
        for _ in 0..10_000 {
            let id = ids.next();
            assert!(id > previous);
            assert!((1..=MAX_RECORD_ID).contains(&id));
            assert!(id < 1 << 53);
            previous = id;
        }
    }

    #[test]
    fn ids_are_unique_across_threads() {
        let ids = Arc::new(TimeIds::default());
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let ids = ids.clone();
                std::thread::spawn(move || (0..1_000).map(|_| ids.next()).collect::<Vec<_>>())
            })
            .collect();
        let mut seen = HashSet::new();
        for handle in handles {
            for id in handle.join().unwrap() {
                assert!(seen.insert(id));
            }
        }
        assert_eq!(seen.len(), 8_000);
    }

    #[test]
    fn secrets_have_the_marker_and_differ() {
        let tokens = OsTokens;
        let (a, b) = (tokens.generate(), tokens.generate());
        assert!(a.expose().starts_with("dibs_"));
        assert_eq!(a.expose().len(), 5 + 43);
        assert_ne!(a, b);
        assert!(SecretToken::parse(a.expose()).is_ok());
    }

    #[test]
    fn the_hash_is_sha256_hex() {
        let token = SecretToken::from_random(&[0; 32]);
        let hash = Sha256Hasher.hash(&token);
        assert_eq!(hash.as_str().len(), 64);
        assert_eq!(hash, Sha256Hasher.hash(&token));
        assert_ne!(hash, Sha256Hasher.hash(&SecretToken::from_random(&[1; 32])));
        let expected: String = Sha256::digest(token.expose().as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hash.as_str(), expected);
    }

    #[tokio::test]
    async fn a_subscriber_is_woken() {
        let notifier = BroadcastNotifier::new(16);
        let mut subscriber = notifier.subscribe();
        notifier.notify(ProjectId(1), EventId(7));
        assert_eq!(
            subscriber.recv().await.unwrap(),
            Wakeup {
                project: ProjectId(1),
                last: EventId(7)
            }
        );
        drop(subscriber);
        notifier.notify(ProjectId(1), EventId(8));
    }
}
