//! Members named by handle and items by their reference beside their id, in the results and
//! refusals of the use cases.
//!
//! A use case reads the handles and references in the same transaction that decided, so a result
//! or refusal never names a member or an item as they were in another moment, and its caller opens
//! no second transaction to show who or what is meant.

use std::collections::BTreeMap;
use std::fmt;

use time::OffsetDateTime;

use dibs_domain::claim::{Claim, ClaimRefused, HolderOnly, TakeoverRefused};
use dibs_domain::edit::{PathVerdict, Verdict};
use dibs_domain::event::Event;
use dibs_domain::handle::Handle;
use dibs_domain::ids::{ItemId, MemberId};
use dibs_domain::item::{ExternalRef, Title};
use dibs_domain::message::{Importance, Recipients};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::slot::{BeatRefused, SlotRefused};

use crate::error::AppError;
use crate::ports::StoreError;
use crate::records::{
    GrantRecord, ItemRecord, MessageRecord, ThreadRecord, ThreadSummary, TokenRecord,
};
use crate::work::Work;

/// A claim with its holder's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedClaim {
    /// The claim.
    pub claim: Claim,
    /// The handle of `claim.member`.
    pub holder: Handle,
}

/// A claim that ended, with its holder's handle and the item it was on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedClaim {
    /// The claim, as it was when it ended.
    pub claim: NamedClaim,
    /// The item.
    pub item: ItemName,
}

/// A work item with its creator's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedItem {
    /// The item.
    pub item: ItemRecord,
    /// The handle of `item.created_by`.
    pub created_by: Handle,
}

/// A grant with the handles of its two members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedGrant {
    /// The grant.
    pub record: GrantRecord,
    /// The handle of the holder who granted, `record.grant.from`.
    pub from: Handle,
    /// The handle of the member who may edit, `record.grant.to`.
    pub to: Handle,
}

/// How a result names an item it mentions: its id, its reference and title, and the tracker's page
/// for it when its reference scheme has pages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemName {
    /// The local id.
    pub id: ItemId,
    /// The reference in an outside tracker.
    pub external_ref: Option<ExternalRef>,
    /// The title.
    pub title: Title,
    /// The tracker's page.
    pub link: Option<String>,
}

impl ItemName {
    /// The name of a stored item, its page found through the reference schemes.
    pub(crate) fn of(record: &ItemRecord, schemes: &RefSchemeRegistry) -> Self {
        Self {
            id: record.id,
            external_ref: record.external_ref.clone(),
            title: record.title.clone(),
            link: record
                .external_ref
                .as_ref()
                .and_then(|reference| schemes.link(reference)),
        }
    }
}

/// One path's verdict with the handles of the members it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedVerdict {
    /// The path and its verdict.
    pub verdict: PathVerdict,
    /// The handle of the member the verdict names — the holder of [`Verdict::HeldBy`], the grantor
    /// of [`Verdict::Granted`] — or `None` for a verdict that names nobody.
    pub member: Option<Handle>,
    /// The reference of the holder's item for [`Verdict::HeldBy`], when that item has one.
    pub item_ref: Option<ExternalRef>,
    /// For [`Verdict::AllowedWithNote`], the handles of its `touched_by`, in the same order; empty
    /// for every other verdict.
    pub touched_by: Vec<Handle>,
    /// The references of the items of `touched_by`, in the same order: `None` for an item without
    /// one.
    pub touched_refs: Vec<Option<ExternalRef>>,
}

/// A conversation with the handles of its author and recipients.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedThread {
    /// The thread.
    pub record: ThreadRecord,
    /// The handle of `record.author`.
    pub author: Handle,
    /// The handles of the members `record.recipients` lists, in its order; empty for a thread
    /// addressed to the whole project.
    pub recipients: Vec<Handle>,
}

/// A message with its author's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedMessage {
    /// The message.
    pub record: MessageRecord,
    /// The handle of `record.author`.
    pub author: Handle,
}

/// A thread with its counts as one reader sees them, its members named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedSummary {
    /// The thread.
    pub thread: NamedThread,
    /// How many messages it has.
    pub messages: u64,
    /// How many of them the reader has not read and did not write.
    pub unread: u64,
    /// When its newest message was written.
    pub last_message_at: Option<OffsetDateTime>,
    /// The highest importance among its messages.
    pub importance: Importance,
    /// How many of its messages ask the reader for an acknowledgement they have not given.
    pub awaiting_ack: u64,
}

