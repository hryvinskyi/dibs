//! The branch overlap radar: an integration reports what each claim's branch changes, the overlaps
//! between live claims of different members are derived from those reports, and each one found is
//! announced once, in a message from dibs, to the holders it concerns.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::event::{self, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ItemId;
use dibs_domain::message::{Author, Recipients};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{
    BranchName, Footprint, FootprintBase, FootprintDigest, FootprintOutcome, FootprintReport,
    Merge, NotStored, Overlap, OverlapChanges, OverlapKind, RadarScene, decide_footprint,
    reconcile,
};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::common::existing_item;
use crate::use_cases::writing::{Posted, SystemPost, system_body};
use crate::work::Work;

/// The checkout a member's request comes from: its repository and the branch it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnBranch {
    /// The repository.
    pub repo: RepoId,
    /// The branch.
    pub branch: BranchName,
}

/// What an integration reports of a claim's branch, its repository as the integration names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FootprintInput {
    /// The item of the claim.
    pub item: ItemId,
    /// The repository, by its id (`api`) or its remote's path (`acme/api`).
    pub repo: String,
    /// The branch it compared.
    pub branch: BranchName,
    /// What it compared against: the base branch, the merge base and its time.
    pub base: FootprintBase,
    /// The paths the branch changes against that base, at most [`FootprintReport::MAX_PATHS`].
    pub paths: BTreeSet<RelPath>,
    /// Whether its forge gave it fewer paths than the branch changes.
    pub truncated: bool,
}

/// What a footprint report did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FootprintRecorded {
    /// It was stored; `changed` says whether it differed from the one before.
    Stored {
        /// Whether it replaced a different footprint, or was the first.
        changed: bool,
    },
    /// It was not stored, and why.
    NotStored(NotStored),
}

/// Stores what an integration reports a claim's branch changes against its base, and derives the
/// overlaps it makes. A report for an item no live claim holds, or for another branch than the one
/// the claim records, is not stored; one that says what is stored changes nothing and appends
/// nothing, so an integration may send every report again safely. A footprint measured from a base
/// no older than a merge that moved the claim's base has caught up with it: that moved base ends.
pub struct RecordFootprint {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl RecordFootprint {
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

    /// Records the report.
    ///
    /// # Errors
    /// [`AppError::Forbidden`] for a member who is neither an integration nor an admin;
    /// [`AppError::NotFound`] for a repository the project does not list; [`AppError::Store`]
    /// when the store fails.
    pub async fn execute(
        &self,
        actor: &Actor,
        input: FootprintInput,
    ) -> Result<FootprintRecorded, AppError> {
        actor.require_import()?;
        let now = self.clock.now();
        let project = actor.project;
        let mut work = Work::begin_for(&*self.store, actor).await?;
        let settings = work.project_settings(&self.hot_rules).await?;
        let repo = settings
            .repo_named(&input.repo)
            .map(|repo| repo.id.clone())
            .ok_or_else(|| AppError::NotFound(Missing::Repository(input.repo.clone())))?;
        let report = FootprintReport {
            item: input.item,
            repo,
            branch: input.branch,
            base: input.base,
            paths: input.paths,
            truncated: input.truncated,
        };
        let claim = work.claim(project, report.item).await?;
        let current = work.footprint(project, report.item, &report.repo).await?;
        let outcome = decide_footprint(
            claim.as_ref(),
            current
                .as_ref()
                .map(|(footprint, paths)| (footprint, paths)),
            &report,
            &settings,
            now,
        );
        let (footprint, member) = match outcome {
            FootprintOutcome::NotStored(why) => {
                work.commit_quietly().await?;
                return Ok(FootprintRecorded::NotStored(why));
            }
            FootprintOutcome::Unchanged => {
                work.commit_quietly().await?;
                return Ok(FootprintRecorded::Stored { changed: false });
            }
            FootprintOutcome::Replaced { footprint, member } => (footprint, member),
        };
        work.save_footprint(project, &footprint, &report.paths)
            .await?;
        work.record(
            now,
            EventKind::FootprintChanged(event::FootprintChanged {
                item: footprint.item,
                member,
                repo: footprint.repo.clone(),
                branch: footprint.branch.clone(),
                paths: footprint.path_count,
                truncated: footprint.truncated,
            }),
        )
        .await?;
        let radar = Radar {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        radar.catch_up(&mut work, &footprint).await?;
        radar.refresh_item(&mut work, footprint.item).await?;
        work.commit(&*self.notifier).await?;
        Ok(FootprintRecorded::Stored { changed: true })
    }
}

/// A merge into a base, as an integration reports it, for the moved bases it brings.
pub(crate) struct MergedInto<'a> {
    /// The pull request.
    pub(crate) pr: &'a PrRef,
    /// The branch it was merged into, as the forge names it (`main` or `refs/heads/main`).
    pub(crate) base: &'a str,
    /// When it was merged.
    pub(crate) merged_at: OffsetDateTime,
    /// The paths it changed, when the integration sent them.
    pub(crate) files: Option<&'a BTreeSet<RelPath>>,
    /// The branch it was merged from, when the integration sent it.
    pub(crate) head: Option<&'a BranchName>,
    /// The item of the claim that recorded it, when one did.
    pub(crate) author: Option<ItemId>,
}

/// The radar's work inside a use case's transaction: deriving overlaps from the stored claims and
/// footprints, storing what changed and announcing what is new.
pub(crate) struct Radar<'a> {
    pub(crate) ids: &'a dyn IdGen,
    pub(crate) schemes: &'a RefSchemeRegistry,
    pub(crate) settings: &'a ProjectSettings,
    pub(crate) now: OffsetDateTime,
}

