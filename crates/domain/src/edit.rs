//! The edit check: for each path a member is about to change, may they, and what is recorded.

use std::collections::BTreeSet;

use thiserror::Error;
use time::OffsetDateTime;

use crate::area::QualifiedArea;
use crate::claim::{Claim, Holder, held_by_other, target_claim};
use crate::grant::Grant;
use crate::hot_rule::{HotContext, HotDecision, HotRuleKind, Note};
use crate::ids::{ItemId, MemberId};
use crate::paths::RepoPath;
use crate::repo::RepoId;
use crate::reservation::{AddArea, ReservationPlan, SharedPath};
use crate::review::PrRef;
use crate::settings::{EditPolicy, ProjectSettings};

/// The answer for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The member may edit.
    Allowed,
    /// The member may edit; a hot-file rule has something to say.
    AllowedWithNote {
        /// The kind of the rule that matched.
        rule: HotRuleKind,
        /// The rule's note.
        note: Option<Note>,
        /// Other members' live claims that edited the same file, for rules that track it.
        touched_by: Vec<Holder>,
    },
    /// The path is governed, the policy requires a claim, and the member holds no live claim.
    NeedsClaim,
    /// Another member's live claim holds the path's area.
    HeldBy {
        /// The holder.
        member: MemberId,
        /// The item of the holder's claim.
        item: ItemId,
        /// The area.
        area: QualifiedArea,
        /// The way on.
        reason: HeldReason,
    },
    /// Another member holds the area and granted this member edits in it.
    Granted {
        /// The holder who granted.
        by: MemberId,
        /// The area.
        area: QualifiedArea,
    },
}

/// Why a held path is held, which also says what the member can do about it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HeldReason {
    /// The holder's claim is in review: the area is free once its pull requests are merged.
    InReview {
        /// The pull requests the holder's claim recorded, in order.
        prs: Vec<PrRef>,
    },
    /// The holder works in the area; message them or ask for a grant. When another member's claim
    /// already waits for the holder to hand the area over, `waiting` names it.
    AskHolder {
        /// The claim that already waits for the area, when one does.
        waiting: Option<Holder>,
    },
    /// The member's live wide claim names the area and its holder has not acknowledged the
    /// announcement yet; their acknowledgement grants the edit.
    AwaitingAck,
    /// The member's feature claim declared the area and waits for the holder's debt claim to hand
    /// it over.
    AwaitingHandover {
        /// When the request goes to the arbiters if the area is not handed over by then; `None`
        /// before the holder was asked and after it was escalated.
        escalates_at: Option<OffsetDateTime>,
    },
    /// The holder's claim is debt work and the member's live feature claim did not declare the
    /// area: declaring it on that claim asks the holder to hand it over.
    AskHandover {
        /// The item of the member's feature claim to declare it on.
        claim: ItemId,
    },
}

impl HeldReason {
    /// The stable name: `in_review`, `ask_holder`, `awaiting_ack`, `awaiting_handover` or
    /// `ask_handover`.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InReview { .. } => "in_review",
            Self::AskHolder { .. } => "ask_holder",
            Self::AwaitingAck => "awaiting_ack",
            Self::AwaitingHandover { .. } => "awaiting_handover",
            Self::AskHandover { .. } => "ask_handover",
        }
    }
}

/// The verdict for one path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathVerdict {
    /// The path.
    pub path: RepoPath,
    /// The verdict.
    pub verdict: Verdict,
}

/// The result of an edit check.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditCheck {
    /// One verdict per path, in the order asked.
    pub verdicts: Vec<PathVerdict>,
    /// What the caller may record so later checks and other members see these edits.
    pub plan: ReservationPlan,
    /// The member's claims the edits were attributed to. An edit is a sign of life, so the caller
    /// refreshes their `last_seen`, wherever the claim was taken.
    pub touch: BTreeSet<ItemId>,
}

/// What an edit check is decided against.
#[derive(Debug, Clone, Copy)]
pub struct EditCheckInput<'a> {
    /// The member about to edit. Every live claim of theirs counts, from any session or host.
    pub me: MemberId,
    /// The project's claims; stale ones are ignored.
    pub claims: &'a [Claim],
    /// The project's grants; those no longer in force are ignored.
    pub grants: &'a [Grant],
    /// The project's shared-path records.
    pub shared: &'a [SharedPath],
    /// The project's settings.
    pub settings: &'a ProjectSettings,
    /// The time of the check.
    pub now: OffsetDateTime,
}

/// Why an edit check cannot be answered.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditCheckRefused {
    /// A path names a repository the settings do not list.
    #[error("the project lists no repository '{repo}'")]
    UnknownRepo {
        /// The repository.
        repo: RepoId,
    },
}