/// An event of the log with the handles of the members it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedEvent {
    /// The event.
    pub event: Event,
    /// The handle of every member `event.kind` names, by id.
    pub members: BTreeMap<MemberId, Handle>,
}

/// A token with its member's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedToken {
    /// The token.
    pub record: TokenRecord,
    /// The handle of `record.member`.
    pub member: Handle,
}

/// A refusal of the domain with the handle of the member it names and the reference of the item
/// it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal<R> {
    /// Why.
    pub reason: R,
    /// The handle of the member [`NamesMember::member`] gives for `reason`; `None` exactly when the
    /// reason names nobody.
    pub member: Option<Handle>,
    /// The reference of the item [`NamesMember::item`] gives for `reason`, when the reason names an
    /// item and that item has a reference. Boxed, so a refusal stays small enough to travel in
    /// every result.
    pub item_ref: Option<Box<ExternalRef>>,
}

impl<R: fmt::Display> fmt::Display for Refusal<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(f)
    }
}

impl<R: fmt::Display + fmt::Debug> std::error::Error for Refusal<R> {}

/// A refusal that may name a member — the holder in the way — and the item they hold.
pub trait NamesMember {
    /// The member the refusal names, if it names one.
    fn member(&self) -> Option<MemberId>;

    /// The item of the member in the way, if the refusal names one.
    fn item(&self) -> Option<ItemId>;
}

impl NamesMember for ClaimRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::HeldBy { member, .. }
            | Self::Stale { member, .. }
            | Self::AreaHeld { member, .. }
            | Self::WideOverlap { member, .. } => Some(*member),
            Self::UnknownArea { .. } | Self::WideAndHeld { .. } => None,
        }
    }

    fn item(&self) -> Option<ItemId> {
        match self {
            Self::AreaHeld { item, .. } | Self::WideOverlap { item, .. } => Some(*item),
            Self::HeldBy { .. }
            | Self::Stale { .. }
            | Self::UnknownArea { .. }
            | Self::WideAndHeld { .. } => None,
        }
    }
}

impl NamesMember for TakeoverRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::NotStale { member, .. }
            | Self::AreaHeld { member, .. }
            | Self::WideOverlap { member, .. } => Some(*member),
            Self::NothingToTake
            | Self::OwnClaim
            | Self::UnknownArea { .. }
            | Self::WideAndHeld { .. } => None,
        }
    }

    fn item(&self) -> Option<ItemId> {
        match self {
            Self::AreaHeld { item, .. } | Self::WideOverlap { item, .. } => Some(*item),
            Self::NotStale { .. }
            | Self::NothingToTake
            | Self::OwnClaim
            | Self::UnknownArea { .. }
            | Self::WideAndHeld { .. } => None,
        }
    }
}

impl NamesMember for HolderOnly {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::NotHolder { member } => Some(*member),
            Self::NoClaim => None,
        }
    }

    fn item(&self) -> Option<ItemId> {
        None
    }
}

impl NamesMember for SlotRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::NotHolder { member } | Self::NotStalled { member, .. } => Some(*member),
            Self::UnknownSlot { .. }
            | Self::HoldsOther { .. }
            | Self::NotHeld
            | Self::RunEnded
            | Self::NotWaiting
            | Self::OwnHold => None,
        }
    }

    fn item(&self) -> Option<ItemId> {
        None
    }
}

impl NamesMember for BeatRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::TakenOver { by } => Some(*by),
            Self::Ended => None,
        }
    }

    fn item(&self) -> Option<ItemId> {
        None
    }
}

/// The handles of a project's members, read inside one transaction.
pub(crate) struct Names {
    handles: BTreeMap<MemberId, Handle>,
}

impl Names {
    /// Reads every member of the transaction's project.
    pub(crate) async fn load(work: &mut Work) -> Result<Self, AppError> {
        let project = work.project_id();
        let handles = work
            .members(project)
            .await?
            .into_iter()
            .map(|member| (member.id, member.handle))
            .collect();
        Ok(Self { handles })
    }

