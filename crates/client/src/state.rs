//! What the client remembers between runs, under the state directory, per server and project:
//!
//! - `sessions/<scope>/<label>.json` — the session id registered under a label;
//! - `cache/<scope>/settings.json` — the project's settings and when they were fetched, so a path
//!   no area or rule governs is answered without the server;
//! - `cache/<scope>/status.json` — the last status and when it was fetched;
//! - `cache/<scope>/unreachable` — when a call last found the server unreachable;
//! - `cache/<scope>/member.json` — the handle the token signs in as;
//! - `cache/<scope>/inbox.json` — the conversations with unread messages, the messages awaiting the
//!   member's acknowledgement and those still to be delivered, the slots offered to them, when they
//!   were fetched, and the deliveries this machine claimed;
//! - `sessions/<scope>/delivery-<digest>.{json,lock}` — what one agent session was shown, and when,
//!   and what was pushed to it while it waited;
//! - `sessions/<scope>/activity-<digest>.{working,waiting}` — when that session last worked and last
//!   began to wait for its person;
//! - `sessions/<scope>/sync-started-<digest>` — when a background sync was last started for that
//!   session, forgotten when it ends;
//! - `cache/<scope>/held.json` — areas edit checks found the member holding, each with when;
//! - `cache/<scope>/heartbeats.json` — when each agent session last sent a heartbeat from this
//!   machine, by the digest of its host and its id; a session's moment is forgotten when it ends,
//!   and every moment once it is older than the project's `stale_after`;
//! - `cache/<scope>/warned`, `sync-started` — when the person was last warned about an unreachable
//!   server, when a background sync that does everything due, the machine's work included, was
//!   last started;
//! - `cache/<scope>/versions.json` — the client versions the server serves and recommends, and when
//!   they were last asked for;
//! - `cache/<scope>/worktree-beats-sent` — when the signs of life left in the pools' worktrees were
//!   last sent to the server; `cache/<scope>/worktrees-unserved` — when the server was last found
//!   to serve no worktree pools;
//! - `cache/<scope>/<name>.lock` — the lock files of whoever holds them, among them
//!   `worktree-<pool>-<name>.lock`, held by whatever resets, prepares or stops one worktree of a
//!   pool;
//! - `cache/<scope>/<name>` — files callers keep for themselves ([`StateDir::file`]);
//! - `worktrees/<scope>/<pool>/` — what this machine knows of one pool ([`WorktreeState`]):
//!   `pool.json`, the directory its worktrees live in and the repository they are of, written with
//!   every hold a take records; and of each of its worktrees: `<name>.hold.json`, the holder this
//!   machine took it for, why, when, and
//!   whether its reset finished, written on a take and removed on a release; `<name>.beat`, whose
//!   time of last change is the holder's last sign of life in it; `<name>.free-since`, when it was
//!   last seen free; `<name>.left`, the session whose agent ended in it and left it in use for a
//!   while, until it is free or taken again; `<name>.stopped`, when its environment was stopped for
//!   being left idle; `<name>.broken`, when and why its preparation, reset or rescue last failed;
//!   and `logs/<name>.log`, private to its user, what the project's commands last printed for it
//!   when nobody read their output.
//!
//! `<scope>` is a digest of the server and the project, so one person in several projects keeps
//! them apart; `<pool>` is the pool's id. Every file is replaced in one step (written beside it, then renamed over it), so a
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
use dibs_domain::ids::{ItemId, MessageId, SessionId, ThreadId};
use dibs_domain::message::{Importance, SessionLabel, Subject};
use dibs_domain::repo::RepoId;
use dibs_domain::settings::SettingsDocument;
use dibs_domain::slot::Purpose;
use dibs_domain::worktree::{AgentKey, PoolId, WorktreeName};

use crate::api::{Inbox, PendingOffer, Session, Status, Versions};
use crate::credentials::CredentialKey;

/// The state of one project on one server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateDir {
    sessions: PathBuf,
    cache: PathBuf,
    worktrees: PathBuf,
}

/// What this machine knows of the worktrees of one pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    dir: PathBuf,
}

/// Where a pool's worktrees live and which repository they are of, as the checkout that lends the
/// pool names them: what a sending of the signs of life needs to read the branch each worktree is
/// on, as the pool itself is known here only by its id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolRecord {
    /// The directory the worktrees live in, each in `<root>/<name>`.
    pub root: PathBuf,
    /// The repository the worktrees are of.
    pub repo: RepoId,
}

/// The holder this machine took a worktree for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeHoldRecord {
    /// The holder's session.
    pub session_id: SessionId,
    /// The holder's agent; `None` for the session itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKey>,
    /// Why it was taken, so that a hold the server let go of is taken again for the same reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<Purpose>,
    /// The work item the hold is for, by local id: the one the take named, or the one a beat's
    /// answer last said the server's hold had learned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// Whether the hold learned `item` from an edit rather than from the take, so that a hold the
    /// server let go of is taken again with an item a later edit may still replace.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub item_learned: bool,
    /// When it was taken.
    #[serde(with = "time::serde::rfc3339")]
    pub taken_at: OffsetDateTime,
    /// Whether the worktree was rescued and reset (or prepared) for this holder to the end; a take
    /// answered again resumes one that did not finish.
    pub reset_done: bool,
}

/// Who left the last sign of life in a worktree: a session, and its agent when an agent did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeatBy {
    /// The session.
    pub session_id: SessionId,
    /// The agent of the session; `None` for the session itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKey>,
}

