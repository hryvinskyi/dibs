//! Which session each agent host process on this machine runs now, as its hooks last saw it.
//!
//! A host can start a fresh session inside the process it already runs, as a `/clear` does: its
//! hooks report the new session, while a tool server it started at the beginning keeps the
//! session it was started for. The hooks write down, per host process, the session they last
//! reported, so that a tool server or a shell command of the same process can act as the session
//! that runs now. One small file per process, named by the host and the process id, written only
//! when the session changes.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::state::{read_json, write_json};

/// The session one host process runs, and since when this machine knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostProcessSession {
    /// The host's id of the session.
    pub key: String,
    /// When the hooks first reported it in this process.
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// The sessions of this machine's host processes, kept under the state directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostProcesses {
    dir: PathBuf,
}

impl HostProcesses {
    /// The records kept under the state directory `state_dir`.
    pub fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("host-processes"),
        }
    }

    /// The session the process `pid` of `host` runs, as its hooks last reported it.
    pub fn session(&self, host: &str, pid: u32) -> Option<HostProcessSession> {
        read_json(&self.file(host, pid))
    }

    /// Writes down that the process `pid` of `host` runs the session `key` as of `at`, unless that
    /// is already what is written; answers whether it wrote.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember(
        &self,
        host: &str,
        pid: u32,
        key: &str,
        at: OffsetDateTime,
    ) -> io::Result<bool> {
        if self
            .session(host, pid)
            .is_some_and(|known| known.key == key)
        {
            return Ok(false);
        }
        write_json(
            &self.file(host, pid),
            &HostProcessSession {
                key: key.to_owned(),
                since: at,
            },
        )?;
        Ok(true)
    }

    fn file(&self, host: &str, pid: u32) -> PathBuf {
        let host: String = host
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(format!("{host}-{pid}.json"))
    }
}

#[cfg(test)]
mod tests {
    use time::Duration;

    use super::*;

    #[test]
    fn a_host_process_keeps_the_session_its_hooks_last_reported_and_since_when() {
        let dir = tempfile::tempdir().unwrap();
        let processes = HostProcesses::new(dir.path());
        let at = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        assert_eq!(processes.session("some-host", 11724), None);
        assert!(processes.remember("some-host", 11724, "first", at).unwrap());
        assert!(
            !processes
                .remember("some-host", 11724, "first", at + Duration::hours(1))
                .unwrap(),
            "the same session is not written again"
        );
        assert_eq!(
            processes.session("some-host", 11724),
            Some(HostProcessSession {
                key: "first".to_owned(),
                since: at
            })
        );
        let cleared = at + Duration::hours(2);
        assert!(
            processes
                .remember("some-host", 11724, "second", cleared)
                .unwrap()
        );
        assert_eq!(
            processes
                .session("some-host", 11724)
                .map(|s| (s.key, s.since)),
            Some(("second".to_owned(), cleared))
        );
        assert_eq!(processes.session("some-host", 1), None, "another process");
        assert_eq!(processes.session("other/host", 11724), None, "another host");
    }
}