    /// The handle of a member a record names.
    ///
    /// # Errors
    /// [`AppError::Store`] when the project has no such member: the record refers to nobody.
    pub(crate) fn handle(&self, member: MemberId) -> Result<Handle, AppError> {
        self.handles.get(&member).cloned().ok_or_else(|| {
            AppError::Store(StoreError::Corrupt {
                what: format!("a record names member {member}, who does not exist"),
            })
        })
    }

    /// The member of a handle, if the project has one.
    pub(crate) fn member(&self, handle: &Handle) -> Option<MemberId> {
        self.handles
            .iter()
            .find(|(_, known)| *known == handle)
            .map(|(id, _)| *id)
    }

    /// The claim with its holder's handle.
    pub(crate) fn claim(&self, claim: Claim) -> Result<NamedClaim, AppError> {
        Ok(NamedClaim {
            holder: self.handle(claim.member)?,
            claim,
        })
    }

    /// The item with its creator's handle.
    pub(crate) fn item(&self, item: ItemRecord) -> Result<NamedItem, AppError> {
        Ok(NamedItem {
            created_by: self.handle(item.created_by)?,
            item,
        })
    }

    /// The grant with the handles of its two members.
    pub(crate) fn grant(&self, record: GrantRecord) -> Result<NamedGrant, AppError> {
        Ok(NamedGrant {
            from: self.handle(record.grant.from)?,
            to: self.handle(record.grant.to)?,
            record,
        })
    }

    /// The thread with the handles of its author and recipients.
    pub(crate) fn thread(&self, record: ThreadRecord) -> Result<NamedThread, AppError> {
        let recipients = match &record.recipients {
            Recipients::Project => Vec::new(),
            Recipients::Members(members) => members
                .iter()
                .map(|member| self.handle(*member))
                .collect::<Result<_, _>>()?,
        };
        Ok(NamedThread {
            author: self.handle(record.author)?,
            recipients,
            record,
        })
    }

    /// The message with its author's handle.
    pub(crate) fn message(&self, record: MessageRecord) -> Result<NamedMessage, AppError> {
        Ok(NamedMessage {
            author: self.handle(record.author)?,
            record,
        })
    }

    /// The summary with the handles of its thread's members, and how many of its messages await
    /// the reader's acknowledgement.
    pub(crate) fn summary(
        &self,
        summary: ThreadSummary,
        awaiting_ack: u64,
    ) -> Result<NamedSummary, AppError> {
        Ok(NamedSummary {
            thread: self.thread(summary.thread)?,
            messages: summary.messages,
            unread: summary.unread,
            last_message_at: summary.last_message_at,
            importance: summary.importance,
            awaiting_ack,
        })
    }

    /// The event with the handles of the members it names.
    pub(crate) fn event(&self, event: Event) -> Result<NamedEvent, AppError> {
        let members = event
            .kind
            .members()
            .into_iter()
            .map(|member| Ok((member, self.handle(member)?)))
            .collect::<Result<_, AppError>>()?;
        Ok(NamedEvent { event, members })
    }

    /// The token with its member's handle.
    pub(crate) fn token(&self, record: TokenRecord) -> Result<NamedToken, AppError> {
        Ok(NamedToken {
            member: self.handle(record.member)?,
            record,
        })
    }

    /// The verdict with the handles of the members it names; `item_ref` is the reference of the
    /// item it names and `touched_refs` those of the items its `touched_by` names, read by the
    /// caller.
    pub(crate) fn verdict(
        &self,
        verdict: PathVerdict,
        item_ref: Option<ExternalRef>,
        touched_refs: Vec<Option<ExternalRef>>,
    ) -> Result<NamedVerdict, AppError> {
        let (member, touched_by) = match &verdict.verdict {
            Verdict::HeldBy { member, .. } => (Some(self.handle(*member)?), Vec::new()),
            Verdict::Granted { by, .. } => (Some(self.handle(*by)?), Vec::new()),
            Verdict::AllowedWithNote { touched_by, .. } => (
                None,
                touched_by
                    .iter()
                    .map(|holder| self.handle(holder.member))
                    .collect::<Result<_, _>>()?,
            ),
            Verdict::Allowed | Verdict::NeedsClaim => (None, Vec::new()),
        };
        Ok(NamedVerdict {
            verdict,
            member,
            item_ref,
            touched_by,
            touched_refs,
        })
    }
}

