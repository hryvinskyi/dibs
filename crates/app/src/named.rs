//! Members named by handle beside their id, in the results and refusals of the use cases.
//!
//! A use case reads the handles in the same transaction that decided, so a result or refusal never
//! names a member as they were in another moment, and its caller opens no second transaction to
//! show who is meant.

use std::collections::BTreeMap;
use std::fmt;

use dibs_domain::claim::{Claim, ClaimRefused, HolderOnly, TakeoverRefused};
use dibs_domain::edit::{PathVerdict, Verdict};
use dibs_domain::handle::Handle;
use dibs_domain::ids::MemberId;

use crate::error::AppError;
use crate::ports::StoreError;
use crate::records::{GrantRecord, ItemRecord};
use crate::work::Work;

/// A claim with its holder's handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedClaim {
    /// The claim.
    pub claim: Claim,
    /// The handle of `claim.member`.
    pub holder: Handle,
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

/// One path's verdict with the handles of the members it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedVerdict {
    /// The path and its verdict.
    pub verdict: PathVerdict,
    /// The handle of the member the verdict names — the holder of [`Verdict::HeldBy`], the grantor
    /// of [`Verdict::Granted`] — or `None` for a verdict that names nobody.
    pub member: Option<Handle>,
    /// For [`Verdict::AllowedWithNote`], the handles of its `touched_by`, in the same order; empty
    /// for every other verdict.
    pub touched_by: Vec<Handle>,
}

/// A refusal of the domain with the handle of the member it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal<R> {
    /// Why.
    pub reason: R,
    /// The handle of the member [`NamesMember::member`] gives for `reason`; `None` exactly when the
    /// reason names nobody.
    pub member: Option<Handle>,
}

impl<R: fmt::Display> fmt::Display for Refusal<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.reason.fmt(f)
    }
}

impl<R: fmt::Display + fmt::Debug> std::error::Error for Refusal<R> {}

/// A refusal that may name a member: the holder in the way.
pub trait NamesMember {
    /// The member the refusal names, if it names one.
    fn member(&self) -> Option<MemberId>;
}

impl NamesMember for ClaimRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::HeldBy { member, .. }
            | Self::Stale { member, .. }
            | Self::AreaHeld { member, .. } => Some(*member),
            Self::UnknownArea { .. } => None,
        }
    }
}

impl NamesMember for TakeoverRefused {
    fn member(&self) -> Option<MemberId> {
        match self {
            Self::NotStale { member, .. } | Self::AreaHeld { member, .. } => Some(*member),
            Self::NothingToTake | Self::OwnClaim | Self::UnknownArea { .. } => None,
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

    /// The verdict with the handles of the members it names.
    pub(crate) fn verdict(&self, verdict: PathVerdict) -> Result<NamedVerdict, AppError> {
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
            touched_by,
        })
    }
}

/// The refusal as an [`AppError`] carrying the handle of the member it names, read in the
/// transaction that refused; [`AppError::Store`] instead when that handle cannot be read.
pub(crate) async fn refuse<R>(work: &mut Work, reason: R) -> AppError
where
    R: NamesMember,
    AppError: From<Refusal<R>>,
{
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
    AppError::from(Refusal { reason, member })
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
    fn a_refusal_reads_as_its_reason() {
        let refusal = Refusal {
            reason: HolderOnly::NoClaim,
            member: None,
        };
        assert_eq!(refusal.to_string(), HolderOnly::NoClaim.to_string());
    }
}
