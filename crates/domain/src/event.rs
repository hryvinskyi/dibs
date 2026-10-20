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
    /// A work item's facts changed: its title, hint, priority, tracker state, source or whether its
    /// tracker closed it. Appended only when one of them actually changed.
    ItemUpdated(ItemUpdated),
    /// A work item became blocked by another.
    DependencyAdded(DependencyAdded),
    /// A work item stopped being blocked by another.
    DependencyRemoved(DependencyRemoved),
    /// A recipient acknowledged a message.
    Acked(Acked),
    /// A claim became wide, or its wide scope grew: its member announces a change over these
    /// areas, holding none of them.
    WideClaimed(WideClaimed),
    /// A holder acknowledged a wide claim's announcement, and that granted the wide claim's member
    /// edits in one of their areas.
    WideGrantedByAck(WideGrantedByAck),
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
            Self::ItemUpdated(_) => "item_updated",
            Self::DependencyAdded(_) => "dependency_added",
            Self::DependencyRemoved(_) => "dependency_removed",
            Self::Acked(_) => "acked",
            Self::WideClaimed(_) => "wide_claimed",
            Self::WideGrantedByAck(_) => "wide_granted_by_ack",
        }
    }

    /// Every member the event names.
    pub fn members(&self) -> BTreeSet<MemberId> {
        let named: &[MemberId] = match self {
            Self::ItemCreated(e) => &[e.by],
            Self::Claimed(e) => &[e.member],
            Self::Renewed(e) => &[e.member],
            Self::TakenOver(e) => &[e.member, e.previous],
            Self::Released(e) => &[e.member],
            Self::Completed(e) => &[e.member],
            Self::ForceReleased(e) => &[e.member, e.by],
            Self::Reserved(e) => &[e.member],
            Self::Granted(e) => &[e.from, e.to],
            Self::GrantRevoked(e) => &[e.from, e.to],
            Self::ThreadStarted(e) => &[e.author],
            Self::MessagePosted(e) => &[e.author],
            Self::SettingsChanged(e) => &[e.by],
            Self::ItemUpdated(e) => &[e.by],
            Self::DependencyAdded(e) => &[e.by],
            Self::DependencyRemoved(e) => &[e.by],
            Self::Acked(e) => &[e.by],
            Self::WideClaimed(e) => &[e.member],
            Self::WideGrantedByAck(e) => &[e.by, e.to],
        };
        named.iter().copied().collect()
    }

    /// The message the event announces, for the kind that announces one.
    pub const fn message(&self) -> Option<MessageId> {
        match self {
            Self::MessagePosted(e) => Some(e.message),
            Self::ItemCreated(_)
            | Self::Claimed(_)
            | Self::Renewed(_)
            | Self::TakenOver(_)
            | Self::Released(_)
            | Self::Completed(_)
            | Self::ForceReleased(_)
            | Self::Reserved(_)
            | Self::Granted(_)
            | Self::GrantRevoked(_)
            | Self::ThreadStarted(_)
            | Self::SettingsChanged(_)
            | Self::ItemUpdated(_)
            | Self::DependencyAdded(_)
            | Self::DependencyRemoved(_)
            | Self::Acked(_)
            | Self::WideClaimed(_)
            | Self::WideGrantedByAck(_) => None,
        }
    }

    /// The conversation the event is about, for the kinds that are about one.
    pub const fn thread(&self) -> Option<ThreadId> {
        match self {
            Self::ThreadStarted(e) => Some(e.thread),
            Self::MessagePosted(e) => Some(e.thread),
            Self::Acked(e) => Some(e.thread),
            Self::ItemCreated(_)
            | Self::Claimed(_)
            | Self::Renewed(_)
            | Self::TakenOver(_)
            | Self::Released(_)
            | Self::Completed(_)
            | Self::ForceReleased(_)
            | Self::Reserved(_)
            | Self::Granted(_)
            | Self::GrantRevoked(_)
            | Self::SettingsChanged(_)
            | Self::ItemUpdated(_)
            | Self::DependencyAdded(_)
            | Self::DependencyRemoved(_)
            | Self::WideClaimed(_)
            | Self::WideGrantedByAck(_) => None,
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

/// A work item's facts changed. Carries the ids only; the item is read for what it says now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemUpdated {
    /// The item.
    pub item: ItemId,
    /// Who changed it.
    pub by: MemberId,
}

/// A work item became blocked by another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyAdded {
    /// The item that waits.
    pub item: ItemId,
    /// The item it waits for.
    pub blocked_by: ItemId,
    /// Who recorded it.
    pub by: MemberId,
}

/// A work item stopped being blocked by another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyRemoved {
    /// The item that waited.
    pub item: ItemId,
    /// The item it waited for.
    pub blocked_by: ItemId,
    /// Who removed it.
    pub by: MemberId,
}

