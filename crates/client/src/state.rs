//! What the client remembers between runs, under the state directory, per server and project:
//!
//! - `sessions/<scope>/<label>.json` — the session id registered under a label;
//! - `cache/<scope>/settings.json` — the project's settings and when they were fetched, so a path
//!   no area or rule governs is answered without the server;
//! - `cache/<scope>/status.json` — the last status and when it was fetched;
//! - `cache/<scope>/unreachable` — when a call last found the server unreachable.
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

use dibs_domain::ids::SessionId;
use dibs_domain::message::SessionLabel;
use dibs_domain::settings::SettingsDocument;

use crate::api::{Session, Status};
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

fn read_json<T: DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
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
        };
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
