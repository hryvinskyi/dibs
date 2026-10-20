//! What each agent session of this machine was shown, one file per session beside the project's
//! sessions, read and replaced only while that session's lock file is held. A hook that cannot
//! take the lock at once shows nothing, so two hooks of one session running together show a
//! message once.
//!
//! Beside it, two stamps per session say when it last worked and when it last began to wait for
//! its person. Each is one small file replaced in one step, with no lock and no sync to the disk,
//! so a hook never waits for one; a stamp that is missing or torn is simply not known.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use dibs_agent_host::ports::{
    Activity, DeliveryLedger, LedgerEntry, PushedDelivery, SessionActivity, SessionLedger,
    SessionRef, Wake,
};
use dibs_client::api::PendingOffer;
use dibs_client::state::{RememberedMessage, StateDir, read_json, write_json};
use dibs_domain::handle::Handle;

use crate::guard::views;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pushed: Vec<PushedFile>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    wakes: Vec<WakeFile>,
}

/// A push waiting to be seen, as the ledger's file holds it.
#[derive(Serialize, Deserialize)]
struct PushedFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<RememberedMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    offer: Option<PendingOffer>,
    written: bool,
    #[serde(with = "time::serde::rfc3339")]
    due: OffsetDateTime,
}

/// A wake, as the ledger's file holds it.
#[derive(Serialize, Deserialize)]
struct WakeFile {
    sender: Handle,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
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

    fn stamp(&self, session: &SessionRef, activity: Activity, at: OffsetDateTime) {
        let Ok((working, waiting)) = self
            .state
            .activity_files(session.host.as_str(), session.key.as_str())
        else {
            return;
        };
        let file = match activity {
            Activity::Working => working,
            Activity::Waiting => waiting,
        };
        if let Err(error) = write_json(&file, &at.unix_timestamp_nanos()) {
            tracing::debug!(error = %error, "the session's stamp could not be written");
        }
    }

    fn activity(&self, session: &SessionRef) -> SessionActivity {
        let Ok((working, waiting)) = self
            .state
            .activity_files(session.host.as_str(), session.key.as_str())
        else {
            return SessionActivity::default();
        };
        let read = |file: &std::path::Path| {
            read_json::<i128>(file)
                .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos).ok())
        };
        SessionActivity {
            working: read(&working),
            waiting: read(&waiting),
        }
    }
}

impl LedgerEntry for LockedLedger {
    fn read(&self) -> SessionLedger {
        let file: LedgerFile = read_json(&self.file).unwrap_or_default();
        SessionLedger {
            last_injection: file.last_injection,
            shown: file.shown,
            pushed: file
                .pushed
                .iter()
                .filter_map(|pushed| {
                    Some(PushedDelivery {
                        pending: views::pending_of(pushed.message.as_ref(), pushed.offer.as_ref())?,
                        written: pushed.written,
                        due: pushed.due,
                    })
                })
                .collect(),
            wakes: file
                .wakes
                .into_iter()
                .map(|wake| Wake {
                    sender: wake.sender,
                    at: wake.at,
                })
                .collect(),
        }
    }

    fn write(&self, ledger: &SessionLedger) -> bool {
        let file = LedgerFile {
            last_injection: ledger.last_injection,
            shown: ledger.shown.clone(),
            pushed: ledger
                .pushed
                .iter()
                .map(|pushed| {
                    let (message, offer) = views::remember_pending(&pushed.pending);
                    PushedFile {
                        message,
                        offer,
                        written: pushed.written,
                        due: pushed.due,
                    }
                })
                .collect(),
            wakes: ledger
                .wakes
                .iter()
                .map(|wake| WakeFile {
                    sender: wake.sender.clone(),
                    at: wake.at,
                })
                .collect(),
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
            pushed: vec![PushedDelivery {
                pending: dibs_agent_host::ports::Pending::Offer(
                    dibs_agent_host::ports::OfferLine {
                        slot: dibs_domain::slot::SlotName::parse("deploy:prod").unwrap(),
                        until: OffsetDateTime::UNIX_EPOCH,
                    },
                ),
                written: false,
                due: OffsetDateTime::UNIX_EPOCH,
            }],
            wakes: vec![Wake {
                sender: Handle::parse("alice").unwrap(),
                at: OffsetDateTime::UNIX_EPOCH,
            }],
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

    #[test]
    fn a_sessions_stamps_are_read_back_and_a_torn_one_is_not_known() {
        let dir = tempfile::tempdir().unwrap();
        let ledgers = ledgers(dir.path());
        let session = session("s-1");
        assert_eq!(ledgers.activity(&session), SessionActivity::default());
        let at = OffsetDateTime::UNIX_EPOCH + time::Duration::minutes(3);
        ledgers.stamp(&session, Activity::Working, at);
        ledgers.stamp(&session, Activity::Waiting, at + time::Duration::seconds(1));
        assert_eq!(
            ledgers.activity(&session),
            SessionActivity {
                working: Some(at),
                waiting: Some(at + time::Duration::seconds(1)),
            }
        );
        let (working, _) = ledgers.state.activity_files("neutral", "s-1").unwrap();
        std::fs::write(&working, b"12").unwrap();
        std::fs::write(&working, b"{tor").unwrap();
        assert_eq!(ledgers.activity(&session).working, None);
        assert_eq!(
            ledgers.activity(&self::session("s-2")),
            SessionActivity::default()
        );
    }
}