/// The reference of the item of this id in the transaction's project, when it has one.
pub(crate) async fn item_ref(
    work: &mut Work,
    id: ItemId,
) -> Result<Option<ExternalRef>, StoreError> {
    let project = work.project_id();
    Ok(work
        .item(project, id)
        .await?
        .and_then(|item| item.external_ref))
}

/// The refusal as an [`AppError`] carrying the handle of the member and the reference of the item
/// it names, read in the transaction that refused; [`AppError::Store`] instead when they cannot be
/// read.
pub(crate) async fn refuse<R>(work: &mut Work, reason: R) -> AppError
where
    R: NamesMember,
    AppError: From<Refusal<R>>,
{
    let item_ref = match reason.item() {
        None => None,
        Some(id) => match item_ref(work, id).await {
            Ok(reference) => reference.map(Box::new),
            Err(error) => return AppError::Store(error),
        },
    };
    let member = match reason.member() {
        None => None,
        Some(id) => {
            let project = work.project_id();
            match work.member(project, id).await {
                Ok(Some(member)) => Some(member.handle),
                Ok(None) => {
                    return AppError::Store(StoreError::Corrupt {
                        what: format!("a claim names member {id}, who does not exist"),
                    });
                }
                Err(error) => return AppError::Store(error),
            }
        }
    };
    AppError::from(Refusal {
        reason,
        member,
        item_ref,
    })
}

#[cfg(test)]
mod tests {
    use time::OffsetDateTime;

    use dibs_domain::area::QualifiedArea;
    use dibs_domain::ids::ItemId;

    use super::*;

    #[test]
    fn each_refusal_names_the_member_in_the_way() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let area = QualifiedArea::parse("api:billing").unwrap();
        let named: Vec<Option<MemberId>> = vec![
            ClaimRefused::HeldBy {
                member: MemberId(1),
                since: at,
                last_seen: at,
            }
            .member(),
            ClaimRefused::Stale {
                member: MemberId(2),
                last_seen: at,
            }
            .member(),
            ClaimRefused::AreaHeld {
                area: area.clone(),
                member: MemberId(3),
                item: ItemId(9),
            }
            .member(),
            ClaimRefused::UnknownArea { area: area.clone() }.member(),
            TakeoverRefused::NotStale {
                member: MemberId(4),
                last_seen: at,
            }
            .member(),
            TakeoverRefused::AreaHeld {
                area,
                member: MemberId(5),
                item: ItemId(9),
            }
            .member(),
            TakeoverRefused::NothingToTake.member(),
            TakeoverRefused::OwnClaim.member(),
            HolderOnly::NotHolder {
                member: MemberId(6),
            }
            .member(),
            HolderOnly::NoClaim.member(),
        ];
        let expected = [
            Some(1),
            Some(2),
            Some(3),
            None,
            Some(4),
            Some(5),
            None,
            None,
            Some(6),
            None,
        ];
        assert_eq!(
            named,
            expected.map(|n| n.map(MemberId)).to_vec(),
            "the members each refusal names"
        );
    }

    #[test]
    fn a_held_area_names_the_holders_item_and_nothing_else_does() {
        let at = OffsetDateTime::UNIX_EPOCH;
        let area = QualifiedArea::parse("api:billing").unwrap();
        assert_eq!(
            ClaimRefused::AreaHeld {
                area: area.clone(),
                member: MemberId(3),
                item: ItemId(9),
            }
            .item(),
            Some(ItemId(9))
        );
        assert_eq!(
            TakeoverRefused::AreaHeld {
                area: area.clone(),
                member: MemberId(3),
                item: ItemId(8),
            }
            .item(),
            Some(ItemId(8))
        );
        assert_eq!(
            ClaimRefused::HeldBy {
                member: MemberId(1),
                since: at,
                last_seen: at,
            }
            .item(),
            None
        );
        assert_eq!(ClaimRefused::UnknownArea { area }.item(), None);
        assert_eq!(TakeoverRefused::OwnClaim.item(), None);
        assert_eq!(HolderOnly::NoClaim.item(), None);
    }

    #[test]
    fn a_refusal_reads_as_its_reason() {
        let refusal = Refusal {
            reason: HolderOnly::NoClaim,
            member: None,
            item_ref: None,
        };
        assert_eq!(refusal.to_string(), HolderOnly::NoClaim.to_string());
    }
}
