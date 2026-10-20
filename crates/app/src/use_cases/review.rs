//! Review: a holder records the pull requests of their claim, an integration reports what became of
//! them, and reads which claims wait in review and what status their pull requests get.

use std::collections::BTreeSet;
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::{self, EventKind, PrReported};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::item::ItemStatus;
use dibs_domain::message::{Author, Importance, Recipients};
use dibs_domain::paths::RelPath;
use dibs_domain::pr_status::{PrState, pr_status};
use dibs_domain::radar::BranchName;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::review::{
    DropRequest, PrEnd, PrRef, PrSettled, ReviewRequest, decide_drop_pr, decide_pr_end,
    decide_review, merge_counts,
};

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{ItemName, NamedClaim, Names, refuse};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::common::existing_item;
use crate::use_cases::handover::Handovers;
use crate::use_cases::radar::{MergedInto, Radar};
use crate::use_cases::slots::Scene;
use crate::use_cases::wide_asks::Announcer;
use crate::use_cases::writing::{Posted, SystemPost, titled_body};
use crate::work::Work;

/// What a holder asks: record this pull request on their claim of `item`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReviewInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The pull request.
    pub pr: PrRef,
    /// Whether more pull requests of the item follow this one.
    pub more: bool,
}

/// A claim in review, with its item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedClaim {
    /// The claim and its holder's handle.
    pub claim: NamedClaim,
    /// The item.
    pub item: ItemName,
}

/// Records a pull request on the caller's claim: the claim is in review and keeps its areas, and it
/// no longer goes stale while the pull request is open. A request to hand one of its areas over
/// asked before is no longer counted, in the same transaction, so that it is asked again once the
/// claim is worked on again.
pub struct StartReview {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl StartReview {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Records the pull request; recording one the claim has changes at most whether more follow.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item;
    /// [`AppError::Review`] when nobody or another member holds it, the claim is stale, another
    /// claim recorded the pull request (naming its holder and item), or no listed repository has
    /// it; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: StartReviewInput,
    ) -> Result<ReviewedClaim, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let item = existing_item(&mut work, input.item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let claims = work.claims(actor.project).await?;
        let request = ReviewRequest {
            item: item.id,
            by: actor.member,
            pr: input.pr,
            more: input.more,
        };
        let started = match decide_review(current.as_ref(), &claims, &request, now, &settings) {
            Ok(started) => started,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        work.save_claim(actor.project, &started.claim).await?;
        if started.added {
            let prs = started
                .claim
                .state
                .review()
                .map(|review| review.prs().clone())
                .unwrap_or_default();
            work.record(
                now,
                EventKind::ReviewStarted(event::ReviewStarted {
                    item: item.id,
                    member: actor.member,
                    prs,
                }),
            )
            .await?;
        }
        Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        }
        .settle(&mut work)
        .await?;
        let reviewed = ReviewedClaim {
            claim: Names::load(&mut work).await?.claim(started.claim)?,
            item: ItemName::of(&item, &self.ref_schemes),
        };
        work.commit(&*self.notifier).await?;
        Ok(reviewed)
    }
}

/// How an integration says a pull request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportedEnd {
    /// It was merged into the branch `base` (`main` or `refs/heads/main`).
    Merged {
        /// The branch it was merged into.
        base: String,
        /// When it was merged.
        merged_at: OffsetDateTime,
        /// The paths it changed, when the integration read them.
        files: Option<BTreeSet<RelPath>>,
        /// The branch it was merged from, when the integration read it.
        head: Option<BranchName>,
    },
    /// It was closed without being merged.
    Closed,
}

/// What an integration reports about a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrReport {
    /// The pull request.
    pub pr: PrRef,
    /// How it ended.
    pub end: ReportedEnd,
}

/// What a report did to the claim that recorded the pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrReportOutcome {
    /// No claim in review recorded it — nobody did, or the report was applied before — so nothing
    /// changed.
    Unmatched,
    /// It left the claim, which waits for its other pull requests.
    StillInReview(ItemId),
    /// It left the claim without delivering its work; the claim waits for its other pull requests,
    /// whose merges no longer complete it, and its holder was told.
    SetAside(ItemId),
    /// It left the claim, whose holder works on it again.
    Working(ItemId),
    /// Its merge completed the claim; the item is done.
    Completed(ItemId),
}

