//! What the client remembers between runs, under the state directory, per server and project:
//!
//! - `sessions/<scope>/<label>.json` — the session id registered under a label;
//! - `cache/<scope>/settings.json` — the project's settings and when they were fetched, so a path
//!   no area or rule governs is answered without the server;
//! - `cache/<scope>/status.json` — the last status and when it was fetched;
//! - `cache/<scope>/unreachable` — when a call last found the server unreachable;
//! - `cache/<scope>/member.json` — the handle the token signs in as;
//! - `cache/<scope>/inbox.json` — the conversations with unread messages, the messages awaiting the
//!   member's acknowledgement, and when they were fetched;
//! - `cache/<scope>/held.json` — areas edit checks found the member holding, each with when;
//! - `cache/<scope>/heartbeat`, `warned`, `sync-started` — when the last heartbeat was sent, when
//!   the person was last warned about an unreachable server, when a background sync was last
//!   started;
//! - `cache/<scope>/<name>.lock` — the lock files of whoever holds them;
//! - `cache/<scope>/<name>` — files callers keep for themselves ([`StateDir::file`]).
//!
//! `<scope>` is a digest of the server and the project, so one person in several projects keeps
//! them apart. Every file is replaced in one step (written beside it, then renamed over it), so a
//! reader never sees half of one. A file that is missing or unreadable is simply not remembered.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;

use dibs_domain::area::QualifiedArea;
use dibs_domain::handle::Handle;
use dibs_domain::ids::SessionId;
use dibs_domain::message::SessionLabel;
use dibs_domain::settings::SettingsDocument;

use crate::api::{Inbox, Session, Status};
use crate::credentials::CredentialKey;

/// The state of one project on one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDir {
    sessions: PathBuf,
    cache: PathBuf,
}

/// The settings as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSettings {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The settings in their normal form.
    pub settings: SettingsDocument,
}

/// The status as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedStatus {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The status.
    pub status: Status,
}

/// The inbox as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedInbox {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The conversations with unread messages, and the messages awaiting an acknowledgement.
    pub inbox: Inbox,
}

/// An area an edit check found the member holding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotedHold {
    /// The area.
    pub area: QualifiedArea,
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
}

#[derive(Serialize, Deserialize)]
struct MemberFile {
    handle: Handle,
}

#[derive(Serialize, Deserialize)]
struct SessionFile {
    session_id: SessionId,
    label: SessionLabel,
}

#[derive(Serialize, Deserialize)]
struct Moment {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

impl StateDir {
    /// The state of the key's project under the state directory `base`.
    pub fn new(base: &Path, key: &CredentialKey) -> Self {
        let scope = scope(key);
        Self {
            sessions: base.join("sessions").join(&scope),
            cache: base.join("cache").join(&scope),
        }
    }

    /// The session id registered under `label`.
    pub fn session(&self, label: &SessionLabel) -> Option<SessionId> {
        let file: SessionFile = read_json(&self.session_file(label))?;
        (file.label == *label).then_some(file.session_id)
    }

    /// Remembers a registered session.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_session(&self, session: &Session) -> io::Result<()> {
        write_json(
            &self.session_file(&session.label),
            &SessionFile {
                session_id: session.session_id,
                label: session.label.clone(),
            },
        )
    }

    /// The settings as last fetched.
    pub fn settings(&self) -> Option<CachedSettings> {
        read_json(&self.cache.join("settings.json"))
    }