/// The most paths a message about an overlap lists by name.
const LISTED_PATHS: usize = 20;

impl Radar<'_> {
    /// Derives again the same-file and contract overlaps that name `item`, from its footprints and
    /// the others: those found are announced, those gone are removed. Stale claims count for what
    /// holds, so an overlap a stale claim is part of is neither ended nor announced again; one is
    /// announced only while every claim it names is live.
    pub(crate) async fn refresh_item(&self, work: &mut Work, item: ItemId) -> Result<(), AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let footprints = work.footprints(project).await?;
        let mut mine: BTreeSet<RepoPath> = BTreeSet::new();
        let mut others = Vec::new();
        for footprint in footprints.iter().filter(|footprint| footprint.item == item) {
            let Some((_, paths)) = work.footprint(project, item, &footprint.repo).await? else {
                continue;
            };
            let sharing = work
                .footprint_entries(project, &footprint.repo, &paths)
                .await?;
            others.extend(sharing.into_iter().filter(|entry| entry.item != item));
            mine.extend(paths.into_iter().map(|path| RepoPath {
                repo: footprint.repo.clone(),
                path,
            }));
        }
        let scene = RadarScene {
            claims: &claims,
            footprints: &footprints,
            settings: self.settings,
            now: self.now,
        };
        let mut derived = scene.same_file(item, &mine, &others);
        derived.extend(
            scene
                .contracts()
                .into_iter()
                .filter(|overlap| overlap.involves(item)),
        );
        let stored = work.overlaps(project).await?;
        let changes = reconcile(
            &stored,
            &derived,
            |overlap| overlap.involves(item) && overlap.kind() != OverlapKind::MainMoved,
            |overlap| scene.all_live(overlap),
        );
        self.apply(work, &claims, changes).await
    }

    /// Brings the radar up to settings that changed: the footprints of a repository the project no
    /// longer lists are dropped with what they made, what each other footprint's paths mean under
    /// the new areas and contracts is read again, and every contract overlap is derived again.
    pub(crate) async fn refresh_contracts(&self, work: &mut Work) -> Result<(), AppError> {
        let project = work.project_id();
        let mut dropped = BTreeSet::new();
        for footprint in work.footprints(project).await? {
            if self.settings.repo(&footprint.repo).is_none() {
                work.delete_footprint(project, footprint.item, &footprint.repo)
                    .await?;
                self.end_moved_bases(work, footprint.item, &footprint.repo)
                    .await?;
                dropped.insert(footprint.item);
                continue;
            }
            let Some((_, paths)) = work
                .footprint(project, footprint.item, &footprint.repo)
                .await?
            else {
                continue;
            };
            let digest = FootprintDigest::of(&footprint.repo, &paths, self.settings);
            if digest != footprint.digest {
                work.set_footprint_digest(project, footprint.item, &footprint.repo, &digest)
                    .await?;
            }
        }
        for item in dropped {
            self.refresh_item(work, item).await?;
        }
        let claims = work.claims(project).await?;
        let footprints = work.footprints(project).await?;
        let scene = RadarScene {
            claims: &claims,
            footprints: &footprints,
            settings: self.settings,
            now: self.now,
        };
        let derived = scene.contracts();
        let stored = work.overlaps(project).await?;
        let changes = reconcile(
            &stored,
            &derived,
            |overlap| overlap.kind() == OverlapKind::Contract,
            |overlap| scene.all_live(overlap),
        );
        self.apply(work, &claims, changes).await
    }

    /// Drops the claim's footprint in the repository, when it has one, and what it made: its moved
    /// bases there end, and its other overlaps are derived again without it. The footprint no
    /// longer describes the claim's work there — its branch changed, or was merged.
    pub(crate) async fn drop_footprint(
        &self,
        work: &mut Work,
        item: ItemId,
        repo: &RepoId,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        if work.footprint(project, item, repo).await?.is_none() {
            return Ok(());
        }
        work.delete_footprint(project, item, repo).await?;
        self.end_moved_bases(work, item, repo).await?;
        self.refresh_item(work, item).await
    }

    /// Ends the moved bases of the footprint's claim in its repository that the footprint has
    /// caught up with: its base is no older than the merge.
    async fn catch_up(&self, work: &mut Work, footprint: &Footprint) -> Result<(), AppError> {
        let project = work.project_id();
        for overlap in work.overlaps(project).await? {
            if overlap.caught_up_by(footprint) {
                work.delete_overlap(project, &overlap.key()).await?;
            }
        }
        Ok(())
    }

    /// Ends every moved base of `item` in `repo`.
    async fn end_moved_bases(
        &self,
        work: &mut Work,
        item: ItemId,
        repo: &RepoId,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        for overlap in work.overlaps(project).await? {
            let moved = overlap.kind() == OverlapKind::MainMoved
                && overlap.involves(item)
                && overlap.in_repo(repo);
            if moved {
                work.delete_overlap(project, &overlap.key()).await?;
            }
        }
        Ok(())
    }

    /// Finds the moved bases a merge brings and announces the new ones, and answers the
    /// repository of the claim's footprint that was the merged branch, when there is one. The
    /// merge's paths are those the integration sent, or else the paths of that footprint; with
    /// neither, nothing is known to have moved.
    pub(crate) async fn merged(
        &self,
        work: &mut Work,
        merge: &MergedInto<'_>,
    ) -> Result<Option<RepoId>, AppError> {
        let project = work.project_id();
        let Some(repo) = self
            .settings
            .repo_of_pr(merge.pr)
            .map(|repo| repo.id.clone())
        else {
            return Ok(None);
        };
        let Ok(base) = BranchName::parse_ref(merge.base) else {
            return Ok(None);
        };
        let empty = BTreeSet::new();
        let shape = Merge {
            pr: merge.pr,
            repo: &repo,
            base: &base,
            merged_at: merge.merged_at,
            files: merge.files.unwrap_or(&empty),
            head: merge.head,
            author: merge.author,
        };
        let merged_footprint = match merge.author {
            Some(item) => work
                .footprint(project, item, &repo)
                .await?
                .filter(|(footprint, _)| shape.merged(footprint)),
            None => None,
        };
        let files = match (merge.files, &merged_footprint) {
            (Some(files), _) => files.clone(),
            (None, Some((_, paths))) => paths.clone(),
            (None, None) => BTreeSet::new(),
        };
        let merged_repo = merged_footprint.map(|(footprint, _)| footprint.repo);
        if files.is_empty() {
            return Ok(merged_repo);
        }
        let hits = work.footprint_entries(project, &repo, &files).await?;
        let claims = work.claims(project).await?;
        let footprints = work.footprints(project).await?;
        let scene = RadarScene {
            claims: &claims,
            footprints: &footprints,
            settings: self.settings,
            now: self.now,
        };
        let derived = scene.main_moved(
            &Merge {
                files: &files,
                ..shape
            },
            &hits,
        );
        let stored = work.overlaps(project).await?;
        let changes = reconcile(
            &stored,
            &derived,
            |_| false,
            |overlap| scene.all_live(overlap),
        );
        self.apply(work, &claims, changes).await?;
        Ok(merged_repo)
    }

    /// Stores what changed and announces each overlap found.
    async fn apply(
        &self,
        work: &mut Work,
        claims: &[Claim],
        changes: OverlapChanges,
    ) -> Result<(), AppError> {
        let project = work.project_id();
        for ended in &changes.ended {
            work.delete_overlap(project, &ended.key()).await?;
        }
        for moved in &changes.moved {
            work.save_overlap(project, moved).await?;
        }
        if changes.found.is_empty() {
            return Ok(());
        }
        let names = Names::load(work).await?;
        for found in &changes.found {
            work.save_overlap(project, found).await?;
            self.announce(work, claims, &names, found).await?;
        }
        Ok(())
    }

    /// Writes the message of an overlap found to the holders of the claims it names, and records
    /// that it was found.
    async fn announce(
        &self,
        work: &mut Work,
        claims: &[Claim],
        names: &Names,
        overlap: &Overlap,
    ) -> Result<(), AppError> {
        let mut parties = Vec::new();
        for item in overlap.items() {
            let Some(claim) = claims.iter().find(|claim| claim.item == *item) else {
                continue;
            };
            let named = ItemName::of(&existing_item(work, *item).await?, self.schemes);
            parties.push(Party {
                item: named,
                holder: names.handle(claim.member)?,
                claim,
            });
        }
        let Some(first) = parties.first() else {
            return Ok(());
        };
        let holders: BTreeSet<_> = parties.iter().map(|party| party.claim.member).collect();
        let lead = wording(overlap, &parties);
        SystemPost {
            ids: Posted::next(self.ids),
            subject: subject(overlap, &parties),
            item: Some(first.item.id),
            on_behalf_of: Author::System,
            to: Recipients::Members(holders),
            body: system_body(&lead, None)?,
            importance: overlap.kind().importance(),
            ack_required: false,
        }
        .send(work, self.now)
        .await?;
        work.record(
            self.now,
            EventKind::OverlapFound(event::OverlapFound {
                kind: overlap.kind(),
                items: overlap.items().clone(),
                pr: overlap.pr().cloned(),
                paths: overlap
                    .paths()
                    .iter()
                    .take(event::OverlapFound::MAX_PATHS)
                    .cloned()
                    .collect(),
            }),
        )
        .await?;
        Ok(())
    }
}

