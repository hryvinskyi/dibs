//! The ports: what the use cases need from the world outside, as traits this crate owns. Adapters
//! implement them and the composition roots inject them; no use case names an adapter.

mod store;

pub use store::{
    AckRows, ClaimRows, DeliveryRows, DependencyRows, EscalationRows, EventRows, GrantRows,
    ItemRows, MemberRows, ProjectRows, RadarRows, RaisedRows, SessionRows, SettingsRows, SlotRows,
    Store, StoreError, StoreResult, ThreadRows, TokenRows, Tx, UiSessionRows, WideAskRows,
};

use time::OffsetDateTime;

use dibs_domain::ids::{EventId, ProjectId};

use crate::token::{SecretToken, TokenHash};

/// The current time. Nothing in this crate reads the system clock.
pub trait Clock: Send + Sync {
    /// Now.
    fn now(&self) -> OffsetDateTime;
}

/// Ids for new records of every kind except events, whose ids the store assigns in log order.
///
/// Every id fits in 63 bits: it lies in `1..=i64::MAX`. A store may keep ids as signed 64-bit
/// integers and refuses a record whose id is outside that range.
pub trait IdGen: Send + Sync {
    /// A number in `1..=i64::MAX` that no record of any project has used, nor will be handed out
    /// again.
    fn next(&self) -> u64;
}

/// New secrets. Nothing in this crate reads a random source.
pub trait TokenGenerator: Send + Sync {
    /// A fresh secret of at least 256 random bits, in the shape [`SecretToken::parse`] accepts.
    fn generate(&self) -> SecretToken;
}

/// The hash secrets are stored and looked up by.
pub trait TokenHasher: Send + Sync {
    /// The hash of a secret; equal secrets give equal hashes.
    fn hash(&self, token: &SecretToken) -> TokenHash;
}

/// Wakes whoever follows a project's event log. Called only after a transaction that appended events
/// committed; a follower reads the events from the store by cursor, so a lost wake-up loses nothing.
pub trait EventNotifier: Send + Sync {
    /// The project's log grew; `last` is the newest event the transaction appended.
    fn notify(&self, project: ProjectId, last: EventId);
}