impl WorktreeHoldRecord {
    /// Whether it records this holder: the session and, for an agent, its key.
    pub fn is_of(&self, session: SessionId, agent: Option<&AgentKey>) -> bool {
        self.session_id == session && self.agent.as_ref() == agent
    }

    /// The record once a beat's answer said its hold is for `item`, when that changes it. An item
    /// other than the recorded one was learned from an edit, as the item a take names stays the
    /// hold's while it lasts. An answer that names no item changes nothing: a live hold never
    /// loses its item, and a server that does not answer it names none.
    pub fn answered(&self, item: Option<ItemId>) -> Option<Self> {
        let item = item.filter(|item| self.item != Some(*item))?;
        Some(Self {
            item: Some(item),
            item_learned: true,
            ..self.clone()
        })
    }

    /// What a take of the hold again names: the item, and the item learned, of which one at most.
    pub const fn retaken_item(&self) -> (Option<ItemId>, Option<ItemId>) {
        if self.item_learned {
            (None, self.item)
        } else {
            (self.item, None)
        }
    }
}

/// Why a worktree is set aside until it is prepared again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrokenWorktree {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// What failed, as a sentence.
    pub reason: String,
}

/// The settings as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedSettings {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The settings in their normal form.
    pub settings: SettingsDocument,
    /// The version of the `dibs` that fetched them: a server serves a client the settings as far
    /// as its version can read them, so another version's copy may lack what this one reads.
    #[serde(default)]
    pub client: Option<String>,
}

/// The version of this `dibs`, which names the settings it fetched.
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The status as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedStatus {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The status.
    pub status: Status,
}

/// The client versions the server serves and recommends, as last asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedVersions {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub checked_at: OffsetDateTime,
    /// The answer.
    pub versions: Versions,
}

/// The inbox as last fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedInbox {
    /// When.
    #[serde(with = "time::serde::rfc3339")]
    pub fetched_at: OffsetDateTime,
    /// The conversations with unread messages, and the messages awaiting an acknowledgement.
    pub inbox: Inbox,
    /// The deliveries this machine claimed from the server, won or lost; kept across refreshes of
    /// the inbox.
    #[serde(default)]
    pub claims: Vec<ClaimedDelivery>,
}

/// A delivery this machine claimed for one session of an agent host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimedDelivery {
    /// What: a message's id, or `offer:<slot>:<deadline>`.
    pub subject: String,
    /// The host of the session it was claimed for.
    pub host: String,
    /// That host's id of the session.
    pub session: String,
    /// Whether the session won it; otherwise another session of the member had it first.
    pub won: bool,
    /// When it was claimed.
    #[serde(with = "time::serde::rfc3339")]
    pub at: OffsetDateTime,
    /// The message it delivers, when it is one: kept, because the server stops listing a message
    /// once it was delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<RememberedMessage>,
    /// The offer it delivers, when it is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offer: Option<PendingOffer>,
}

/// What a claimed delivery keeps of its message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedMessage {
    /// The message.
    pub id: MessageId,
    /// Its thread.
    pub thread: ThreadId,
    /// The work item its thread is about, when it is about one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<ItemId>,
    /// Who wrote it.
    pub author: Handle,
    /// Whether dibs wrote it, in the project's name.
    #[serde(default)]
    pub system: bool,
    /// Its thread's subject.
    pub subject: Subject,
    /// How much it asks for attention.
    pub importance: Importance,
    /// Whether it awaits the member's acknowledgement.
    pub awaiting_ack: bool,
    /// Whether acknowledging it grants something.
    #[serde(default)]
    pub ack_grants: bool,
    /// Its text.
    pub body: String,
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
struct LeftBy {
    session_id: SessionId,
}

#[derive(Serialize, Deserialize)]
struct Moment {
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

/// When one session last sent a heartbeat, the session named by [`session_digest`].
#[derive(Serialize, Deserialize)]
struct SessionMoment {
    session: String,
    #[serde(with = "time::serde::rfc3339")]
    at: OffsetDateTime,
}

/// The file of every session's last heartbeat in the project's cache.
const HEARTBEATS: &str = "heartbeats.json";

impl StateDir {
    /// The state of the key's project under the state directory `base`.
    pub fn new(base: &Path, key: &CredentialKey) -> Self {
        let scope = scope(key);
        Self {
            sessions: base.join("sessions").join(&scope),
            cache: base.join("cache").join(&scope),
            worktrees: base.join("worktrees").join(&scope),
        }
    }

    /// What this machine knows of the worktrees of `pool`.
    pub fn worktrees(&self, pool: &PoolId) -> WorktreeState {
        WorktreeState {
            dir: self.worktrees.join(pool.as_str()),
        }
    }

    /// Every pool of the project this machine keeps files of, each with its id, in the order of
    /// their ids.
    pub fn worktree_pools(&self) -> Vec<(PoolId, WorktreeState)> {
        let Ok(entries) = fs::read_dir(&self.worktrees) else {
            return Vec::new();
        };
        let mut pools: Vec<(PoolId, WorktreeState)> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| PoolId::parse(&entry.file_name().to_string_lossy()).ok())
            .map(|pool| {
                let files = self.worktrees(&pool);
                (pool, files)
            })
            .collect();
        pools.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        pools
    }