/// Applies an integration's report that a pull request was merged or closed, matched only against
/// the pull requests claims in review recorded. A report nobody's claim matches changes no claim, so
/// an integration may send every report again safely. A merge into a branch that delivers the work
/// also moves the base under every live branch measured against that branch from an older base
/// that changes a path it changed — the merged branch's own claim's other branches included — each
/// such moved base announced once, whether or not a claim recorded the pull request. The merged
/// branch's footprint of a claim that stays live goes: its work is in the base. A debt claim the
/// report completes hands the areas other claims wait for over to them, and one that is worked on
/// again is asked for them, in the same transaction.
pub struct ReportPr {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl ReportPr {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Applies the report.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither an integration nor an admin;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        report: PrReport,
    ) -> Result<PrReportOutcome, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let end = match &report.end {
            ReportedEnd::Merged { base, .. } if merge_counts(&settings, &report.pr, base) => {
                PrEnd::Merged
            }
            ReportedEnd::Merged { .. } => PrEnd::MergedElsewhere,
            ReportedEnd::Closed => PrEnd::Closed,
        };
        let claims = work.claims(actor.project).await?;
        let settled = decide_pr_end(&claims, &report.pr, end, now);
        let radar = Radar {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        let mut merged_branch = None;
        if let ReportedEnd::Merged {
            base,
            merged_at,
            files,
            head,
        } = &report.end
            && end.delivers()
        {
            let merge = MergedInto {
                pr: &report.pr,
                base,
                merged_at: *merged_at,
                files: files.as_ref(),
                head: head.as_ref(),
                author: settled.as_ref().map(|settled| settled.claim().item),
            };
            merged_branch = radar.merged(&mut work, &merge).await?;
        }
        let Some(settled) = settled else {
            work.commit(&*self.notifier).await?;
            return Ok(PrReportOutcome::Unmatched);
        };
        let claim = settled.claim().clone();
        let reported = PrReported {
            item: claim.item,
            member: claim.member,
            pr: report.pr.clone(),
        };
        work.record(
            now,
            match end {
                PrEnd::Merged | PrEnd::MergedElsewhere => EventKind::PrMerged(reported),
                PrEnd::Closed => EventKind::PrClosed(reported),
            },
        )
        .await?;
        let pr = &report.pr;
        let how = match &report.end {
            ReportedEnd::Merged { base, .. } => format!(
                "was merged into {base}, which is not a branch the project protects, so its work is not delivered yet"
            ),
            ReportedEnd::Closed => "was closed without being merged".to_owned(),
        };
        let outcome = match settled {
            PrSettled::StillInReview { claim } => {
                work.save_claim(actor.project, &claim).await?;
                PrReportOutcome::StillInReview(claim.item)
            }
            PrSettled::SetAside { claim } => {
                work.save_claim(actor.project, &claim).await?;
                let waiting = claim
                    .state
                    .review()
                    .map(|review| {
                        review
                            .prs()
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();
                self.tell(&mut work, &claim, "Set aside in review: ", |item, title| {
                    format!(
                        "Pull request {pr} {how}. Your claim on work item {item} \"{title}\" stays in review on {waiting}, but their merges no longer complete it: record the pull request that brings this work in (dibs claim review {item} --pr <owner>/<repo>#<number>), or mark the item done yourself once it is in."
                    )
                }, now)
                .await?;
                PrReportOutcome::SetAside(claim.item)
            }
            PrSettled::Working { claim } => {
                work.save_claim(actor.project, &claim).await?;
                work.record(
                    now,
                    EventKind::ReviewEnded(event::ReviewEnded {
                        item: claim.item,
                        member: claim.member,
                    }),
                )
                .await?;
                if !end.delivers() {
                    self.tell(&mut work, &claim, "Back from review: ", |item, title| {
                        format!(
                            "Pull request {pr} {how}, so your claim on work item {item} \"{title}\" is no longer in review. You still hold it and its areas; work on it again, record the pull request that brings the work in, or release it."
                        )
                    }, now)
                    .await?;
                }
                Announcer {
                    ids: &*self.ids,
                    schemes: &self.ref_schemes,
                    settings: &settings,
                    now,
                }
                .announce_due(&mut work)
                .await?;
                PrReportOutcome::Working(claim.item)
            }
            PrSettled::Completed { claim } => {
                let ending = Ending {
                    status: ItemStatus::Done,
                    event: EventKind::CompletedByMerge(event::CompletedByMerge {
                        item: claim.item,
                        member: claim.member,
                    }),
                };
                end_claim(
                    &mut work,
                    &claim,
                    ending,
                    &Handovers {
                        ids: &*self.ids,
                        schemes: &self.ref_schemes,
                        settings: &settings,
                        now,
                    },
                )
                .await?;
                self.tell(&mut work, &claim, "Done by merge: ", |item, title| {
                    format!(
                        "Pull request {pr} merged; the claim on work item {item} \"{title}\" is done. Its areas are free and the item is done."
                    )
                }, now)
                .await?;
                PrReportOutcome::Completed(claim.item)
            }
        };
        Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        }
        .settle(&mut work)
        .await?;
        let stays = !matches!(outcome, PrReportOutcome::Completed(_));
        if let Some(repo) = merged_branch.filter(|_| stays) {
            radar.drop_footprint(&mut work, claim.item, &repo).await?;
        }
        work.commit(&*self.notifier).await?;
        Ok(outcome)
    }

    /// Tells the claim's holder, in a message from dibs, what became of their pull request: the
    /// subject starts with `subject` and names the item's title, and `lead` writes the text from
    /// how the item is named and its title.
    async fn tell(
        &self,
        work: &mut Work,
        claim: &Claim,
        subject: &str,
        lead: impl FnOnce(&str, &str) -> String,
        now: OffsetDateTime,
    ) -> Result<(), AppError> {
        let item = ItemName::of(&existing_item(work, claim.item).await?, &self.ref_schemes);
        let named = item
            .external_ref
            .as_ref()
            .map_or_else(|| format!("#{}", item.id), ToString::to_string);
        SystemPost {
            ids: Posted::next(&*self.ids),
            subject: format!("{subject}{}", item.title),
            item: Some(item.id),
            on_behalf_of: Author::System,
            to: Recipients::Members([claim.member].into()),
            body: titled_body(&lead(&named, item.title.as_str()), None)?,
            importance: Importance::Normal,
            ack_required: false,
        }
        .send(work, now)
        .await?;
        Ok(())
    }
}

/// What a holder asks: remove this pull request from their claim of `item`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPrInput {
    /// The item, by local id.
    pub item: ItemId,
    /// The pull request recorded by mistake.
    pub pr: PrRef,
}

