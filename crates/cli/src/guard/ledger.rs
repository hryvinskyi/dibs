//! What each agent session of this machine was shown, one file per session beside the project's
//! sessions, read and replaced only while that session's lock file is held. A hook that cannot
//! take the lock at once shows nothing, so two hooks of one session running together show a
//! message once.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_agent_host::ports::{DeliveryLedger, LedgerEntry, SessionLedger, SessionRef};
use dibs_client::state::{StateDir, read_json, write_json};

/// The ledgers of one project's sessions.
pub struct SessionLedgers {
    state: StateDir,
}

impl SessionLedgers {
    /// The ledgers kept in `state`.
    pub fn new(state: StateDir) -> Self {
        Self { state }
    }
}

/// A session's ledger as its file holds it.
#[derive(Default, Serialize, Deserialize)]
struct LedgerFile {
    #[serde(default, with = "time::serde::rfc3339::option")]
    last_injection: Option<OffsetDateTime>,
    #[serde(default)]
    shown: Vec<String>,
}

/// One session's ledger, its lock held for as long as this lives.
struct LockedLedger {
    file: PathBuf,
    _lock: File,
}

impl DeliveryLedger for SessionLedgers {
    fn open(&self, session: &SessionRef) -> Option<Box<dyn LedgerEntry + '_>> {
        let (file, lock) = self
            .state
            .delivery_files(session.host.as_str(), session.key.as_str())
            .map_err(|error| tracing::debug!(error = %error, "the ledger's directory"))
            .ok()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(lock)
            .map_err(|error| tracing::debug!(error = %error, "the ledger's lock file"))
            .ok()?;
        match lock.try_lock() {
            Ok(()) => Some(Box::new(LockedLedger { file, _lock: lock })),
            Err(TryLockError::WouldBlock) => None,
            Err(TryLockError::Error(error)) => {
                tracing::debug!(error = %error, "the ledger's lock");
                None
            }
        }
    }
}

impl LedgerEntry for LockedLedger {
    fn read(&self) -> SessionLedger {
        let file: LedgerFile = read_json(&self.file).unwrap_or_default();
        SessionLedger {
            last_injection: file.last_injection,
            shown: file.shown,
        }
    }

    fn write(&self, ledger: &SessionLedger) -> bool {
        let file = LedgerFile {
            last_injection: ledger.last_injection,
            shown: ledger.shown.clone(),
        };
        match write_json(&self.file, &file) {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(error = %error, "the ledger could not be written");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use dibs_agent_host::{HostId, SessionKey};
    use dibs_client::credentials::CredentialKey;
    use dibs_client::server_url::ServerUrl;
    use dibs_domain::project::ProjectSlug;

    use super::*;

    fn ledgers(dir: &std::path::Path) -> SessionLedgers {
        SessionLedgers::new(StateDir::new(
            dir,
            &CredentialKey {
                server: ServerUrl::parse("https://dibs.example.com").unwrap(),
                project: ProjectSlug::parse("acme").unwrap(),
            },
        ))
    }

    fn session(key: &str) -> SessionRef {
        SessionRef {
            host: HostId::new("neutral"),
            key: SessionKey::parse(key).unwrap(),
        }
    }

    #[test]
    fn a_ledger_is_written_in_one_step_and_read_back_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let ledgers = ledgers(dir.path());
        let entry = ledgers.open(&session("s-1")).unwrap();
        assert_eq!(entry.read(), SessionLedger::default());
        let shown = SessionLedger {
            last_injection: Some(OffsetDateTime::UNIX_EPOCH),
            shown: vec!["42".to_owned()],
        };
        assert!(entry.write(&shown));
        drop(entry);
        assert_eq!(ledgers.open(&session("s-1")).unwrap().read(), shown);
        assert_eq!(
            ledgers.open(&session("s-2")).unwrap().read(),
            SessionLedger::default()
        );
    }

    #[test]
    fn while_one_holds_a_sessions_ledger_another_gets_none() {
        let dir = tempfile::tempdir().unwrap();
        let ledgers = ledgers(dir.path());
        let held = ledgers.open(&session("s-1")).unwrap();
        assert!(ledgers.open(&session("s-1")).is_none());
        assert!(
            ledgers.open(&session("s-2")).is_some(),
            "another session's is free"
        );
        drop(held);
        assert!(ledgers.open(&session("s-1")).is_some());
    }
}
