//! The agent hosts this binary supports, built from the adapter crates it links. Adding a host is
//! one more entry here and the adapter crate it comes from; nothing else in the binary changes.

use dibs_agent_host::neutral::Neutral;
use dibs_agent_host::{HostId, HostRegistry, SessionKey};
use dibs_host_claude_code::ClaudeCode;

use crate::guard::remote::short_name;

/// The registry of every linked host.
pub fn registry() -> HostRegistry {
    HostRegistry::new(vec![Box::new(Neutral), Box::new(ClaudeCode)])
}

/// The agent host session a process runs in, as the host names it in the process's environment:
/// the session its hooks report, so that what the process does is that session's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSession {
    /// The host.
    pub host: HostId,
    /// The host's id of the session.
    pub key: SessionKey,
}

impl HostSession {
    /// The name after the machine's in the session's label: the one its hooks register under.
    pub fn name(&self) -> String {
        short_name(&self.key)
    }

    /// The kind of agent the session reports: the host's id, as its hooks report it.
    pub const fn agent(&self) -> &'static str {
        self.host.as_str()
    }
}

/// The session the environment, read through `var`, names: of `host` when one is given, or else
/// of the first linked host whose variable is set. None when no such variable holds a valid id.
pub fn session_named(
    host: Option<&str>,
    registry: &HostRegistry,
    var: impl Fn(&str) -> Option<String>,
) -> Option<HostSession> {
    let named = |adapter: &dyn dibs_agent_host::HostAdapter| {
        let key = adapter
            .session_var()
            .and_then(&var)
            .and_then(|id| SessionKey::parse(&id))?;
        Some(HostSession {
            host: adapter.id(),
            key,
        })
    };
    match host {
        Some(host) => registry.get(host).and_then(named),
        None => registry.adapters().find_map(named),
    }
}

#[cfg(test)]
mod tests {
    use dibs_domain::session::AgentLabel;

    use super::*;

    #[test]
    fn every_linked_host_has_an_id_its_sessions_can_report() {
        let registry = registry();
        for adapter in registry.adapters() {
            assert!(AgentLabel::parse(adapter.id().as_str()).is_ok());
            assert!(registry.get(adapter.id().as_str()).is_some());
        }
        assert!(registry.get("neutral").is_some());
        assert!(registry.get("claude-code").is_some());
    }

    #[test]
    fn a_process_is_the_host_session_its_environment_names() {
        let registry = registry();
        let named = |name: &str| {
            (name == "CLAUDE_CODE_SESSION_ID").then(|| "33738ac7-61b0-44ca-946f".to_owned())
        };
        let session = session_named(None, &registry, named).unwrap();
        assert_eq!(session.host.as_str(), "claude-code");
        assert_eq!(
            session.name(),
            "33738ac7",
            "the name its hooks register under"
        );
        assert_eq!(session.agent(), "claude-code");
        assert_eq!(
            session_named(Some("claude-code"), &registry, named),
            Some(session)
        );
        assert_eq!(
            session_named(Some("neutral"), &registry, named),
            None,
            "a host that names no session variable"
        );
        assert_eq!(session_named(None, &registry, |_: &str| None), None);
        assert_eq!(
            session_named(None, &registry, |_: &str| Some(String::new())),
            None,
            "an id that is no valid key"
        );
    }
}