/// Removes a pull request the caller recorded on their claim by mistake. With none left the claim
/// is worked on again, seen now, and a debt holder among the claims it holds areas for is asked
/// to hand over what another claim waits for.
pub struct DropPr {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl DropPr {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// Removes the pull request.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for an integration; [`AppError::NotFound`] for an unknown item;
    /// [`AppError::DropPr`] when nobody or another member holds it, or the claim did not record
    /// the pull request; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: DropPrInput,
    ) -> Result<ReviewedClaim, AppError> {
        actor.require_person()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let item = existing_item(&mut work, input.item).await?;
        let current = work.claim(actor.project, item.id).await?;
        let request = DropRequest {
            by: actor.member,
            pr: input.pr.clone(),
        };
        let settled = match decide_drop_pr(current.as_ref(), &request, now) {
            Ok(settled) => settled,
            Err(reason) => return Err(refuse(&mut work, reason).await),
        };
        let claim = settled.claim().clone();
        work.save_claim(actor.project, &claim).await?;
        work.record(
            now,
            EventKind::PrDropped(PrReported {
                item: item.id,
                member: actor.member,
                pr: input.pr,
            }),
        )
        .await?;
        if claim.is_working() {
            work.record(
                now,
                EventKind::ReviewEnded(event::ReviewEnded {
                    item: item.id,
                    member: actor.member,
                }),
            )
            .await?;
            let settings = work.project_settings(&self.hot_rules).await?;
            Announcer {
                ids: &*self.ids,
                schemes: &self.ref_schemes,
                settings: &settings,
                now,
            }
            .announce_due(&mut work)
            .await?;
            Handovers {
                ids: &*self.ids,
                schemes: &self.ref_schemes,
                settings: &settings,
                now,
            }
            .settle(&mut work)
            .await?;
        }
        let reviewed = ReviewedClaim {
            claim: Names::load(&mut work).await?.claim(claim)?,
            item: ItemName::of(&item, &self.ref_schemes),
        };
        work.commit(&*self.notifier).await?;
        Ok(reviewed)
    }
}