    /// The newest sign of life left in a worktree of any of the project's pools on this machine.
    pub fn newest_worktree_beat(&self) -> Option<OffsetDateTime> {
        self.worktree_pools()
            .iter()
            .filter_map(|(_, files)| files.newest_beat())
            .max()
    }

    /// When the signs of life left in the pools' worktrees were last sent.
    pub fn worktree_beats_sent_at(&self) -> Option<OffsetDateTime> {
        self.moment("worktree-beats-sent")
    }

    /// Remembers that the signs of life left until `at` were sent.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_worktree_beats_sent(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("worktree-beats-sent", at)
    }

    /// When the server was last found to serve no worktree pools.
    pub fn worktrees_unserved_at(&self) -> Option<OffsetDateTime> {
        self.moment("worktrees-unserved")
    }

    /// Remembers that the server served no worktree pools at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_worktrees_unserved(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("worktrees-unserved", at)
    }

    /// The lock file held by whatever resets, prepares or stops the worktree `name` of `pool`:
    /// `cache/<scope>/worktree-<pool>-<name>.lock`, its directory made.
    ///
    /// # Errors
    /// The file system's failure to make the directory.
    pub fn worktree_lock(&self, pool: &PoolId, name: &WorktreeName) -> io::Result<PathBuf> {
        self.lock_file(&format!("worktree-{pool}-{name}"))
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

    /// The settings as last fetched by this version of `dibs`; none when another version fetched
    /// them, so that they are fetched again as this version reads them.
    pub fn settings(&self) -> Option<CachedSettings> {
        read_json::<CachedSettings>(&self.cache.join("settings.json"))
            .filter(|cached| cached.client.as_deref() == Some(CLIENT_VERSION))
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
                client: Some(CLIENT_VERSION.to_owned()),
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

    /// The client versions as last asked for.
    pub fn versions(&self) -> Option<CachedVersions> {
        read_json(&self.cache.join("versions.json"))
    }

    /// Remembers the client versions the server answered at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_versions(&self, versions: &Versions, at: OffsetDateTime) -> io::Result<()> {
        write_json(
            &self.cache.join("versions.json"),
            &CachedVersions {
                checked_at: at,
                versions: versions.clone(),
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
        let claims = self.inbox().map(|cached| cached.claims).unwrap_or_default();
        write_json(
            &self.cache.join("inbox.json"),
            &CachedInbox {
                fetched_at: at,
                inbox: inbox.clone(),
                claims,
            },
        )
    }

    /// Replaces the deliveries remembered beside the inbox with `claims`, keeping the inbox as it
    /// was. Nothing is written while no inbox is cached.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_claims(&self, claims: &[ClaimedDelivery]) -> io::Result<()> {
        let Some(mut cached) = self.inbox() else {
            return Ok(());
        };
        cached.claims = claims.to_vec();
        write_json(&self.cache.join("inbox.json"), &cached)
    }

    /// The file of what one session of a host was shown, and the lock file a hook takes before it
    /// reads or writes it: `sessions/<scope>/delivery-<digest>.json` and `.lock`, the digest of the
    /// host and its session id, so any id makes one file name. The directory is made.
    ///
    /// # Errors
    /// The file system's failure to make the directory.
    pub fn delivery_files(&self, host: &str, session: &str) -> io::Result<(PathBuf, PathBuf)> {
        fs::create_dir_all(&self.sessions)?;
        let name = session_digest(host, session);
        Ok((
            self.sessions.join(format!("delivery-{name}.json")),
            self.sessions.join(format!("delivery-{name}.lock")),
        ))
    }

    /// The files of one agent session's stamps — when it last worked and when it last began to
    /// wait for its person — named as [`delivery_files`](Self::delivery_files) names its ledger:
    /// `sessions/<scope>/activity-<digest>.working` and `.waiting`.
    ///
    /// # Errors
    /// The file system's failure to make the directory.
    pub fn activity_files(&self, host: &str, session: &str) -> io::Result<(PathBuf, PathBuf)> {
        let (ledger, _) = self.delivery_files(host, session)?;
        let name = ledger
            .file_stem()
            .map(|stem| stem.to_string_lossy().replacen("delivery-", "activity-", 1))
            .unwrap_or_default();
        Ok((
            self.sessions.join(format!("{name}.working")),
            self.sessions.join(format!("{name}.waiting")),
        ))
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

    /// When the session `session` of the host `host` last sent a heartbeat from this machine. Every
    /// session keeps its own moment, so one session's heartbeat never stands for another's.
    pub fn heartbeat_at(&self, host: &str, session: &str) -> Option<OffsetDateTime> {
        let digest = session_digest(host, session);
        self.heartbeats()
            .into_iter()
            .find(|moment| moment.session == digest)
            .map(|moment| moment.at)
    }

    /// Remembers that the session `session` of the host `host` sent a heartbeat at `at`, and
    /// forgets every other session's moment that is `keep` or more before `at` or after it.
    ///
    /// The file is read and replaced without a lock: a session's end written at the same moment
    /// may be lost, and that session's moment then stays until it is `keep` old.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_heartbeat(
        &self,
        host: &str,
        session: &str,
        at: OffsetDateTime,
        keep: time::Duration,
    ) -> io::Result<()> {
        let digest = session_digest(host, session);
        let mut moments: Vec<SessionMoment> = self
            .heartbeats()
            .into_iter()
            .filter(|moment| moment.session != digest && moment.at <= at && at - moment.at < keep)
            .collect();
        moments.push(SessionMoment {
            session: digest,
            at,
        });
        write_json(&self.cache.join(HEARTBEATS), &moments)
    }

    /// Forgets when the session `session` of the host `host` last sent a heartbeat, as it ended.
    /// Nothing is written when no moment of it is kept.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn forget_heartbeat(&self, host: &str, session: &str) -> io::Result<()> {
        let digest = session_digest(host, session);
        let moments = self.heartbeats();
        if !moments.iter().any(|moment| moment.session == digest) {
            return Ok(());
        }
        let kept: Vec<SessionMoment> = moments
            .into_iter()
            .filter(|moment| moment.session != digest)
            .collect();
        write_json(&self.cache.join(HEARTBEATS), &kept)
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

    /// When a background sync that does everything due, the machine's work included, was last
    /// started.
    pub fn sync_started_at(&self) -> Option<OffsetDateTime> {
        self.moment("sync-started")
    }

    /// Remembers that a background sync that does everything due was started at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_sync_started(&self, at: OffsetDateTime) -> io::Result<()> {
        self.remember_moment("sync-started", at)
    }

    /// When a background sync was last started for the session `session` of the host `host`.
    pub fn session_sync_started_at(&self, host: &str, session: &str) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.session_sync_file(host, session)).map(|moment| moment.at)
    }

    /// Remembers that a background sync was started for the session `session` of the host
    /// `host` at `at`. Each session has a file of its own, so sessions starting syncs at the same
    /// moment never overwrite one another's.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_session_sync_started(
        &self,
        host: &str,
        session: &str,
        at: OffsetDateTime,
    ) -> io::Result<()> {
        write_json(&self.session_sync_file(host, session), &Moment { at })
    }

    /// Forgets when a background sync was last started for the session `session` of the host
    /// `host`, as it ended; nothing happens when none was.
    ///
    /// # Errors
    /// The file system's failure, other than the file being gone already.
    pub fn forget_session_sync_started(&self, host: &str, session: &str) -> io::Result<()> {
        match fs::remove_file(self.session_sync_file(host, session)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// The lock file of this name (`sync` → `cache/<scope>/sync.lock`), its directory made.
    ///
    /// # Errors
    /// The file system's failure to make the directory.
    pub fn lock_file(&self, name: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(&self.cache)?;
        Ok(self.cache.join(format!("{}.lock", file_name(name))))
    }

    /// A file of this project's cache that a caller keeps itself (`origin`); the name is written so
    /// it is one file name on every platform.
    pub fn file(&self, name: &str) -> PathBuf {
        self.cache.join(file_name(name))
    }

    /// A directory of this project's cache where a caller spools files of its own: `report` →
    /// `cache/<scope>/report.spool`.
    pub fn spool_dir(&self, name: &str) -> PathBuf {
        self.cache.join(format!("{}.spool", file_name(name)))
    }

    /// A JSON file of this project's cache that a caller keeps itself: `report` →
    /// `cache/<scope>/report.json`.
    pub fn json_file(&self, name: &str) -> PathBuf {
        self.cache.join(format!("{}.json", file_name(name)))
    }

    fn heartbeats(&self) -> Vec<SessionMoment> {
        read_json(&self.cache.join(HEARTBEATS)).unwrap_or_default()
    }

    fn moment(&self, name: &str) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.cache.join(name)).map(|moment| moment.at)
    }

    fn remember_moment(&self, name: &str, at: OffsetDateTime) -> io::Result<()> {
        write_json(&self.cache.join(name), &Moment { at })
    }

    fn session_sync_file(&self, host: &str, session: &str) -> PathBuf {
        self.sessions
            .join(format!("sync-started-{}", session_digest(host, session)))
    }

    fn session_file(&self, label: &SessionLabel) -> PathBuf {
        self.sessions
            .join(format!("{}.json", file_name(label.as_str())))
    }
}