/// Decides each path, in this order:
///
/// 1. The path's repository; one the settings do not list refuses the whole check.
/// 2. The first hot-file rule of that repository, in declaration order, whose globs match. An
///    `owner` rule continues at step 3 for its area; every other kind decides here.
/// 3. The first area of that repository, in declaration order, containing the path. None:
///    `Allowed`.
/// 4. Another member's live claim holds the area, working or in review: `Granted` when that holder
///    granted me edits in it for one of my live claims — by a grant, or by acknowledging my wide
///    claim's announcement — else `HeldBy`, its reason the way on, the first that applies: free
///    once its pull requests merge when the holder's claim is in review; waiting for that
///    acknowledgement when my live wide claim names the area; waiting for the holder to hand it
///    over when my live claim has it pending; asking the holder, naming the claim, when another
///    member's claim already waits for it; declaring it on my live feature claim when the
///    holder's claim is debt work; else asking the holder.
/// 5. Nobody else holds it: `NeedsClaim` when I hold no live claim and the policy requires one,
///    else `Allowed`.
///
/// Allowed edits in an area none of my claims holds and none of my live wide claims names add the
/// area to my target claim for it: of my live claims I am working on that do not wait for the area,
/// the one most recently claimed or renewed, the lowest item on a tie; with none, my claims in review
/// by the same rule. An area another member's claim waits for is added to none of mine, since it is
/// on its way to that claim. An edit inside my wide scope adds nothing, since a wide claim holds
/// nothing it names, and is attributed to that wide claim. Edits under a `warn-shared` rule record
/// the path under the target claim. Nothing already recorded is planned again.
///
/// # Errors
/// [`EditCheckRefused::UnknownRepo`] when a path's repository is not listed.
pub fn edit_verdicts(
    paths: &[RepoPath],
    input: &EditCheckInput<'_>,
) -> Result<EditCheck, EditCheckRefused> {
    if let Some(path) = paths
        .iter()
        .find(|p| input.settings.repo(&p.repo).is_none())
    {
        return Err(EditCheckRefused::UnknownRepo {
            repo: path.repo.clone(),
        });
    }
    let world = EditWorld::new(input);
    let mut check = EditCheck::default();
    for path in paths {
        let verdict = world.decide(path, &mut check);
        check.verdicts.push(PathVerdict {
            path: path.clone(),
            verdict,
        });
    }
    Ok(check)
}

/// The facts one edit check decides against, with the member's own live claims picked out.
pub(crate) struct EditWorld<'a> {
    input: &'a EditCheckInput<'a>,
    mine: Vec<&'a Claim>,
}

impl<'a> EditWorld<'a> {
    fn new(input: &'a EditCheckInput<'a>) -> Self {
        let stale_after = input.settings.stale_after();
        let mine = input
            .claims
            .iter()
            .filter(|claim| claim.member == input.me && claim.is_live(input.now, stale_after))
            .collect();
        Self { input, mine }
    }

    fn decide(&self, path: &RepoPath, check: &mut EditCheck) -> Verdict {
        let Some(repo) = self.input.settings.repo(&path.repo) else {
            return Verdict::Allowed;
        };
        if let Some(spec) = repo.hot_rule_for(&path.path) {
            return match spec.decide(&HotContext::new(spec, path, self)) {
                HotDecision::Decide(verdict) => verdict,
                HotDecision::Share(verdict) => {
                    self.share(path, check);
                    verdict
                }
                HotDecision::AsArea(area) => self.decide_area(&area, check),
            };
        }
        match repo.area_for(&path.path) {
            Some(area) => self.decide_area(&repo.qualify(&area.name), check),
            None => Verdict::Allowed,
        }
    }