    /// Remembers the settings fetched at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_settings(
        &self,
        settings: &SettingsDocument,
        at: OffsetDateTime,
    ) -> io::Result<()> {
        write_json(
            &self.cache.join("settings.json"),
            &CachedSettings {
                fetched_at: at,
                settings: settings.clone(),
            },
        )
    }

    /// The status as last fetched.
    pub fn status(&self) -> Option<CachedStatus> {
        read_json(&self.cache.join("status.json"))
    }

    /// Remembers the status fetched at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_status(&self, status: &Status, at: OffsetDateTime) -> io::Result<()> {
        write_json(
            &self.cache.join("status.json"),
            &CachedStatus {
                fetched_at: at,
                status: status.clone(),
            },
        )
    }

    /// When a call last found the server unreachable, unless one has reached it since.
    pub fn unreachable_since(&self) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.cache.join("unreachable")).map(|moment| moment.at)
    }

    /// Remembers that the server was unreachable at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn mark_unreachable(&self, at: OffsetDateTime) -> io::Result<()> {
        write_json(&self.cache.join("unreachable"), &Moment { at })
    }

    /// Forgets that the server was unreachable.
    ///
    /// # Errors
    /// The file system's failure other than the mark being absent.
    pub fn clear_unreachable(&self) -> io::Result<()> {
        match fs::remove_file(self.cache.join("unreachable")) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The handle the token signs in as, as last learned.
    pub fn member(&self) -> Option<Handle> {
        read_json::<MemberFile>(&self.cache.join("member.json")).map(|file| file.handle)
    }

    /// Remembers the handle the token signs in as.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_member(&self, handle: &Handle) -> io::Result<()> {
        write_json(
            &self.cache.join("member.json"),
            &MemberFile {
                handle: handle.clone(),
            },
        )
    }

    /// The inbox as last fetched.
    pub fn inbox(&self) -> Option<CachedInbox> {
        read_json(&self.cache.join("inbox.json"))
    }

    /// Remembers the inbox fetched at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_inbox(&self, inbox: &Inbox, at: OffsetDateTime) -> io::Result<()> {
        write_json(
            &self.cache.join("inbox.json"),
            &CachedInbox {
                fetched_at: at,
                inbox: inbox.clone(),
            },
        )
    }

    /// The areas edit checks found the member holding.
    pub fn noted_holds(&self) -> Vec<NotedHold> {
        read_json(&self.cache.join("held.json")).unwrap_or_default()
    }

    /// Remembers that the member holds `areas` as of `at`, keeping the newest note per area and
    /// forgetting notes older than `keep` before `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn note_holds(
        &self,
        areas: &[QualifiedArea],
        at: OffsetDateTime,
        keep: time::Duration,
    ) -> io::Result<()> {
        let mut holds: Vec<NotedHold> = self
            .noted_holds()
            .into_iter()
            .filter(|hold| !areas.contains(&hold.area) && hold.at <= at && at - hold.at < keep)
            .collect();
        holds.extend(areas.iter().map(|area| NotedHold {
            area: area.clone(),
            at,
        }));
        write_json(&self.cache.join("held.json"), &holds)
    }

    /// When the last heartbeat from this machine was sent.
    pub fn heartbeat_at(&self) -> Option<OffsetDateTime> {
        self.moment("heartbeat")
    }

    /// Remembers that a heartbeat was sent at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_heartbeat(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("heartbeat", at)
    }

    /// When the person was last warned that the server could not be reached.
    pub fn warned_at(&self) -> Option<OffsetDateTime> {
        self.moment("warned")
    }

    /// Remembers that the person was warned at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_warning(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("warned", at)
    }

    /// When a background sync was last started.
    pub fn sync_started_at(&self) -> Option<OffsetDateTime> {
        self.moment("sync-started")
    }

    /// Remembers that a background sync was started at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_sync_started(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("sync-started", at)
    }

    /// The lock file of this name (`sync` → `cache/<scope>/sync.lock`), its directory made.
    ///
    /// # Errors
    /// The file system's failure to make the directory.
    pub fn lock_file(&self, name: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.cache)?;
        Ok(self.cache.join(format!("{}.lock", file_name(name))))
    }

    /// A file of this project's cache that a caller keeps itself (`origin.json`); the name is
    /// written so it is one file name on every platform.
    pub fn file(&self, name: &str) -> PathBuf {
        self.cache.join(file_name(name))
    }

    fn moment(&self, name: &str) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.cache.join(name)).map(|moment| moment.at)
    }

    fn remember_moment(&self, name: &str, at: OffsetDateTime) -> io::Result<()> {
        write_json(&self.cache.join(name), &Moment { at })
    }

    fn session_file(&self, label: &SessionLabel) -> PathBuf {
        self.sessions
            .join(format!("{}.json", file_name(label.as_str())))
    }
}