impl WorktreeState {
    /// The directory of the pool's files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the pool's worktrees live and which repository they are of, when a take recorded it.
    pub fn pool(&self) -> Option<PoolRecord> {
        read_json(&self.dir.join(POOL_FILE))
    }

    /// Remembers where the pool's worktrees live and which repository they are of.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_pool(&self, record: &PoolRecord) -> io::Result<()> {
        write_json(&self.dir.join(POOL_FILE), record)
    }

    /// The holder this machine took `name` for, when it holds a record of one.
    pub fn hold(&self, name: &WorktreeName) -> Option<WorktreeHoldRecord> {
        read_json(&self.path(name, "hold.json"))
    }

    /// Remembers the holder `name` was taken for.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn remember_hold(
        &self,
        name: &WorktreeName,
        record: &WorktreeHoldRecord,
    ) -> io::Result<()> {
        write_json(&self.path(name, "hold.json"), record)
    }

    /// Forgets the holder of `name`.
    ///
    /// # Errors
    /// The file system's failure other than the record being absent.
    pub fn forget_hold(&self, name: &WorktreeName) -> io::Result<()> {
        remove(&self.path(name, "hold.json"))
    }

    /// Every holder this machine holds a record of, with the worktree, in the order of the names.
    pub fn holds(&self) -> Vec<(WorktreeName, WorktreeHoldRecord)> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut holds: Vec<(WorktreeName, WorktreeHoldRecord)> = entries
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name();
                let name = file.to_str()?.strip_suffix(".hold.json")?.to_owned();
                let name = WorktreeName::parse(&name).ok()?;
                let record = self.hold(&name)?;
                Some((name, record))
            })
            .collect();
        holds.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        holds
    }

    /// The file whose time of last change is the holder's last sign of life in `name`.
    pub fn beat_file(&self, name: &WorktreeName) -> PathBuf {
        self.path(name, "beat")
    }

    /// When the holder of `name` was last seen in it, by the beat file's time of last change.
    pub fn beat_at(&self, name: &WorktreeName) -> Option<OffsetDateTime> {
        let modified = fs::metadata(self.beat_file(name)).ok()?.modified().ok()?;
        Some(OffsetDateTime::from(modified))
    }

    /// Leaves a sign of life in `name` at `at`, left by the holder `by` when it is known: its beat
    /// file, made when missing, naming that holder and last changed then.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn touch_beat(
        &self,
        name: &WorktreeName,
        at: OffsetDateTime,
        by: Option<&BeatBy>,
    ) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let text = match by {
            Some(by) => serde_json::to_vec(by).map_err(io::Error::other)?,
            None => Vec::new(),
        };
        let mut file = fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(self.beat_file(name))?;
        file.write_all(&text)?;
        file.set_modified(at.into())
    }

    /// The holder that left the last sign of life in `name`, when the beat file names one.
    pub fn beat_by(&self, name: &WorktreeName) -> Option<BeatBy> {
        read_json(&self.beat_file(name))
    }

    /// The newest sign of life left in any worktree of the pool.
    pub fn newest_beat(&self) -> Option<OffsetDateTime> {
        let entries = fs::read_dir(&self.dir).ok()?;
        entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|file| file.ends_with(".beat"))
            })
            .filter_map(|entry| entry.metadata().ok()?.modified().ok())
            .map(OffsetDateTime::from)
            .max()
    }

    /// When `name` was last seen free: released here, or found free by the server.
    pub fn free_since(&self, name: &WorktreeName) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.path(name, "free-since")).map(|moment| moment.at)
    }

    /// Remembers that `name` is free since `at`, and forgets which session left it.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn mark_free(&self, name: &WorktreeName, at: OffsetDateTime) -> io::Result<()> {
        write_json(&self.path(name, "free-since"), &Moment { at })?;
        remove(&self.path(name, "left"))
    }

    /// Remembers that an agent of `session` ended in `name` and left it in use for a while.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn mark_left(&self, name: &WorktreeName, session: SessionId) -> io::Result<()> {
        write_json(
            &self.path(name, "left"),
            &LeftBy {
                session_id: session,
            },
        )
    }

    /// The worktrees an agent of `session` ended in and left in use, in the order of the names.
    pub fn left_by(&self, session: SessionId) -> Vec<WorktreeName> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut left: Vec<WorktreeName> = entries
            .flatten()
            .filter_map(|entry| {
                let file = entry.file_name();
                let name = WorktreeName::parse(file.to_str()?.strip_suffix(".left")?).ok()?;
                let by: LeftBy = read_json(&entry.path())?;
                (by.session_id == session).then_some(name)
            })
            .collect();
        left.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        left
    }

    /// When the environment of `name` was stopped for being left idle.
    pub fn stopped_at(&self, name: &WorktreeName) -> Option<OffsetDateTime> {
        read_json::<Moment>(&self.path(name, "stopped")).map(|moment| moment.at)
    }

    /// Remembers that the environment of `name` was stopped at `at`.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn mark_stopped(&self, name: &WorktreeName, at: OffsetDateTime) -> io::Result<()> {
        write_json(&self.path(name, "stopped"), &Moment { at })
    }

    /// Forgets that `name` was free, which session left it, and that its environment was stopped:
    /// it was just taken.
    ///
    /// # Errors
    /// The file system's failure other than the marks being absent.
    pub fn mark_taken(&self, name: &WorktreeName) -> io::Result<()> {
        remove(&self.path(name, "free-since"))?;
        remove(&self.path(name, "left"))?;
        remove(&self.path(name, "stopped"))
    }

    /// Why `name` is set aside, when it is.
    pub fn broken(&self, name: &WorktreeName) -> Option<BrokenWorktree> {
        let path = self.path(name, "broken");
        read_json(&path).or_else(|| {
            path.exists().then(|| BrokenWorktree {
                at: OffsetDateTime::UNIX_EPOCH,
                reason: "it was set aside".to_owned(),
            })
        })
    }

    /// Sets `name` aside until it is prepared again.
    ///
    /// # Errors
    /// The file system's failure.
    pub fn mark_broken(&self, name: &WorktreeName, broken: &BrokenWorktree) -> io::Result<()> {
        write_json(&self.path(name, "broken"), broken)
    }

    /// Takes `name` back into the pool.
    ///
    /// # Errors
    /// The file system's failure other than the mark being absent.
    pub fn clear_broken(&self, name: &WorktreeName) -> io::Result<()> {
        remove(&self.path(name, "broken"))
    }

    fn path(&self, name: &WorktreeName, what: &str) -> PathBuf {
        self.dir.join(format!("{name}.{what}"))
    }
}

