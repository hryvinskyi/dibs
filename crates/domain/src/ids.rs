//! Identifiers of the stored records. Every id is a number handed out by the id generator port; the
//! domain never makes one up.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// The number behind the id.
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_type!(
    /// A project: the namespace every other record belongs to.
    ProjectId
);
id_type!(
    /// A member of a project.
    MemberId
);
id_type!(
    /// One running agent process (or browser or MCP server) of a member.
    SessionId
);
id_type!(
    /// A work item, addressed by this local id in every URL.
    ItemId
);
id_type!(
    /// A conversation.
    ThreadId
);
id_type!(
    /// One message of a conversation.
    MessageId
);
id_type!(
    /// A position in the project's append-only event log; later events have larger ids.
    EventId
);
id_type!(
    /// A stored grant.
    GrantId
);
id_type!(
    /// One run of a command under a slot hold: what that run's heartbeats and its release name.
    RunId
);
id_type!(
    /// A member's place in a slot's queue; places taken at the same moment are ordered by it.
    WaiterId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_order_and_print_as_their_number() {
        assert!(EventId(2) > EventId(1));
        assert_eq!(ItemId(42).to_string(), "42");
        assert_eq!(MemberId(7).get(), 7);
    }
}