/// One claim an overlap names, with its item and holder.
struct Party<'a> {
    item: ItemName,
    holder: Handle,
    claim: &'a Claim,
}

impl Party<'_> {
    /// `work item gh:acme/widgets#42 "Title" (alice, on alice/42-x)`.
    fn described(&self, repos: &BTreeSet<&RepoId>) -> String {
        let branches: Vec<String> = repos
            .iter()
            .filter_map(|repo| self.claim.branch(repo))
            .map(ToString::to_string)
            .collect();
        let on = if branches.is_empty() {
            String::new()
        } else {
            format!(", on {}", branches.join(" and "))
        };
        format!(
            "work item {} \"{}\" ({}{on})",
            reference(&self.item),
            self.item.title,
            self.holder
        )
    }
}

fn reference(item: &ItemName) -> String {
    item.external_ref
        .as_ref()
        .map_or_else(|| format!("#{}", item.id), ToString::to_string)
}

/// The overlap's paths for a sentence: the first ones by name, then how many more.
fn listed(paths: &BTreeSet<RepoPath>) -> String {
    let named: Vec<String> = paths
        .iter()
        .take(LISTED_PATHS)
        .map(ToString::to_string)
        .collect();
    let more = paths.len().saturating_sub(LISTED_PATHS);
    if more == 0 {
        named.join(", ")
    } else {
        format!("{} and {more} more", named.join(", "))
    }
}