/// The file of a pool's [`PoolRecord`] in the pool's directory. No worktree's file is named so:
/// each of theirs is `<name>.<what>`, and no `<what>` is `json` alone.
const POOL_FILE: &str = "pool.json";

/// Removes a file, its absence being no failure.
fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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

/// The digest naming one session of one host: any id makes one file name, and no id is kept as
/// it was given.
fn session_digest(host: &str, session: &str) -> String {
    let digest = Sha256::digest(format!("{host}\n{session}").as_bytes());
    digest[..12]
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

/// Writes `value` as JSON to `path` in one step, as [`write_json`] does, and makes it durable
/// before it replaces the file: the written file is synced to the disk before it is renamed over
/// the old one, and the directory after, where the platform lets a directory be synced. A crash
/// leaves the old file or the new one, never a torn one.
///
/// # Errors
/// The file system's failure.
pub fn write_json_durably<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::create_dir_all(dir)?;
    let text = serde_json::to_vec(value).map_err(io::Error::other)?;
    let mut temp = tempfile::Builder::new().prefix(".dibs-").tempfile_in(dir)?;
    temp.write_all(&text)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    sync_dir(dir);
    Ok(())
}

/// Syncs the directory's entries to the disk, so a rename in it survives a crash.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Err(error) = fs::File::open(dir).and_then(|dir| dir.sync_all()) {
        tracing::debug!(error = %error, "the state directory could not be synced");
    }
}

