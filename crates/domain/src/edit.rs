//! The edit check: for each path a member is about to change, may they, and what is recorded.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;
use time::{Duration, OffsetDateTime};

use crate::area::QualifiedArea;
use crate::claim::{AreaOrigin, Claim, Holder, held_by_other, target_claim};
use crate::grant::Grant;
use crate::hot_rule::{HotContext, HotDecision, HotRuleKind, Note, Recipe};
use crate::ids::{ItemId, MemberId};
use crate::paths::RepoPath;
use crate::repo::RepoId;
use crate::reservation::{AddArea, ReservationPlan, SharedPath};
use crate::review::PrRef;
use crate::settings::{EditPolicy, ProjectSettings};

mod reported;

pub use reported::{ReportedEdits, ReportedEditsError, claims_shown_by};

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
        /// The command that regenerates the file, for a rule that names one.
        recipe: Option<Recipe>,
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
    /// The member's claims the edits were attributed to. An edit is a sign of the claim's own
    /// work, so the caller refreshes their `last_seen`, wherever the claim was taken. Edits the
    /// hooks let through without a check, reported with a heartbeat, refresh claims by the same
    /// attribution ([`claims_shown_by`]).
    pub touch: BTreeSet<ItemId>,
    /// Of `touch`, the claims an edit was attributed to by its area: the claim declared the area,
    /// names it wide, or was granted edits in it. A claim an edit reached only as the member's
    /// target claim — for an area none of their claims answers for, for an area it holds only
    /// because such an edit gained it, or for a shared path — is in `touch` alone.
    pub by_area: BTreeSet<ItemId>,
    /// For each claim of `touch` an edit in an area was attributed to, the areas of those edits.
    /// The caller moves the clocks of those the claim holds ([`Claim::saw_work`]); an area it only
    /// names wide, was granted edits in, or that waits to be handed over to another claim has no
    /// clock there.
    pub edited: BTreeMap<ItemId, BTreeSet<QualifiedArea>>,
}

impl EditCheck {
    /// The areas of the edits attributed to the claim on `item`, in order; none when no edit in an
    /// area was.
    pub fn edited_areas(&self, item: ItemId) -> impl Iterator<Item = &QualifiedArea> {
        self.edited.get(&item).into_iter().flatten()
    }

    /// Notes that an edit in `area` was attributed to the claim on `item`.
    fn edited_in(&mut self, item: ItemId, area: &QualifiedArea) {
        self.edited.entry(item).or_default().insert(area.clone());
    }

    /// How the edits came to be attributed to the claim on `item`, when they were: by an area when
    /// any of them was, else as the member's target claim.
    pub fn attribution(&self, item: ItemId) -> Option<Attribution> {
        if self.by_area.contains(&item) {
            Some(Attribution::Area)
        } else if self.touch.contains(&item) {
            Some(Attribution::Target)
        } else {
            None
        }
    }
}

/// How an edit came to be attributed to one of the member's claims, which says whether the
/// attribution tells what work the edit is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attribution {
    /// The claim declared the edit's area, names it wide, or was granted edits in it: the edit is
    /// that claim's work.
    Area,
    /// No claim of the member answers for the edit's area or path, so the edit went to the
    /// member's target claim, the one most recently claimed or renewed: a guess at the work, which
    /// keeps the claim alive and nothing more. An edit in an area the claim holds only because
    /// such a guess gained it is the same guess.
    Target,
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
/// Each claim an edit is attributed to is told apart by how ([`EditCheck::attribution`]): by its
/// area when the claim declared the area, names it wide or was granted edits in it; as the target
/// when the edit went to it only because none of my claims answers for the area, or for a shared
/// path. An area added to a claim this way is gained ([`AreaOrigin::Gained`]): the claim holds it
/// like a declared one, but a later edit in it is still attributed to the claim as the target,
/// until a renewal of the claim declares it. When several of my live claims hold the area, the edit
/// goes to one that declared it — the most recently claimed or renewed of those, the lowest item on
/// a tie — and to one that only gained it only when none declared it.
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
        let mine = live_claims_of(input.me, input.claims, input.now, stale_after);
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
                    check.by_area.insert(grant.item);
                    check.edited_in(grant.item, area);
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
        let waited_for_elsewhere = input
            .claims
            .iter()
            .any(|claim| claim.member != input.me && claim.waits_for(area));
        if let Some(claim) = taker_of(&self.mine, area) {
            check.touch.insert(claim.item);
            check.edited_in(claim.item, area);
            if claim.areas.origin(area) != Some(AreaOrigin::Gained) {
                check.by_area.insert(claim.item);
            }
        } else if let Some(target) = self.target_for(area) {
            if !waited_for_elsewhere {
                check.plan.add_area(AddArea {
                    claim: target.item,
                    area: area.clone(),
                });
            }
            check.touch.insert(target.item);
            check.edited_in(target.item, area);
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
        work_target(&self.mine)
    }

    /// The claim an area an edit meets goes to ([`target_for`]).
    fn target_for(&self, area: &QualifiedArea) -> Option<&'a Claim> {
        target_for(&self.mine, area)
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

/// The member's live claims among `claims`, from any session or host.
fn live_claims_of(
    me: MemberId,
    claims: &[Claim],
    now: OffsetDateTime,
    stale_after: Duration,
) -> Vec<&Claim> {
    claims
        .iter()
        .filter(|claim| claim.member == me && claim.is_live(now, stale_after))
        .collect()
}

/// The one claim among `mine`, the member's live claims, that an edit in `area` nobody else holds
/// goes to: the target among those that declared the area, else among their wide claims that name
/// it, else among those that only gained it. `None` when none of them answers for the area. A
/// member's own claims may share an area, so this picks one of them rather than every holder.
fn taker_of<'a>(mine: &[&'a Claim], area: &QualifiedArea) -> Option<&'a Claim> {
    let holding = |origin: AreaOrigin| {
        target_claim(
            mine.iter()
                .copied()
                .filter(move |claim| claim.areas.origin(area) == Some(origin)),
        )
    };
    let wide = || target_claim(mine.iter().copied().filter(|claim| claim.names_wide(area)));
    holding(AreaOrigin::Declared)
        .or_else(wide)
        .or_else(|| holding(AreaOrigin::Gained))
}

/// The claim among `mine`, the member's live claims, that an edit in `area` goes to when nobody
/// holds the area and none of them answers for it: their target claim ([`work_target`]) among
/// those that do not wait for the area.
fn target_for<'a>(mine: &[&'a Claim], area: &QualifiedArea) -> Option<&'a Claim> {
    let free = || mine.iter().copied().filter(|claim| !claim.waits_for(area));
    target_claim(free().filter(|claim| claim.is_working())).or_else(|| target_claim(free()))
}

/// The member's target claim among `mine`, their live claims: of those they work on, the one most
/// recently claimed or renewed, the lowest item on a tie; with none, of those in review by the
/// same rule.
fn work_target<'a>(mine: &[&'a Claim]) -> Option<&'a Claim> {
    target_claim(mine.iter().copied().filter(|claim| claim.is_working()))
        .or_else(|| target_claim(mine.iter().copied()))
}

#[cfg(test)]
mod tests;