    fn decide_area(&self, area: &QualifiedArea, check: &mut EditCheck) -> Verdict {
        let input = self.input;
        let stale_after = input.settings.stale_after();
        if let Some(holder) = held_by_other(area, input.me, input.claims, input.now, stale_after) {
            let grant = input.grants.iter().find(|grant| {
                grant.from == holder.member
                    && grant.to == input.me
                    && grant.area == *area
                    && grant.in_force(input.claims, input.now, stale_after)
            });
            return match grant {
                Some(grant) => {
                    check.touch.insert(grant.item);
                    Verdict::Granted {
                        by: holder.member,
                        area: area.clone(),
                    }
                }
                None => Verdict::HeldBy {
                    member: holder.member,
                    item: holder.item,
                    area: area.clone(),
                    reason: self.held_reason(holder, area),
                },
            };
        }
        if self.mine.is_empty() {
            return match input.settings.edit_policy() {
                EditPolicy::RequireClaim => Verdict::NeedsClaim,
                EditPolicy::Advise => Verdict::Allowed,
            };
        }
        let holding = target_claim(self.mine.iter().copied().filter(|claim| claim.holds(area)));
        let wide = target_claim(
            self.mine
                .iter()
                .copied()
                .filter(|claim| claim.names_wide(area)),
        );
        let waited_for_elsewhere = input
            .claims
            .iter()
            .any(|claim| claim.member != input.me && claim.waits_for(area));
        if let Some(claim) = holding.or(wide) {
            check.touch.insert(claim.item);
        } else if let Some(target) = self.target_for(area) {
            if !waited_for_elsewhere {
                check.plan.add_area(AddArea {
                    claim: target.item,
                    area: area.clone(),
                });
            }
            check.touch.insert(target.item);
        }
        Verdict::Allowed
    }

    /// The way on from an area another member's claim holds: its pull requests' merge when that
    /// claim is in review, their acknowledgement when one of my live wide claims names it, their
    /// handover when one of my live claims waits for it, asking them when another member's claim
    /// already waits for it, declaring it on my feature claim when theirs is debt work, else
    /// asking them.
    fn held_reason(&self, holder: &Claim, area: &QualifiedArea) -> HeldReason {
        if let Some(review) = holder.state.review() {
            return HeldReason::InReview {
                prs: review.prs().iter().cloned().collect(),
            };
        }
        if self.mine.iter().any(|claim| claim.names_wide(area)) {
            return HeldReason::AwaitingAck;
        }
        let settings = self.input.settings;
        if let Some(pending) = self.mine.iter().find_map(|claim| claim.pending.get(area)) {
            return HeldReason::AwaitingHandover {
                escalates_at: pending
                    .asked_at
                    .filter(|_| pending.escalated_at.is_none())
                    .map(|asked| asked + settings.yield_escalates_after()),
            };
        }
        if let Some(waiting) = self
            .input
            .claims
            .iter()
            .filter(|claim| claim.member != self.input.me && claim.waits_for(area))
            .min_by_key(|claim| claim.item)
        {
            return HeldReason::AskHolder {
                waiting: Some(waiting.holder()),
            };
        }
        let taking = |claim: &&&Claim| claim.kind.takes_from(holder.kind);
        let feature = target_claim(
            self.mine
                .iter()
                .filter(taking)
                .copied()
                .filter(|claim| claim.is_working()),
        )
        .or_else(|| target_claim(self.mine.iter().filter(taking).copied()));
        match feature {
            Some(claim) => HeldReason::AskHandover { claim: claim.item },
            None => HeldReason::AskHolder { waiting: None },
        }
    }

    fn share(&self, path: &RepoPath, check: &mut EditCheck) {
        let Some(target) = self.target() else {
            return;
        };
        check.touch.insert(target.item);
        let already = self
            .input
            .shared
            .iter()
            .any(|shared| shared.claim == target.item && shared.path == *path);
        if !already {
            check.plan.share_path(SharedPath {
                claim: target.item,
                path: path.clone(),
            });
        }
    }

    /// The claim new reservations go to: the target among my claims I am working on, else among
    /// my claims in review.
    fn target(&self) -> Option<&'a Claim> {
        target_claim(self.mine.iter().copied().filter(|claim| claim.is_working()))
            .or_else(|| target_claim(self.mine.iter().copied()))
    }

    /// The claim an area an edit meets goes to: [`EditWorld::target`] among my claims that do not
    /// wait for it.
    fn target_for(&self, area: &QualifiedArea) -> Option<&'a Claim> {
        let free = || {
            self.mine
                .iter()
                .copied()
                .filter(|claim| !claim.waits_for(area))
        };
        target_claim(free().filter(|claim| claim.is_working())).or_else(|| target_claim(free()))
    }

    /// Other members' live claims that recorded this path as shared, each once, ordered.
    pub(crate) fn others_sharing(&self, path: &RepoPath) -> Vec<Holder> {
        let input = self.input;
        let stale_after = input.settings.stale_after();
        let holders: BTreeSet<Holder> = input
            .shared
            .iter()
            .filter(|shared| shared.path == *path)
            .filter_map(|shared| input.claims.iter().find(|claim| claim.item == shared.claim))
            .filter(|claim| claim.member != input.me && claim.is_live(input.now, stale_after))
            .map(Claim::holder)
            .collect();
        holders.into_iter().collect()
    }
}

#[cfg(test)]
mod tests;
