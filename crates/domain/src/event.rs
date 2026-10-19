//! The published event language: one record per state change, appended to the project's log.
//!
//! The log is read by every member and by integrations, while a conversation is readable only by
//! its author and recipients, so thread and message events carry ids and the author — never a
//! subject or a body. Readers fetch content through reads that apply the visibility rule.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::area::QualifiedArea;
use crate::ids::{EventId, GrantId, ItemId, MemberId, MessageId, ProjectId, SessionId, ThreadId};
use crate::item::{ExternalRef, Title};
use crate::paths::RepoPath;
use crate::session::HostName;

/// One entry of a project's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Its position; later events have larger ids.
    pub id: EventId,
    /// The project.
    pub project: ProjectId,
    /// When it happened.
    pub at: OffsetDateTime,
    /// What happened.
    pub kind: EventKind,
}

/// What happened. Serialised as `{"kind": "<name>", "payload": {…}}`, `<name>` being
/// [`EventKind::name`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum EventKind {
    /// A work item was created.
    ItemCreated(ItemCreated),
    /// A member claimed a free item.
    Claimed(Claimed),
    /// A member claimed an item they already held.
    Renewed(Renewed),
    /// A member took over another member's stale claim.
    TakenOver(TakenOver),
    /// The holder released their claim.
    Released(Released),
    /// The holder completed their claim.
    Completed(Completed),
    /// An admin ended a claim.
    ForceReleased(ForceReleased),
    /// A claim gained areas or shared paths through an edit check. Appended only when something
    /// was actually added.
    Reserved(Reserved),
    /// A holder granted another member edits in an area.
    Granted(Granted),
    /// A grant ended.
    GrantRevoked(GrantRevoked),
    /// A conversation started.
    ThreadStarted(ThreadStarted),
    /// A message was posted.
    MessagePosted(MessagePosted),
    /// The project's settings changed.
    SettingsChanged(SettingsChanged),
}

impl EventKind {
    /// The stable name of the kind, as serialised (`item_created`, `claimed`, …).
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ItemCreated(_) => "item_created",
            Self::Claimed(_) => "claimed",
            Self::Renewed(_) => "renewed",
            Self::TakenOver(_) => "taken_over",
            Self::Released(_) => "released",
            Self::Completed(_) => "completed",
            Self::ForceReleased(_) => "force_released",
            Self::Reserved(_) => "reserved",
            Self::Granted(_) => "granted",
            Self::GrantRevoked(_) => "grant_revoked",
            Self::ThreadStarted(_) => "thread_started",
            Self::MessagePosted(_) => "message_posted",
            Self::SettingsChanged(_) => "settings_changed",
        }
    }
}

/// A work item was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemCreated {
    /// Its local id.
    pub item: ItemId,
    /// Its reference in an outside tracker, when it has one.
    pub external_ref: Option<ExternalRef>,
    /// Its title.
    pub title: Title,
    /// Who created it.
    pub by: MemberId,
}

/// A member claimed a free item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claimed {
    /// The item.
    pub item: ItemId,
    /// The member now holding it.
    pub member: MemberId,
    /// The session that claimed it.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// The areas the claim declares.
    pub areas: BTreeSet<QualifiedArea>,
}

/// A member claimed an item they already held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Renewed {
    /// The item.
    pub item: ItemId,
    /// The member holding it.
    pub member: MemberId,
    /// The session that renewed it.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// The areas the claim declares after the renewal.
    pub areas: BTreeSet<QualifiedArea>,
}

/// A member took over another member's stale claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakenOver {
    /// The item.
    pub item: ItemId,
    /// The member now holding it.
    pub member: MemberId,
    /// The member whose stale claim ended.
    pub previous: MemberId,
    /// The session that took it.
    pub session: SessionId,
    /// That session's host.
    pub host: HostName,
    /// The areas the new claim declares.
    pub areas: BTreeSet<QualifiedArea>,
}

/// The holder released their claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Released {
    /// The item.
    pub item: ItemId,
    /// The member who held it.
    pub member: MemberId,
}

/// The holder completed their claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Completed {
    /// The item.
    pub item: ItemId,
    /// The member who held it.
    pub member: MemberId,
}

/// An admin ended a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForceReleased {
    /// The item.
    pub item: ItemId,
    /// The member who held it.
    pub member: MemberId,
    /// The admin.
    pub by: MemberId,
}

/// A claim gained areas or shared paths through an edit check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reserved {
    /// The item of the claim.
    pub item: ItemId,
    /// Its member.
    pub member: MemberId,
    /// The areas added.
    pub areas: BTreeSet<QualifiedArea>,
    /// The paths recorded as shared.
    pub paths: BTreeSet<RepoPath>,
}

/// A holder granted another member edits in an area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Granted {
    /// The stored grant.
    pub grant: GrantId,
    /// The grantee's work item.
    pub item: ItemId,
    /// The area.
    pub area: QualifiedArea,
    /// The holder who granted.
    pub from: MemberId,
    /// The member who may edit.
    pub to: MemberId,
}