/// A directory cannot be opened to be synced on this platform; its rename is made durable by the
/// file system itself.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

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
    fn the_client_versions_are_remembered_with_when_they_were_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("widgets"));
        assert_eq!(state.versions(), None);
        let versions = Versions {
            server: semver::Version::new(0, 5, 0),
            min_client: None,
            latest_client: Some(semver::Version::new(0, 5, 0)),
        };
        let at = OffsetDateTime::UNIX_EPOCH + Duration::days(3);
        state.remember_versions(&versions, at).unwrap();
        let cached = state.versions().unwrap();
        assert_eq!(cached.checked_at, at);
        assert_eq!(cached.versions, versions);
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
                settings: settings.clone(),
                client: Some(env!("CARGO_PKG_VERSION").to_owned()),
            }
        );
        for (older, written) in [
            ("an older dibs", r#""client":"0.1.1","#),
            ("a dibs that named no version", ""),
        ] {
            std::fs::write(
                dir.path()
                    .join("cache")
                    .join(scope(&key("acme")))
                    .join("settings.json"),
                format!(
                    r#"{{{written}"fetched_at":"1970-01-02T00:00:00Z","settings":{}}}"#,
                    serde_json::to_string(&settings).unwrap()
                ),
            )
            .unwrap();
            assert!(
                state.settings().is_none(),
                "the settings {older} fetched are fetched again"
            );
        }
        let status = Status {
            live: Vec::new(),
            stale: Vec::new(),
            holds: Vec::new(),
            wide: Vec::new(),
            ready: crate::api::ReadyList::default(),
            slots: Vec::new(),
            escalations: Vec::new(),
            integration_silent_since: None,
            overlaps: Vec::new(),
            worktrees: Vec::new(),
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
            deliverable: Vec::new(),
            grant_asks: Vec::new(),
            unread_conversations: None,
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
        assert!(
            written_before_acks.inbox.grant_asks.is_empty(),
            "a cache written before grant requests were listed reads as none waiting"
        );
        state.remember_warning(at + Duration::seconds(1)).unwrap();
        state
            .remember_sync_started(at + Duration::seconds(2))
            .unwrap();
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

    #[test]
    fn a_pool_keeps_where_its_worktrees_live_and_their_repository_beside_their_holds() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let worktrees = state.worktrees(&PoolId::parse("c0ffee").unwrap());
        let other = state.worktrees(&PoolId::parse("beef").unwrap());
        assert_eq!(
            worktrees.pool(),
            None,
            "a pool taken before it was recorded"
        );
        let record = PoolRecord {
            root: dir.path().join("widgets-worktrees"),
            repo: RepoId::parse("api").unwrap(),
        };
        worktrees.remember_pool(&record).unwrap();
        assert_eq!(worktrees.pool(), Some(record.clone()));
        assert_eq!(other.pool(), None, "another pool's record is its own");
        let moved = PoolRecord {
            repo: RepoId::parse("web").unwrap(),
            ..record
        };
        worktrees.remember_pool(&moved).unwrap();
        assert_eq!(worktrees.pool(), Some(moved));

        let name = WorktreeName::parse("pool").unwrap();
        let hold = WorktreeHoldRecord {
            session_id: SessionId(4),
            agent: None,
            purpose: None,
            item: None,
            item_learned: false,
            taken_at: OffsetDateTime::UNIX_EPOCH,
            reset_done: true,
        };
        worktrees.remember_hold(&name, &hold).unwrap();
        assert_eq!(
            worktrees.holds(),
            vec![(name, hold)],
            "the pool's record is no worktree's, even one named pool"
        );
        assert!(worktrees.pool().is_some());
    }

    #[test]
    fn a_hold_record_follows_the_item_a_beats_answer_names_as_learned() {
        let record = |item: Option<u64>, item_learned| WorktreeHoldRecord {
            session_id: SessionId(4),
            agent: None,
            purpose: None,
            item: item.map(ItemId),
            item_learned,
            taken_at: OffsetDateTime::UNIX_EPOCH,
            reset_done: true,
        };
        let none = record(None, false);
        assert_eq!(none.retaken_item(), (None, None));
        assert_eq!(none.answered(None), None, "nothing said, nothing learned");
        let learned = none.answered(Some(ItemId(3)));
        assert_eq!(learned, Some(record(Some(3), true)));
        assert_eq!(
            learned.as_ref().map(WorktreeHoldRecord::retaken_item),
            Some((None, Some(ItemId(3)))),
            "taken again with the item it learned"
        );
        let learned = record(Some(3), true);
        assert_eq!(learned.answered(Some(ItemId(3))), None);
        assert_eq!(learned.answered(None), None, "a hold keeps its item");
        assert_eq!(
            learned.answered(Some(ItemId(4))),
            Some(record(Some(4), true)),
            "an edit taught it another"
        );
        let named = record(Some(7), false);
        assert_eq!(named.answered(Some(ItemId(7))), None);
        assert_eq!(named.retaken_item(), (Some(ItemId(7)), None));
        let read: WorktreeHoldRecord = serde_json::from_str(
            r#"{"session_id":4,"item":7,"taken_at":"1970-01-01T00:00:00Z","reset_done":true}"#,
        )
        .unwrap();
        assert_eq!(
            read, named,
            "a record written before it said how reads as named"
        );
        let written = serde_json::to_value(&named).unwrap();
        assert!(written.get("item_learned").is_none());
    }

    #[test]
    fn a_pools_worktrees_keep_their_holder_marks_and_lock_apart_from_other_pools() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let pool = PoolId::parse("c0ffee").unwrap();
        let other = state.worktrees(&PoolId::parse("beef").unwrap());
        let worktrees = state.worktrees(&pool);
        let name = WorktreeName::parse("worktree2").unwrap();
        let at = OffsetDateTime::UNIX_EPOCH + Duration::hours(5);
        assert_eq!(worktrees.hold(&name), None);
        let record = WorktreeHoldRecord {
            session_id: SessionId(4),
            agent: Some(AgentKey::parse("a1b2").unwrap()),
            purpose: Some(Purpose::parse("fix the totals").unwrap()),
            item: Some(ItemId(7)),
            item_learned: false,
            taken_at: at,
            reset_done: false,
        };
        worktrees.remember_hold(&name, &record).unwrap();
        assert_eq!(worktrees.hold(&name), Some(record.clone()));
        assert_eq!(worktrees.holds(), vec![(name.clone(), record.clone())]);
        assert!(record.is_of(SessionId(4), Some(&AgentKey::parse("a1b2").unwrap())));
        assert!(!record.is_of(SessionId(4), None));
        assert_eq!(
            other.hold(&name),
            None,
            "another pool's records are its own"
        );
        worktrees.forget_hold(&name).unwrap();
        worktrees.forget_hold(&name).unwrap();
        assert_eq!(worktrees.hold(&name), None);

        worktrees.mark_left(&name, SessionId(4)).unwrap();
        assert_eq!(worktrees.left_by(SessionId(4)), vec![name.clone()]);
        assert!(worktrees.left_by(SessionId(5)).is_empty());
        assert!(other.left_by(SessionId(4)).is_empty());
        worktrees.mark_taken(&name).unwrap();
        assert!(worktrees.left_by(SessionId(4)).is_empty(), "taken again");
        worktrees.mark_left(&name, SessionId(4)).unwrap();
        worktrees.mark_free(&name, at).unwrap();
        assert!(worktrees.left_by(SessionId(4)).is_empty(), "free now");
        worktrees
            .mark_stopped(&name, at + Duration::minutes(30))
            .unwrap();
        assert_eq!(worktrees.free_since(&name), Some(at));
        assert_eq!(
            worktrees.stopped_at(&name),
            Some(at + Duration::minutes(30))
        );
        worktrees.mark_taken(&name).unwrap();
        assert_eq!(
            (worktrees.free_since(&name), worktrees.stopped_at(&name)),
            (None, None)
        );

        let broken = BrokenWorktree {
            at,
            reason: "Its reset failed.".to_owned(),
        };
        worktrees.mark_broken(&name, &broken).unwrap();
        assert_eq!(worktrees.broken(&name), Some(broken));
        worktrees.clear_broken(&name).unwrap();
        assert_eq!(worktrees.broken(&name), None);

        assert_eq!(worktrees.beat_at(&name), None);
        assert_eq!(worktrees.newest_beat(), None);
        assert_eq!(state.newest_worktree_beat(), None);
        fs::write(worktrees.beat_file(&name), b"").unwrap();
        assert!(worktrees.beat_at(&name).is_some());
        let beat = OffsetDateTime::UNIX_EPOCH + Duration::days(20_000);
        assert_eq!(worktrees.beat_by(&name), None, "an empty beat names nobody");
        let by = BeatBy {
            session_id: SessionId(7),
            agent: Some(AgentKey::parse("a1b2").unwrap()),
        };
        worktrees.touch_beat(&name, beat, Some(&by)).unwrap();
        let earlier = WorktreeName::parse("worktree1").unwrap();
        other
            .touch_beat(&earlier, beat - Duration::hours(1), None)
            .unwrap();
        assert_eq!(worktrees.beat_at(&name), Some(beat));
        assert_eq!(worktrees.beat_by(&name), Some(by));
        assert_eq!(other.beat_by(&earlier), None);
        worktrees.touch_beat(&name, beat, None).unwrap();
        assert_eq!(
            worktrees.beat_by(&name),
            None,
            "a sign of life of nobody known forgets the session of the one before"
        );
        assert_eq!(worktrees.newest_beat(), Some(beat));
        assert_eq!(
            state.newest_worktree_beat(),
            Some(beat),
            "the newest of every pool"
        );
        let pools: Vec<String> = state
            .worktree_pools()
            .iter()
            .map(|(pool, _)| pool.to_string())
            .collect();
        assert_eq!(pools, ["beef", "c0ffee"]);
        assert!(
            worktrees
                .beat_file(&name)
                .starts_with(dir.path().join("worktrees"))
        );

        let lock = state.worktree_lock(&pool, &name).unwrap();
        assert_eq!(lock.file_name().unwrap(), "worktree-c0ffee-worktree2.lock");
        assert!(lock.parent().unwrap().is_dir());
    }

    #[test]
    fn claimed_deliveries_survive_a_refresh_of_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let inbox = Inbox {
            threads: Vec::new(),
            unacked: Vec::new(),
            offers: Vec::new(),
            deliverable: Vec::new(),
            grant_asks: Vec::new(),
            unread_conversations: None,
        };
        let claim = ClaimedDelivery {
            subject: "42".to_owned(),
            host: "neutral".to_owned(),
            session: "s-1".to_owned(),
            won: true,
            at: OffsetDateTime::UNIX_EPOCH,
            message: None,
            offer: None,
        };
        state.remember_claims(std::slice::from_ref(&claim)).unwrap();
        assert_eq!(state.inbox(), None, "nothing is written without an inbox");
        state
            .remember_inbox(&inbox, OffsetDateTime::UNIX_EPOCH)
            .unwrap();
        state.remember_claims(std::slice::from_ref(&claim)).unwrap();
        let later = OffsetDateTime::UNIX_EPOCH + Duration::minutes(1);
        state.remember_inbox(&inbox, later).unwrap();
        let cached = state.inbox().unwrap();
        assert_eq!(cached.fetched_at, later);
        assert_eq!(cached.claims, [claim]);
    }

    #[test]
    fn each_session_keeps_its_own_heartbeat_moment_until_it_ends_or_grows_old() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let at = OffsetDateTime::UNIX_EPOCH + Duration::hours(10);
        let keep = Duration::hours(1);
        assert_eq!(state.heartbeat_at("neutral", "s-1"), None);
        state.forget_heartbeat("neutral", "s-1").unwrap();

        state
            .remember_heartbeat("neutral", "s-1", at, keep)
            .unwrap();
        assert_eq!(state.heartbeat_at("neutral", "s-1"), Some(at));
        assert_eq!(
            state.heartbeat_at("neutral", "s-2"),
            None,
            "one session's heartbeat is not another's"
        );
        assert_eq!(
            state.heartbeat_at("other", "s-1"),
            None,
            "nor that of another host's session with the same id"
        );
        let later = at + Duration::minutes(1);
        state
            .remember_heartbeat("neutral", "s-2", later, keep)
            .unwrap();
        assert_eq!(state.heartbeat_at("neutral", "s-1"), Some(at));
        assert_eq!(state.heartbeat_at("neutral", "s-2"), Some(later));

        state.forget_heartbeat("neutral", "s-1").unwrap();
        assert_eq!(state.heartbeat_at("neutral", "s-1"), None, "it ended");
        assert_eq!(state.heartbeat_at("neutral", "s-2"), Some(later));

        let newer = later + Duration::minutes(30);
        state
            .remember_heartbeat("neutral", "s-3", newer, keep)
            .unwrap();
        state
            .remember_heartbeat("neutral", "s-4", later + keep, keep)
            .unwrap();
        assert_eq!(
            state.heartbeat_at("neutral", "s-2"),
            None,
            "a moment as old as the time it is kept for is forgotten"
        );
        assert_eq!(state.heartbeat_at("neutral", "s-3"), Some(newer));
        assert_eq!(state.heartbeat_at("neutral", "s-4"), Some(later + keep));
    }

    #[test]
    fn each_session_keeps_when_a_sync_was_last_started_for_it_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let at = OffsetDateTime::UNIX_EPOCH + Duration::hours(10);
        assert_eq!(state.session_sync_started_at("neutral", "s-1"), None);
        state.forget_session_sync_started("neutral", "s-1").unwrap();

        state
            .remember_session_sync_started("neutral", "s-1", at)
            .unwrap();
        state
            .remember_session_sync_started("neutral", "s-2", at + Duration::seconds(1))
            .unwrap();
        assert_eq!(state.session_sync_started_at("neutral", "s-1"), Some(at));
        assert_eq!(
            state.session_sync_started_at("neutral", "s-2"),
            Some(at + Duration::seconds(1)),
            "one session's start is not another's"
        );
        assert_eq!(state.session_sync_started_at("other", "s-1"), None);
        assert_eq!(
            state.sync_started_at(),
            None,
            "nor the start of the sync that does the machine's work"
        );

        state.forget_session_sync_started("neutral", "s-1").unwrap();
        assert_eq!(
            state.session_sync_started_at("neutral", "s-1"),
            None,
            "it ended"
        );
        assert_eq!(
            state.session_sync_started_at("neutral", "s-2"),
            Some(at + Duration::seconds(1))
        );
    }

    #[test]
    fn every_session_id_names_one_delivery_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let state = StateDir::new(dir.path(), &key("acme"));
        let (file, lock) = state.delivery_files("an-agent", "a/../b c:*").unwrap();
        assert!(file.starts_with(dir.path().join("sessions")));
        assert_eq!(file.parent(), lock.parent());
        let (other, _) = state.delivery_files("neutral", "a/../b c:*").unwrap();
        assert_ne!(file, other);
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            name.starts_with("delivery-") && name.ends_with(".json"),
            "{name}"
        );
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-.".contains(c)),
            "{name}"
        );
    }
}
