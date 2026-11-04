//! The agent hosts this binary supports, built from the adapter crates it links. Adding a host is
//! one more entry here and the adapter crate it comes from; nothing else in the binary changes.

use dibs_agent_host::neutral::Neutral;
use dibs_agent_host::{HostId, HostRegistry, SessionKey};
use dibs_client::host_process::HostProcesses;
use dibs_host_claude_code::ClaudeCode;
use time::OffsetDateTime;

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

    /// The session the host process `pid` runs now: the one its hooks reported last, when they did
    /// so at `since` or later (at any time without `since`), and this one otherwise. A host that
    /// starts a new session in the same process, as on `/clear`, leaves a tool server and a shell
    /// with the earlier one in their environment.
    #[must_use]
    pub fn following(
        self,
        processes: &HostProcesses,
        pid: Option<u32>,
        since: Option<OffsetDateTime>,
    ) -> Self {
        let Some(known) = pid.and_then(|pid| processes.session(self.host.as_str(), pid)) else {
            return self;
        };
        if since.is_some_and(|since| known.since < since) {
            return self;
        }
        match SessionKey::parse(&known.key) {
            Some(key) => Self { key, ..self },
            None => self,
        }
    }
}

/// A host session a long-running process acts as, followed to the one its host process runs now:
/// a tool server the host started for one session, which the host may replace by another in the
/// same process.
#[derive(Debug, Clone)]
pub struct FollowedSession {
    named: HostSession,
    process: Option<u32>,
    processes: Option<HostProcesses>,
    since: OffsetDateTime,
}

impl FollowedSession {
    /// The session `named`, followed through what the hooks of the host process `process` report,
    /// as written down in `processes`, from `since` on: what was written before belongs to an
    /// earlier process or session.
    pub const fn new(
        named: HostSession,
        process: Option<u32>,
        processes: Option<HostProcesses>,
        since: OffsetDateTime,
    ) -> Self {
        Self {
            named,
            process,
            processes,
            since,
        }
    }

    /// The session to act as now.
    pub fn current(&self) -> HostSession {
        match &self.processes {
            Some(processes) => {
                self.named
                    .clone()
                    .following(processes, self.process, Some(self.since))
            }
            None => self.named.clone(),
        }
    }
}

/// The process of `host` a process runs under: the one the environment, read through `var`, names
/// in the host's process variable, or else `parent`, for a tool server the host started directly.
pub fn host_process(
    host: HostId,
    registry: &HostRegistry,
    var: impl Fn(&str) -> Option<String>,
    parent: Option<u32>,
) -> Option<u32> {
    let named = registry
        .get(host.as_str())
        .and_then(|adapter| adapter.process_var())
        .and_then(var)
        .and_then(|value| value.trim().parse().ok());
    named.or(parent)
}

/// The id of this process's parent, where the platform says it.
pub fn parent_process() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(std::os::unix::process::parent_id())
    }
    #[cfg(not(unix))]
    {
        None
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

    #[test]
    fn a_session_follows_what_the_hooks_of_its_host_process_reported_since() {
        let dir = tempfile::tempdir().unwrap();
        let processes = HostProcesses::new(dir.path());
        let registry = registry();
        let started = OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000);
        let named = HostSession {
            host: dibs_agent_host::HostId::new("claude-code"),
            key: SessionKey::parse("first-session").unwrap(),
        };
        let pid = host_process(
            named.host,
            &registry,
            |name| (name == "CLAUDE_PID").then(|| "11724".to_owned()),
            Some(1),
        );
        assert_eq!(pid, Some(11724), "the host's variable before the parent");
        assert_eq!(
            host_process(named.host, &registry, |_: &str| None, Some(42)),
            Some(42),
            "the parent of a tool server the host started"
        );
        assert_eq!(
            named.clone().following(&processes, pid, Some(started)),
            named,
            "nothing reported: the session it was given"
        );
        processes
            .remember(
                "claude-code",
                11724,
                "first-session",
                started - time::Duration::minutes(5),
            )
            .unwrap();
        processes
            .remember(
                "claude-code",
                11724,
                "earlier-process",
                started - time::Duration::minutes(1),
            )
            .unwrap();
        assert_eq!(
            named.clone().following(&processes, pid, Some(started)),
            named,
            "a report from before the tool server started, perhaps of another process with the same id"
        );
        processes
            .remember(
                "claude-code",
                11724,
                "after-clear",
                started + time::Duration::minutes(1),
            )
            .unwrap();
        let followed = named.clone().following(&processes, pid, Some(started));
        assert_eq!(followed.key.as_str(), "after-clear");
        assert_eq!(followed.name(), "after-cl");
        assert_eq!(
            named.clone().following(&processes, pid, None).key.as_str(),
            "after-clear",
            "a shell follows whatever the process's hooks reported last"
        );
        assert_eq!(
            named.clone().following(&processes, None, None),
            named,
            "no process known"
        );
    }
}
