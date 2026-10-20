//! Waking a session that waits for its person. A host whose tool server can put a delivery in
//! front of an idle agent declares here how it does so; the tool server itself is the same for
//! every host, and follows the project, claims each delivery from the server and sends it.
//!
//! What a host declares: the experimental capabilities its client looks for, what the tool
//! server's instructions add, the newest protocol revision on which the host still carries a push,
//! the environment variable through which it names the session it started the tool server for,
//! and the notification that carries one delivery. Nothing here reads a clock, a file or the
//! network.

use serde_json::{Map, Value};

use crate::ports::Pending;

/// A notification for the host's tool-server client: its method and its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct PushNotification {
    /// The method.
    pub method: String,
    /// The parameters.
    pub params: Value,
}

/// What a host adds to the tool server so that a delivery reaches a session waiting for its
/// person.
pub trait McpExtension: Send + Sync {
    /// The experimental capabilities the tool server declares, each name with its settings.
    fn experimental(&self) -> Vec<(&'static str, Map<String, Value>)>;

    /// What the tool server's instructions add: what a pushed delivery looks like to the agent
    /// and what it may do about one.
    fn instructions(&self) -> &'static str;

    /// The newest protocol revision, as dated by the protocol (`2025-11-25`), on which the host
    /// carries a push; `None` when it carries one on every revision.
    fn newest_protocol(&self) -> Option<&'static str>;

    /// The environment variable through which the host gives the tool server its id of the
    /// session it serves, the same id its hooks are given; `None` when it gives none.
    fn session_var(&self) -> Option<&'static str>;

    /// The notification that puts `pending` in front of the agent.
    fn notification(&self, pending: &Pending) -> PushNotification;

    /// The line the brief adds at the start of a session that opted into pushes: what the host
    /// needs for them to arrive, and that without it they show late.
    fn opt_in_notice(&self) -> &'static str;
}