/// Why a grant ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantEnd {
    /// The grantor revoked it.
    Revoked,
    /// The grantor no longer holds the area through a live claim.
    GrantorLeft,
    /// The grantee's claim on the item ended.
    GranteeLeft,
}

/// A grant ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRevoked {
    /// The grant that ended.
    pub grant: GrantId,
    /// The grantee's work item.
    pub item: ItemId,
    /// The area.
    pub area: QualifiedArea,
    /// The holder who had granted.
    pub from: MemberId,
    /// The member who could edit.
    pub to: MemberId,
    /// Why it ended.
    pub cause: GrantEnd,
}

/// A conversation started. Carries no subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadStarted {
    /// The thread.
    pub thread: ThreadId,
    /// Who started it.
    pub author: MemberId,
    /// The work item it is about, if any.
    pub item: Option<ItemId>,
}

/// A message was posted. Carries no body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessagePosted {
    /// The thread.
    pub thread: ThreadId,
    /// The message.
    pub message: MessageId,
    /// Who wrote it.
    pub author: MemberId,
}

/// The project's settings changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChanged {
    /// Who changed them.
    pub by: MemberId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ALICE, BOB, area};

    fn every_kind() -> Vec<EventKind> {
        let item = ItemId(4);
        let host = HostName::parse("laptop").unwrap();
        let areas = BTreeSet::from([area("api:billing"), area("web:checkout")]);
        vec![
            EventKind::ItemCreated(ItemCreated {
                item,
                external_ref: Some(ExternalRef::parse("gh:acme/web#12").unwrap()),
                title: Title::parse("Rounding in invoices").unwrap(),
                by: ALICE,
            }),
            EventKind::Claimed(Claimed {
                item,
                member: ALICE,
                session: SessionId(1),
                host: host.clone(),
                areas: areas.clone(),
            }),
            EventKind::Renewed(Renewed {
                item,
                member: ALICE,
                session: SessionId(2),
                host: host.clone(),
                areas: areas.clone(),
            }),
            EventKind::TakenOver(TakenOver {
                item,
                member: BOB,
                previous: ALICE,
                session: SessionId(3),
                host,
                areas,
            }),
            EventKind::Released(Released { item, member: BOB }),
            EventKind::Completed(Completed { item, member: BOB }),
            EventKind::ForceReleased(ForceReleased {
                item,
                member: BOB,
                by: ALICE,
            }),
            EventKind::Reserved(Reserved {
                item,
                member: BOB,
                areas: BTreeSet::from([area("api:search")]),
                paths: BTreeSet::from([RepoPath::parse("api:config/app.yaml").unwrap()]),
            }),
            EventKind::Granted(Granted {
                grant: GrantId(1),
                item,
                area: area("api:billing"),
                from: ALICE,
                to: BOB,
            }),
            EventKind::GrantRevoked(GrantRevoked {
                grant: GrantId(1),
                item,
                area: area("api:billing"),
                from: ALICE,
                to: BOB,
                cause: GrantEnd::GranteeLeft,
            }),
            EventKind::ThreadStarted(ThreadStarted {
                thread: ThreadId(5),
                author: ALICE,
                item: Some(item),
            }),
            EventKind::MessagePosted(MessagePosted {
                thread: ThreadId(5),
                message: MessageId(6),
                author: ALICE,
            }),
            EventKind::SettingsChanged(SettingsChanged { by: ALICE }),
        ]
    }

    #[test]
    fn every_kind_round_trips_under_its_name() {
        for kind in every_kind() {
            let value = serde_json::to_value(&kind).unwrap();
            assert_eq!(value["kind"], kind.name());
            assert!(value["payload"].is_object(), "{}", kind.name());
            let back: EventKind = serde_json::from_value(value).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn values_serialise_in_their_text_forms() {
        let kinds = every_kind();
        let reserved = serde_json::to_value(&kinds[7]).unwrap();
        assert_eq!(
            reserved["payload"],
            serde_json::json!({
                "item": 4,
                "member": 2,
                "areas": ["api:search"],
                "paths": ["api:config/app.yaml"],
            })
        );
        let created = serde_json::to_value(&kinds[0]).unwrap();
        assert_eq!(created["payload"]["external_ref"], "gh:acme/web#12");
    }

    #[test]
    fn conversation_events_carry_ids_and_author_only() {
        for kind in every_kind() {
            if !matches!(
                kind,
                EventKind::ThreadStarted(_) | EventKind::MessagePosted(_)
            ) {
                continue;
            }
            let value = serde_json::to_value(&kind).unwrap();
            let fields: BTreeSet<String> = value["payload"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            for field in &fields {
                assert!(
                    ["thread", "message", "author", "item"].contains(&field.as_str()),
                    "{} carries {field}",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn an_invalid_value_in_a_payload_is_refused_on_reading() {
        let text = r#"{"kind":"released","payload":{"item":1,"member":2}}"#;
        assert!(serde_json::from_str::<EventKind>(text).is_ok());
        let bad_area =
            r#"{"kind":"reserved","payload":{"item":1,"member":2,"areas":["Api:x"],"paths":[]}}"#;
        assert!(serde_json::from_str::<EventKind>(bad_area).is_err());
    }
}
