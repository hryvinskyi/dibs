//! The host-neutral guard of dibs: what an agent host's hooks tell it, what it answers, and the
//! port each host is reached through.
//!
//! An agent host is a detail on the outside. Its adapter turns the host's hook input into a
//! neutral [`HostEvent`] and a neutral [`GuardOutcome`] into the host's reply; the [`Guard`]
//! decides between the two, the same way for every host, over ports the command line implements.
//! Nothing here reads a clock, a file or the network, and nothing here names a host other than
//! the [`neutral`] one, whose native format is the guard's own.

pub mod adapter;
pub mod capabilities;
#[cfg(any(test, feature = "conformance"))]
pub mod conformance;
pub mod delivery;
pub mod event;
pub mod guard;
pub mod neutral;
pub mod outcome;
pub mod ports;
pub mod push;
pub mod registry;
pub mod slots;
pub mod text;
pub mod timing;
pub mod upkeep;
pub mod wording;

pub use adapter::{HostAdapter, HostId, HostReply, OwnWorktrees, ParseError, Probe, WorktreeTerms};
pub use capabilities::Capabilities;
pub use event::{AgentKey, EventKind, HostEvent, SessionKey, Thread, ToolCall, WorktreeLabel};
pub use guard::{Guard, HookAnswer, HookRequest};
pub use outcome::GuardOutcome;
pub use registry::HostRegistry;
pub use text::Text;