/// A claim became wide, or its wide scope grew. Carries no reason; the claim is read for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideClaimed {
    /// The item of the claim.
    pub item: ItemId,
    /// Its member.
    pub member: MemberId,
    /// Every area the wide scope names now.
    pub areas: BTreeSet<QualifiedArea>,
}

/// A holder's acknowledgement of a wide claim's announcement granted edits in one area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WideGrantedByAck {
    /// The stored grant; it ends like any grant, with a `grant_revoked` naming it.
    pub grant: GrantId,
    /// The wide claim's item.
    pub item: ItemId,
    /// The area.
    pub area: QualifiedArea,
    /// The holder who acknowledged.
    pub by: MemberId,
    /// The wide claim's member, who may edit.
    pub to: MemberId,
}

/// A recipient acknowledged a message. Carries the ids and the member only, like the message
/// events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Acked {
    /// The thread of the message.
    pub thread: ThreadId,
    /// The message.
    pub message: MessageId,
    /// The recipient who acknowledged it.
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
            EventKind::ItemUpdated(ItemUpdated { item, by: BOB }),
            EventKind::DependencyAdded(DependencyAdded {
                item,
                blocked_by: ItemId(5),
                by: BOB,
            }),
            EventKind::DependencyRemoved(DependencyRemoved {
                item,
                blocked_by: ItemId(5),
                by: ALICE,
            }),
            EventKind::Acked(Acked {
                thread: ThreadId(5),
                message: MessageId(6),
                by: BOB,
            }),
            EventKind::WideClaimed(WideClaimed {
                item,
                member: ALICE,
                areas: BTreeSet::from([area("api:billing"), area("web:checkout")]),
            }),
            EventKind::WideGrantedByAck(WideGrantedByAck {
                grant: GrantId(2),
                item,
                area: area("api:billing"),
                by: BOB,
                to: ALICE,
            }),
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
    fn item_and_dependency_changes_carry_ids_and_author_only() {
        for kind in every_kind() {
            let allowed: &[&str] = match kind {
                EventKind::ItemUpdated(_) => &["item", "by"],
                EventKind::DependencyAdded(_) | EventKind::DependencyRemoved(_) => {
                    &["item", "blocked_by", "by"]
                }
                _ => continue,
            };
            let value = serde_json::to_value(&kind).unwrap();
            let fields: Vec<String> = value["payload"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            for field in &fields {
                assert!(
                    allowed.contains(&field.as_str()),
                    "{} carries {field}",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn conversation_events_carry_ids_and_author_only() {
        for kind in every_kind() {
            if !matches!(
                kind,
                EventKind::ThreadStarted(_) | EventKind::MessagePosted(_) | EventKind::Acked(_)
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
                    ["thread", "message", "author", "item", "by"].contains(&field.as_str()),
                    "{} carries {field}",
                    kind.name()
                );
            }
        }
    }

    fn both_again() -> BTreeSet<MemberId> {
        BTreeSet::from([ALICE, BOB])
    }

    #[test]
    fn wide_claim_events_carry_ids_areas_and_members_only() {
        for kind in every_kind() {
            let allowed: &[&str] = match kind {
                EventKind::WideClaimed(_) => &["item", "member", "areas"],
                EventKind::WideGrantedByAck(_) => &["grant", "item", "area", "by", "to"],
                _ => continue,
            };
            let value = serde_json::to_value(&kind).unwrap();
            let fields: Vec<String> = value["payload"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect();
            assert_eq!(fields.len(), allowed.len(), "{}", kind.name());
            for field in &fields {
                assert!(
                    allowed.contains(&field.as_str()),
                    "{} carries {field}",
                    kind.name()
                );
            }
        }
    }

    #[test]
    fn each_kind_names_its_members_and_its_thread() {
        let named: Vec<(BTreeSet<MemberId>, Option<ThreadId>)> = every_kind()
            .iter()
            .map(|kind| (kind.members(), kind.thread()))
            .collect();
        let messages: Vec<Option<MessageId>> =
            every_kind().iter().map(EventKind::message).collect();
        let mut expected = vec![None; 19];
        expected[11] = Some(MessageId(6));
        assert_eq!(messages, expected);
        let alice = BTreeSet::from([ALICE]);
        let bob = BTreeSet::from([BOB]);
        let both = BTreeSet::from([ALICE, BOB]);
        assert_eq!(
            named,
            [
                (alice.clone(), None),
                (alice.clone(), None),
                (alice.clone(), None),
                (both.clone(), None),
                (bob.clone(), None),
                (bob.clone(), None),
                (both.clone(), None),
                (bob, None),
                (both.clone(), None),
                (both, None),
                (alice.clone(), Some(ThreadId(5))),
                (alice.clone(), Some(ThreadId(5))),
                (alice.clone(), None),
                (BTreeSet::from([BOB]), None),
                (BTreeSet::from([BOB]), None),
                (alice.clone(), None),
                (BTreeSet::from([BOB]), Some(ThreadId(5))),
                (alice, None),
                (both_again(), None),
            ]
        );
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