fn subject(overlap: &Overlap, parties: &[Party<'_>]) -> String {
    let refs: Vec<String> = parties.iter().map(|party| reference(&party.item)).collect();
    let pair = refs.join(" and ");
    match overlap.kind() {
        OverlapKind::SameFile => format!("Overlap: {pair} change the same files"),
        OverlapKind::Contract => format!("Overlap: {pair} meet at a published contract"),
        OverlapKind::MainMoved => format!(
            "Base moved: {} changed files of {pair}",
            overlap.pr().map(ToString::to_string).unwrap_or_default()
        ),
    }
}

fn wording(overlap: &Overlap, parties: &[Party<'_>]) -> String {
    let repos: BTreeSet<&RepoId> = overlap.paths().iter().map(|path| &path.repo).collect();
    let who: Vec<String> = parties
        .iter()
        .map(|party| party.described(&repos))
        .collect();
    let who = who.join(" and ");
    let paths = listed(overlap.paths());
    match overlap.kind() {
        OverlapKind::SameFile => format!(
            "The branches of {who} change the same files: {paths}. Rebase onto the base now, and settle here who changes what, so the second merge does not meet a conflict."
        ),
        OverlapKind::Contract => format!(
            "The branches of {who} meet at a published contract: one changes {paths}, and the other works where that contract is used. Rebase onto the base now, and read the other change before going on, so both sides build on the same contract."
        ),
        OverlapKind::MainMoved => format!(
            "Pull request {} was merged into the base after the branch of {who} took its base, and it changed files that branch changes too: {paths}. Rebase onto the base now.",
            overlap.pr().map(ToString::to_string).unwrap_or_default()
        ),
    }
}

/// An overlap with the items it names and their holders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedOverlap {
    /// The overlap.
    pub overlap: Overlap,
    /// Each item it names with its holder's handle, in item order.
    pub items: Vec<(ItemName, Handle)>,
}

/// The stored overlaps whose claims are all live, each with its items and holders, in key order.
/// The items they name are read together, once.
pub(crate) async fn live_overlaps(
    work: &mut Work,
    claims: &[Claim],
    names: &Names,
    settings: &ProjectSettings,
    schemes: &RefSchemeRegistry,
    now: OffsetDateTime,
) -> Result<Vec<NamedOverlap>, AppError> {
    let project = work.project_id();
    let live: BTreeMap<ItemId, &Claim> = claims
        .iter()
        .filter(|claim| claim.is_live(now, settings.stale_after()))
        .map(|claim| (claim.item, claim))
        .collect();
    let shown: Vec<Overlap> = work
        .overlaps(project)
        .await?
        .into_iter()
        .filter(|overlap| overlap.items().iter().all(|item| live.contains_key(item)))
        .collect();
    let wanted: BTreeSet<ItemId> = shown
        .iter()
        .flat_map(|overlap| overlap.items().iter().copied())
        .collect();
    let records: BTreeMap<ItemId, ItemName> = work
        .items_of(project, &wanted)
        .await?
        .iter()
        .map(|record| (record.id, ItemName::of(record, schemes)))
        .collect();
    let mut named = Vec::with_capacity(shown.len());
    for overlap in shown {
        let mut items = Vec::new();
        for item in overlap.items() {
            let (Some(record), Some(claim)) = (records.get(item), live.get(item)) else {
                return Err(AppError::NotFound(Missing::Item(*item)));
            };
            items.push((record.clone(), names.handle(claim.member)?));
        }
        named.push(NamedOverlap { overlap, items });
    }
    Ok(named)
}

#[cfg(test)]
mod tests;