/// A claim in review as an integration watches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewClaim {
    /// The claim and its holder's handle.
    pub claim: NamedClaim,
    /// The item.
    pub item: ItemName,
    /// The pull requests it recorded, in order.
    pub prs: BTreeSet<PrRef>,
    /// Since when it is in review.
    pub since: OffsetDateTime,
    /// Whether more pull requests of the item follow these.
    pub more: bool,
}

/// Lists the claims in review with the pull requests they recorded, so an integration knows which
/// pull requests to watch.
pub struct ReviewClaims {
    store: Arc<dyn Store>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl ReviewClaims {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            ref_schemes: deps.ref_schemes.clone(),
        }
    }

    /// The claims in review, ordered by item.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither an integration nor an admin;
    /// [`AppError::Store`] when the store fails.
    pub async fn execute(&self, actor: &Actor) -> Result<Vec<ReviewClaim>, AppError> {
        actor.require_import()?;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let claims = work.claims(actor.project).await?;
        let names = Names::load(&mut work).await?;
        let mut listed = Vec::new();
        for claim in claims {
            let Some(review) = claim.state.review().cloned() else {
                continue;
            };
            let item = existing_item(&mut work, claim.item).await?;
            listed.push(ReviewClaim {
                prs: review.prs().clone(),
                since: review.since(),
                more: review.more(),
                item: ItemName::of(&item, &self.ref_schemes),
                claim: names.claim(claim)?,
            });
        }
        work.commit_quietly().await?;
        Ok(listed)
    }
}

/// The status of the pull requests of one repository into one base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatusView {
    /// Whether the project wants the repository's pull requests to get a status at all.
    pub enabled: bool,
    /// The name the status is filed under.
    pub context: String,
    /// Failure while a held slot freezes the base branch, success otherwise.
    pub state: PrState,
    /// The description shown with it.
    pub description: String,
}

/// What a pull-request status is asked for: a repository, by its id or its remote's path, and the
/// base branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrStatusQuery {
    /// The repository as named.
    pub repo: String,
    /// The base branch (`main` or `refs/heads/main`).
    pub base: String,
}

/// Computes the status a forge shows on pull requests into a base branch, from the slots and the
/// settings, after applying what time changed about the slots.
pub struct ReadPrStatus {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
}

impl ReadPrStatus {
    /// Builds the use case.
    pub fn new(deps: &Dependencies) -> Self {
        Self {
            store: deps.store.clone(),
            clock: deps.clock.clone(),
            ids: deps.ids.clone(),
            notifier: deps.notifier.clone(),
            hot_rules: deps.hot_rules.clone(),
        }
    }

    /// The status now.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither an integration nor an admin;
    /// [`AppError::NotFound`] for a repository the project does not list or a base that is no
    /// branch; [`AppError::Store`] when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        query: PrStatusQuery,
    ) -> Result<PrStatusView, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let mut scene = Scene::load(&mut work, &self.hot_rules, now).await?;
        scene.save(&mut work, &*self.ids, now).await?;
        let repo = scene
            .settings
            .repo_named(&query.repo)
            .ok_or_else(|| AppError::NotFound(Missing::Repository(query.repo.clone())))?;
        let base = repo
            .branch(&query.base)
            .ok_or_else(|| AppError::NotFound(Missing::Branch(query.base.clone())))?;
        let settings = repo.pr_status.clone();
        let status = pr_status(&base, scene.catalog(), &scene.board);
        let holder = match &status.freeze {
            Some(freeze) => Some(Names::load(&mut work).await?.handle(freeze.holder)?),
            None => None,
        };
        let view = PrStatusView {
            enabled: settings.enabled,
            context: settings.context.to_string(),
            state: status.state,
            description: status.description(settings.describe_holder, holder.as_ref()),
        };
        work.commit(&*self.notifier).await?;
        Ok(view)
    }
}

#[cfg(test)]
mod tests;