/// The digest naming one project on one server.
fn scope(key: &CredentialKey) -> String {
    let digest = Sha256::digest(format!("{}\n{}", key.server, key.project).as_bytes());
    digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A label written so it is one file name on every platform: letters, digits, `-` and `_` as
/// they are, every other byte as `%XX`.
fn file_name(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for byte in label.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The JSON file at `path`, read; `None` when it is missing or not in the shape asked for.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

/// Writes `value` as JSON to `path` in one step: written beside it, then renamed over it, which
/// replaces an existing file on every platform. The directory is made when missing.
///
/// # Errors
/// The file system's failure.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(dir)?;
    let text = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut temp = tempfile::Builder::new().prefix(".dibs-").tempfile_in(dir)?;
    temp.write_all(&text)?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use dibs_domain::project::ProjectSlug;
    use time::Duration;

    use super::*;
    use crate::server_url::ServerUrl;

    fn key(project: &str) -> CredentialKey {
        CredentialKey {
            server: ServerUrl::parse("https://dibs.example.com").unwrap(),
            project: ProjectSlug::parse(project).unwrap(),
        }
    }

    fn session(label: &str, id: u64) -> Session {
        Session {
            session_id: SessionId(id),
            host: "laptop".parse().unwrap(),
            label: label.parse().unwrap(),
            agent: "cli".parse().unwrap(),
            push: false,
            last_seen: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn sessions_are_remembered_per_project_and_label() {
        let dir = tempfile::tempdir().unwrap();
        let acme = StateDir::new(dir.path(), &key("acme"));
        let globex = StateDir::new(dir.path(), &key("globex"));
        acme.remember_session(&session("laptop/cli", 3)).unwrap();
        acme.remember_session(&session("laptop:9/mcp", 4)).unwrap();
        globex.remember_session(&session("laptop/cli", 9)).unwrap();
        let label = |text: &str| SessionLabel::parse(text).unwrap();
        assert_eq!(acme.session(&label("laptop/cli")), Some(SessionId(3)));
        assert_eq!(acme.session(&label("laptop:9/mcp")), Some(SessionId(4)));
        assert_eq!(globex.session(&label("laptop/cli")), Some(SessionId(9)));
        assert_eq!(acme.session(&label("desktop/cli")), None);
        acme.remember_session(&session("laptop/cli", 5)).unwrap();
        assert_eq!(acme.session(&label("laptop/cli")), Some(SessionId(5)));
    }

    #[test]
    fn caches_keep_what_was_fetched_and_when() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        assert!(state.settings().is_none());
        let at = OffsetDateTime::UNIX_EPOCH + Duration::days(1);
        let settings = SettingsDocument {
            stale_after: Some("2h".to_owned()),
            ..SettingsDocument::default()
        };
        state.remember_settings(&settings, at).unwrap();
        assert_eq!(
            state.settings().unwrap(),
            CachedSettings {
                fetched_at: at,
                settings
            }
        );
        let status = Status {
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            ready: crate::api::ReadyList::default(),
            slots: Vec::new(),
        };
        let earlier: Status =
            serde_json::from_str(r#"{"live": [], "stale": [], "holds": []}"#).unwrap();
        assert_eq!(
            earlier, status,
            "a status cached before the ready list and the slots reads as empty"
        );
        state.remember_status(&status, at).unwrap();
        assert_eq!(state.status().unwrap().fetched_at, at);
    }

    #[test]
    fn the_unreachable_mark_is_set_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        assert_eq!(state.unreachable_since(), None);
        state.clear_unreachable().unwrap();
        let at = OffsetDateTime::UNIX_EPOCH + Duration::minutes(3);
        state.mark_unreachable(at).unwrap();
        assert_eq!(state.unreachable_since(), Some(at));
        state.clear_unreachable().unwrap();
        assert_eq!(state.unreachable_since(), None);
    }

    #[test]
    fn the_member_inbox_and_moments_are_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        assert_eq!(state.member(), None);
        state.remember_member(&"bob".parse().unwrap()).unwrap();
        assert_eq!(state.member().unwrap().as_str(), "bob");
        let at = OffsetDateTime::UNIX_EPOCH + Duration::hours(2);
        let inbox = Inbox {
            threads: Vec::new(),
            unacked: Vec::new(),
            offers: Vec::new(),
        };
        state.remember_inbox(&inbox, at).unwrap();
        assert_eq!(state.inbox().unwrap().fetched_at, at);
        let written_before_acks: CachedInbox = serde_json::from_str(
            r#"{"fetched_at": "1970-01-01T02:00:00Z", "inbox": {"threads": []}}"#,
        )
        .unwrap();
        assert!(
            written_before_acks.inbox.unacked.is_empty(),
            "a cache written before acknowledgements reads as awaiting none"
        );
        assert_eq!(state.heartbeat_at(), None);
        state.remember_heartbeat(at).unwrap();
        state.remember_warning(at + Duration::seconds(1)).unwrap();
        state
            .remember_sync_started(at + Duration::seconds(2))
            .unwrap();
        assert_eq!(state.heartbeat_at(), Some(at));
        assert_eq!(state.warned_at(), Some(at + Duration::seconds(1)));
        assert_eq!(state.sync_started_at(), Some(at + Duration::seconds(2)));
        let lock = state.lock_file("sync").unwrap();
        assert_eq!(lock.file_name().unwrap(), "sync.lock");
        assert!(lock.parent().unwrap().is_dir());
    }

    #[test]
    fn a_noted_hold_keeps_its_newest_time_and_old_ones_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let billing = QualifiedArea::parse("api:billing").unwrap();
        let search = QualifiedArea::parse("api:search").unwrap();
        let at = OffsetDateTime::UNIX_EPOCH + Duration::hours(2);
        let keep = Duration::seconds(30);
        state
            .note_holds(std::slice::from_ref(&billing), at, keep)
            .unwrap();
        state
            .note_holds(
                std::slice::from_ref(&search),
                at + Duration::seconds(10),
                keep,
            )
            .unwrap();
        state
            .note_holds(
                std::slice::from_ref(&billing),
                at + Duration::seconds(20),
                keep,
            )
            .unwrap();
        let holds = state.noted_holds();
        assert_eq!(holds.len(), 2);
        assert!(holds.contains(&NotedHold {
            area: billing.clone(),
            at: at + Duration::seconds(20)
        }));
        state
            .note_holds(&[billing], at + Duration::seconds(45), keep)
            .unwrap();
        let later = state.noted_holds();
        assert_eq!(
            later.len(),
            1,
            "the search note is past its keep: {later:?}"
        );
    }

    #[test]
    fn a_damaged_file_is_not_remembered() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        state
            .remember_settings(&SettingsDocument::default(), OffsetDateTime::UNIX_EPOCH)
            .unwrap();
        let file = dir
            .path()
            .join("cache")
            .join(scope(&key("acme")))
            .join("settings.json");
        fs::write(&file, "{ half").unwrap();
        assert!(state.settings().is_none());
    }

    #[test]
    fn a_label_is_one_file_name() {
        assert_eq!(file_name("laptop/cli"), "laptop%2Fcli");
        assert_eq!(file_name("a b:c\\d"), "a%20b%3Ac%5Cd");
    }
}
