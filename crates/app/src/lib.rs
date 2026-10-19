//! Use cases and the ports they need.
//!
//! Each use case is a struct built from [`Dependencies`] with one `execute` method. It opens one
//! unit of work, loads what the domain decides on, decides through `dibs-domain`, writes the
//! result and appends one event per change in the same transaction, and wakes the project's
//! followers only after the commit. The ports — the store, the clock, the id and secret generators,
//! the hasher and the notifier — are traits of this crate; the adapters live elsewhere and are
//! injected by the composition roots.

pub mod actor;
pub mod dependencies;
pub mod error;
pub mod ports;
pub mod records;
pub mod token;
pub mod use_cases;
mod work;

#[cfg(any(test, feature = "fakes"))]
pub mod fakes;

pub use actor::Actor;
pub use dependencies::Dependencies;
pub use error::AppError;
