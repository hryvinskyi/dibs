//! The branch overlap radar: an integration reports what each claim's branch changes, the overlaps
//! between live claims of different members are derived from those reports, and each one found is
//! announced once, in a message from dibs to each holder it concerns, in that holder's claim's
//! conversation. A collision between two claims carries an order: which one lands first and why,
//! worked out when it is found and again as its claims change; dibs raises the rule's ties with
//! the project's arbiters itself.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use time::OffsetDateTime;

use dibs_domain::claim::Claim;
use dibs_domain::escalation::{EscalationReason, EscalationSubject, Parties};
use dibs_domain::event::{self, EventKind};
use dibs_domain::handle::Handle;
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, MemberId, MessageId, ThreadId};
use dibs_domain::message::{Importance, may_read};
use dibs_domain::paths::{RelPath, RepoPath};
use dibs_domain::radar::{
    BranchName, Contender, Footprint, FootprintBase, FootprintDigest, FootprintOutcome,
    FootprintReport, Landing, Merge, NotStored, OrderRule, Overlap, OverlapChanges, OverlapKind,
    OverlapOrder, RadarScene, decide_footprint, order, reconcile,
};
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::RepoId;
use dibs_domain::review::PrRef;
use dibs_domain::settings::ProjectSettings;

use crate::actor::Actor;
use crate::dependencies::Dependencies;
use crate::error::{AppError, Missing};
use crate::named::{ItemName, Names};
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::records::{EscalationRecord, ItemRecord};
use crate::use_cases::claim_notice::{ClaimNotice, claim_link};
use crate::use_cases::escalate::{Case, Origin, Raised, item_about};
use crate::use_cases::writing::{SystemReply, titled_body};
use crate::work::Work;

mod checkout;
mod held;

pub use checkout::FromCheckout;
pub(crate) use checkout::{fed_by_checkout, report_checkout};

/// The repository a member's checkout is of and the branch it is on.
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
/// nothing, so an integration may send every report again safely. A report that replaced what was
/// stored shows a push of the claim's work, so the claim is seen now; an unchanged one moves
/// nothing, and since a stale claim's report is not stored, a push never brings a stale claim
/// back. A footprint measured from a base
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
        if let Some(mut pushed) = claim {
            pushed.last_seen = now;
            work.save_claim(project, &pushed).await?;
        }
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

    /// Brings the radar up to settings that changed. The footprints of a repository the project no
    /// longer lists are dropped with what they made, and what each other footprint's paths mean
    /// under the new areas and contracts is read again. Then every overlap the settings decide is
    /// derived again: the same-file and contract overlaps of every claim that has or had a
    /// footprint, as [`Radar::refresh_item`] derives them, and every contract overlap of the
    /// project. An overlap the new settings no longer find ends, and one they now find is stored
    /// and announced as any new overlap is. Moved bases in a repository still listed stay: they
    /// are about merges that already happened.
    pub(crate) async fn adopt_settings(&self, work: &mut Work) -> Result<(), AppError> {
        let project = work.project_id();
        let mut refreshed = BTreeSet::new();
        for footprint in work.footprints(project).await? {
            refreshed.insert(footprint.item);
            if self.settings.repo(&footprint.repo).is_none() {
                work.delete_footprint(project, footprint.item, &footprint.repo)
                    .await?;
                self.end_moved_bases(work, footprint.item, &footprint.repo)
                    .await?;
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
        for item in refreshed {
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

    /// Stores what changed and announces each overlap found. A collision found is stored with its
    /// order, worked out from its two claims and their items' priorities, and the order is recorded
    /// as the first one of the collision; dibs raises one the rule ties with the arbiters before
    /// its holders are told, so that the notices say who decides. An overlap stored again on other
    /// paths keeps the order it had.
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
        let items = items_named(work, &changes.found).await?;
        for found in &changes.found {
            let ordered = found
                .clone()
                .with_order(order_of(found, claims, &items))
                .map_err(|_| misordered())?;
            work.save_overlap(project, &ordered).await?;
            let settling = self.settle(work, claims, &names, &items, &ordered).await?;
            self.announce(work, claims, &names, &items, &ordered, settling.as_ref())
                .await?;
            self.record_order(work, &ordered, false).await?;
        }
        Ok(())
    }

    /// Records the order `overlap` is stored with; `changed` says whether it replaced an earlier
    /// one. An overlap stored without an order records nothing.
    async fn record_order(
        &self,
        work: &mut Work,
        overlap: &Overlap,
        changed: bool,
    ) -> Result<(), AppError> {
        if let Some(ordered) = event::OverlapOrdered::of(overlap, changed) {
            work.record(self.now, EventKind::OverlapOrdered(ordered))
                .await?;
        }
        Ok(())
    }

    /// Works out again the order of every stored collision whose claims are all live, from those
    /// claims and their items' priorities as they are now, and answers how many changed. A changed
    /// order is stored. Both holders are told in their claims' conversations, and the new order is
    /// recorded as a change, only when which claim lands first changed, or the order became or
    /// stopped being a tie, which dibs raises with the arbiters first; a change of the reason alone
    /// is stored without a notice or a record. A collision found before orders were stored has
    /// none: the first order worked out for it is told as a first notice and recorded as a first
    /// order, not as a change. A collision whose order did not change writes nothing.
    pub(crate) async fn reorder(&self, work: &mut Work) -> Result<usize, AppError> {
        let project = work.project_id();
        let claims = work.claims(project).await?;
        let stale_after = self.settings.stale_after();
        let live: BTreeSet<ItemId> = claims
            .iter()
            .filter(|claim| claim.is_live(self.now, stale_after))
            .map(|claim| claim.item)
            .collect();
        let watched: Vec<Overlap> = work
            .overlaps(project)
            .await?
            .into_iter()
            .filter(|overlap| {
                overlap.kind().is_collision()
                    && overlap.items().iter().all(|item| live.contains(item))
            })
            .collect();
        if watched.is_empty() {
            return Ok(0);
        }
        let items = items_named(work, &watched).await?;
        let names = Names::load(work).await?;
        let mut changed = 0;
        for overlap in watched {
            let (Some(words), Some(worked)) = (
                collision_words(overlap.kind()),
                order_of(&overlap, &claims, &items),
            ) else {
                continue;
            };
            let before = overlap.order().copied();
            if before == Some(worked) {
                continue;
            }
            let reordered = overlap.with_order(Some(worked)).map_err(|_| misordered())?;
            work.save_overlap(project, &reordered).await?;
            changed += 1;
            if before.is_some_and(|before| before.first() == worked.first()) {
                continue;
            }
            let first_order = before.is_none();
            let settling = self
                .settle(work, &claims, &names, &items, &reordered)
                .await?;
            let parties = self.parties(&claims, &names, &items, &reordered)?;
            for (index, reader) in parties.iter().enumerate() {
                let others = others_of(&parties, index);
                let standing = standing(&reordered, reader.claim, settling.as_ref());
                let text = if first_order {
                    wording(&reordered, &reader.side, &others, standing)
                } else {
                    reordered_wording(words, &reordered, &others, standing)
                };
                self.tell(work, reordered.kind(), reader, &text).await?;
            }
            self.record_order(work, &reordered, !first_order).await?;
        }
        Ok(changed)
    }

    /// Who settles the order of a collision the rule ties. dibs raises the tie with the project's
    /// arbiters itself, as an escalation about the lower of its two items that reaches both holders
    /// and the arbiters, and records the pair on it. When an escalation about that item already
    /// stands, the pair goes to that one instead, at most once, and not at all while one of its
    /// holders is among that escalation's arbiters. When no arbiter would receive it, nothing is
    /// escalated and the holders agree between them. `None` for an order the rule decided, or none.
    async fn settle(
        &self,
        work: &mut Work,
        claims: &[Claim],
        names: &Names,
        items: &BTreeMap<ItemId, ItemRecord>,
        overlap: &Overlap,
    ) -> Result<Option<Settling>, AppError> {
        if overlap.order() != Some(&OverlapOrder::Tied) {
            return Ok(None);
        }
        let (Some(pair), Some(subject)) = (
            tie_pair(overlap),
            overlap
                .items()
                .first()
                .and_then(|item| claims.iter().find(|claim| claim.item == *item)),
        ) else {
            return Ok(None);
        };
        let record = items
            .get(&subject.item)
            .ok_or(AppError::NotFound(Missing::Item(subject.item)))?;
        let named = overlap
            .items()
            .iter()
            .map(|item| {
                items
                    .get(item)
                    .map(|record| ItemName::of(record, self.schemes))
                    .ok_or(AppError::NotFound(Missing::Item(*item)))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let reason = tie_reason(overlap, &named, subject.state.is_in_review())?;
        let holders: BTreeSet<MemberId> = claims
            .iter()
            .filter(|claim| overlap.involves(claim.item))
            .map(|claim| claim.member)
            .collect();
        let project = work.project_id();
        let about = EscalationSubject::Item(subject.item);
        if let Some(standing) = work
            .open_escalations(project)
            .await?
            .into_iter()
            .find(|open| open.subject == about)
        {
            return self
                .join(work, names, &standing, pair, &holders, &reason)
                .await
                .map(Some);
        }
        let case = Case {
            subject: about,
            about: item_about(record, self.schemes),
            parties: Parties {
                holders,
                requester: None,
            },
            origin: Origin::Dibs(Raised {
                recorded_for: subject.member,
                tie: pair,
                why: TIE_WHY,
                meanwhile: TIE_MEANWHILE,
            }),
            reason: &reason,
        };
        match case
            .send(work, self.ids, self.settings.arbiters(), self.now)
            .await
        {
            Ok(_) => Ok(Some(Settling::Arbiters {
                outside: BTreeSet::new(),
                before: false,
            })),
            Err(AppError::Escalation(_)) => Ok(Some(Settling::Holders)),
            Err(error) => Err(error),
        }
    }

    /// Puts a tie to the escalation that stands about its lower item, so that a pair reaches the
    /// arbiters at most once for each escalation that stands. A tie one of whose holders is among
    /// that escalation's arbiters is not added, since that holder could settle their own claim's
    /// order alone: it is left to the holders. A pair the escalation already carries is not put
    /// again. Any other is added to its conversation and recorded on it.
    async fn join(
        &self,
        work: &mut Work,
        names: &Names,
        standing: &EscalationRecord,
        pair: [ItemId; 2],
        holders: &BTreeSet<MemberId>,
        reason: &EscalationReason,
    ) -> Result<Settling, AppError> {
        if !standing.arbiters.is_disjoint(holders) {
            return Ok(Settling::HolderArbitrates);
        }
        let outside = outside_of(work, standing.thread, holders).await?;
        if standing.ties.contains(&pair) {
            return Ok(Settling::Arbiters {
                outside,
                before: true,
            });
        }
        self.add_tie(work, names, standing.thread, &outside, reason)
            .await?;
        let project = work.project_id();
        work.add_escalation_tie(project, standing.message, pair)
            .await?;
        Ok(Settling::Arbiters {
            outside,
            before: false,
        })
    }

    /// Adds a tie to the escalation that stands about its lower item: an `urgent` message from dibs
    /// in that escalation's conversation, naming the two work items, where their changes meet and
    /// why the order rule cannot decide. That conversation's readers stay those it reached, so the
    /// message names each holder of the tie who does not read it, `outside`, and asks the arbiters
    /// to write the answer to them as well.
    async fn add_tie(
        &self,
        work: &mut Work,
        names: &Names,
        thread: ThreadId,
        outside: &BTreeSet<MemberId>,
        reason: &EscalationReason,
    ) -> Result<(), AppError> {
        let unread = names_outside(names, outside)?;
        let lead = format!("{TIE_ADDED}{unread}{TIE_ADDED_MEANWHILE}");
        SystemReply {
            thread,
            message: MessageId(self.ids.next()),
            body: titled_body(&lead, Some(("Why", reason.as_str())))?,
            importance: Importance::Urgent,
            ack_required: false,
        }
        .send(work, self.now)
        .await?;
        Ok(())
    }

    /// Tells the holder of each claim the overlap names, in that claim's own conversation, and
    /// records once that it was found. A collision's notice says which claim lands first and why,
    /// or who decides when the rule ties. A moved base found within a day of the last one told
    /// about the same claim is recorded but not told.
    async fn announce(
        &self,
        work: &mut Work,
        claims: &[Claim],
        names: &Names,
        items: &BTreeMap<ItemId, ItemRecord>,
        overlap: &Overlap,
        settling: Option<&Settling>,
    ) -> Result<(), AppError> {
        let parties = self.parties(claims, names, items, overlap)?;
        if parties.is_empty() {
            return Ok(());
        }
        for (index, reader) in parties.iter().enumerate() {
            let others = others_of(&parties, index);
            let text = wording(
                overlap,
                &reader.side,
                &others,
                standing(overlap, reader.claim, settling),
            );
            self.tell(work, overlap.kind(), reader, &text).await?;
        }
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

    /// The claims the overlap names, each with how a notice describes it. An item no claim holds
    /// is left out.
    fn parties<'c>(
        &self,
        claims: &'c [Claim],
        names: &Names,
        items: &BTreeMap<ItemId, ItemRecord>,
        overlap: &Overlap,
    ) -> Result<Vec<Party<'c>>, AppError> {
        let repos: BTreeSet<&RepoId> = overlap.paths().iter().map(|path| &path.repo).collect();
        let mut parties = Vec::new();
        for item in overlap.items() {
            let Some(claim) = claims.iter().find(|claim| claim.item == *item) else {
                continue;
            };
            let record = items
                .get(item)
                .ok_or(AppError::NotFound(Missing::Item(*item)))?;
            parties.push(Party {
                side: Side {
                    item: ItemName::of(record, self.schemes),
                    holder: names.handle(claim.member)?,
                    branches: repos
                        .iter()
                        .filter_map(|repo| claim.branch(repo))
                        .cloned()
                        .collect(),
                },
                claim,
            });
        }
        Ok(parties)
    }

    /// Writes `text` about an overlap of `kind` to the holder of one claim it names, in that
    /// claim's conversation. The claim's link to its conversation is read once: it says both where
    /// to write and when a moved base was last told there. An overlap of a kind told at most so
    /// often is not written within that time of the last one told about the claim, and when it is
    /// written, the time is kept on the conversation.
    async fn tell(
        &self,
        work: &mut Work,
        kind: OverlapKind,
        reader: &Party<'_>,
        text: &str,
    ) -> Result<(), AppError> {
        let link = claim_link(work, reader.claim).await?;
        if !kind.is_told(link.and_then(|link| link.base_moved_told_at), self.now) {
            return Ok(());
        }
        let thread = ClaimNotice {
            claim: reader.claim,
            item: &reader.side.item,
            body: titled_body(text, None)?,
            importance: kind.importance(),
        }
        .send_linked(work, self.ids, self.now, link)
        .await?;
        if kind.told_at_most_every().is_some() {
            let project = work.project_id();
            work.set_base_moved_told_at(project, thread, self.now)
                .await?;
        }
        Ok(())
    }
}

/// The items the overlaps name, read together, by id.
async fn items_named(
    work: &mut Work,
    overlaps: &[Overlap],
) -> Result<BTreeMap<ItemId, ItemRecord>, AppError> {
    let project = work.project_id();
    let wanted: BTreeSet<ItemId> = overlaps
        .iter()
        .flat_map(|overlap| overlap.items().iter().copied())
        .collect();
    Ok(work
        .items_of(project, &wanted)
        .await?
        .into_iter()
        .map(|record| (record.id, record))
        .collect())
}

/// The order of a collision, from its two claims, live or stale, and their items' priorities;
/// `None` for a moved base, and for a collision whose claim or item is not among those given.
fn order_of(
    overlap: &Overlap,
    claims: &[Claim],
    items: &BTreeMap<ItemId, ItemRecord>,
) -> Option<OverlapOrder> {
    if !overlap.kind().is_collision() {
        return None;
    }
    let sides = overlap
        .items()
        .iter()
        .map(|item| {
            Some(Contender {
                claim: claims.iter().find(|claim| claim.item == *item)?,
                priority: items.get(item)?.priority,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    match sides.as_slice() {
        [a, b] => Some(order(*a, *b)),
        _ => None,
    }
}

/// An overlap whose order names an item it does not: the order is worked out from the overlap's
/// own items, so this is a fault of the stored records.
fn misordered() -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: "an overlap's order names an item the overlap does not".to_owned(),
    })
}

/// The two work items of a collision, the lower first; `None` for an overlap that names one.
fn tie_pair(overlap: &Overlap) -> Option<[ItemId; 2]> {
    let mut items = overlap.items().iter().copied();
    Some([items.next()?, items.next()?])
}

/// The holders among `holders` who do not read the conversation `thread`.
async fn outside_of(
    work: &mut Work,
    thread: ThreadId,
    holders: &BTreeSet<MemberId>,
) -> Result<BTreeSet<MemberId>, AppError> {
    let project = work.project_id();
    let conversation = work.thread(project, thread).await?.ok_or_else(|| {
        AppError::Store(StoreError::Corrupt {
            what: "a standing escalation's conversation is missing".to_owned(),
        })
    })?;
    let members = work.members(project).await?;
    Ok(holders
        .iter()
        .copied()
        .filter(|holder| {
            !members.iter().any(|member| {
                member.id == *holder
                    && may_read(
                        conversation.author,
                        &conversation.recipients,
                        member.id,
                        member.role,
                    )
            })
        })
        .collect())
}

/// One claim an overlap names, with how its message describes it.
struct Party<'a> {
    side: Side,
    claim: &'a Claim,
}

/// The sides of every party but the one at `index`.
fn others_of<'p>(parties: &'p [Party<'_>], index: usize) -> Vec<&'p Side> {
    parties
        .iter()
        .enumerate()
        .filter(|(other, _)| *other != index)
        .map(|(_, party)| &party.side)
        .collect()
}

/// How a message about an overlap describes one claim it names: its item, its holder, and the
/// branches its work is on in the repositories of the overlap's paths, as far as they are
/// recorded.
struct Side {
    item: ItemName,
    holder: Handle,
    branches: Vec<BranchName>,
}

impl Side {
    /// The reader's own branches: `your branch alice/1-x`, or `your branch` when none is recorded.
    fn yours(&self) -> String {
        match self.branches.as_slice() {
            [] => "your branch".to_owned(),
            [one] => format!("your branch {one}"),
            many => format!("your branches {}", joined(many)),
        }
    }

    /// Another member's claim: `work item gh:acme/widgets#42 "Title" (alice, on alice/42-x)`.
    fn described(&self) -> String {
        let on = if self.branches.is_empty() {
            String::new()
        } else {
            format!(", on {}", joined(&self.branches))
        };
        format!(
            "work item {} \"{}\" ({}{on})",
            self.item.label(),
            self.item.title,
            self.holder
        )
    }

    /// Another member's claim, named again once described: `work item gh:acme/widgets#42`.
    fn named(&self) -> String {
        format!("work item {}", self.item.label())
    }
}

/// Who decides the order of a collision the rule ties, as dibs settled it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Settling {
    /// The project's arbiters, in an escalation dibs raised or added the tie to. `outside` are the
    /// tie's holders who do not read that escalation's conversation. `before` is set when the
    /// escalation already carried the pair, so that nothing was put to the arbiters now.
    Arbiters {
        outside: BTreeSet<MemberId>,
        before: bool,
    },
    /// The holders between them: no arbiter would receive the question.
    Holders,
    /// The holders between them: an escalation about the tie's lower item stands, and one of the
    /// holders is among its arbiters.
    HolderArbitrates,
}

impl Settling {
    /// Who decides, as the holder `reader` is told.
    fn for_reader(&self, reader: MemberId) -> TieSettling {
        match self {
            Self::Arbiters {
                outside,
                before: true,
            } => TieSettling::AskedBefore {
                reads: !outside.contains(&reader),
            },
            Self::Arbiters { outside, .. } if outside.contains(&reader) => {
                TieSettling::ArbitersWriting
            }
            Self::Arbiters { .. } => TieSettling::Arbiters,
            Self::Holders => TieSettling::Holders,
            Self::HolderArbitrates => TieSettling::HolderArbitrates,
        }
    }
}

/// Who decides the order of a collision the rule ties, as one holder is told.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TieSettling {
    /// The project's arbiters, in an escalation the holder reads.
    Arbiters,
    /// The project's arbiters, in an escalation the holder does not read: they were asked to write
    /// their answer to the holder.
    ArbitersWriting,
    /// The project's arbiters, who were asked about these two claims before, in an escalation the
    /// holder reads or not.
    AskedBefore { reads: bool },
    /// The holders between them: no arbiter would receive the question.
    Holders,
    /// The holders between them: one of them is among the arbiters of the escalation that stands.
    HolderArbitrates,
}

impl TieSettling {
    /// The sentence that says who decides; `write` is how to write to the other holders.
    fn then(self, write: &str) -> String {
        match self {
            Self::Arbiters => "dibs asked the project's arbiters.".to_owned(),
            Self::ArbitersWriting => "dibs asked the project's arbiters in an escalation you do not read, and asked them to write their answer to you.".to_owned(),
            Self::AskedBefore { reads: true } => {
                "dibs already asked the project's arbiters about these two claims.".to_owned()
            }
            Self::AskedBefore { reads: false } => "dibs already asked the project's arbiters about these two claims, in an escalation you do not read.".to_owned(),
            Self::Holders => {
                format!("No arbiter can be asked, so {write} and agree on the order.")
            }
            Self::HolderArbitrates => format!(
                "An escalation about one of these work items already stands and one of you is among its arbiters, so {write} and agree on the order."
            ),
        }
    }
}

/// Where the reader of a notice stands in the overlap's order, as the notice tells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// There is no order to tell: a moved base, or a collision whose order is not known.
    Unordered,
    /// The reader's claim lands first, by this step of the rule.
    First(OrderRule),
    /// The other claim lands first, by this step of the rule.
    Second(OrderRule),
    /// The rule ties.
    Tied {
        /// Whether both claims are in review; otherwise both are at work.
        in_review: bool,
        /// Who decides.
        settling: TieSettling,
    },
}

/// Where the holder of `reader`'s claim stands in the overlap's order. A tie that went to nobody
/// is left to the holders.
fn standing(overlap: &Overlap, reader: &Claim, settling: Option<&Settling>) -> Standing {
    match overlap.landing(reader.item) {
        Landing::First { because } => Standing::First(because),
        Landing::Second { because, .. } => Standing::Second(because),
        Landing::Tied => Standing::Tied {
            in_review: reader.state.is_in_review(),
            settling: settling.map_or(TieSettling::Holders, |settled| {
                settled.for_reader(reader.member)
            }),
        },
        Landing::Unknown => Standing::Unordered,
    }
}

/// How the notices word a collision of one kind.
struct CollisionWords {
    /// Where the two changes meet, given the paths: `change the same files: a.rs`.
    meet: fn(&str) -> String,
    /// What the claim that lands second does once the first merges, as the first is told.
    follows: &'static str,
    /// What the holder of the claim that lands second is asked to do, given how the shared files
    /// are named.
    second: fn(&str) -> String,
}

fn same_file_meet(paths: &str) -> String {
    format!("change the same files: {paths}")
}

fn same_file_second(files: &str) -> String {
    format!(
        "once it merges, rebase onto the base and resolve {files}, then say in this conversation that you rebased."
    )
}

fn contract_meet(paths: &str) -> String {
    format!(
        "meet at a published contract: one changes {paths}, and the other works where that contract is used"
    )
}

fn contract_second(_files: &str) -> String {
    "read the other change and build on it once it merges, then say in this conversation that you rebased.".to_owned()
}

const SAME_FILE_WORDS: CollisionWords = CollisionWords {
    meet: same_file_meet,
    follows: "rebases onto it",
    second: same_file_second,
};

const CONTRACT_WORDS: CollisionWords = CollisionWords {
    meet: contract_meet,
    follows: "builds on it",
    second: contract_second,
};

/// How the notices word an overlap of the kind; `None` for a moved base, which is no collision.
fn collision_words(kind: OverlapKind) -> Option<&'static CollisionWords> {
    match kind {
        OverlapKind::SameFile => Some(&SAME_FILE_WORDS),
        OverlapKind::Contract => Some(&CONTRACT_WORDS),
        OverlapKind::MainMoved => None,
    }
}

/// What a notice says of a collision's order to one holder.
struct OrderWords<'a> {
    words: &'a CollisionWords,
    /// The other claims, named again: `work item gh:acme/widgets#2`.
    other: &'a str,
    /// How to write to their holders.
    write: &'a str,
}

impl OrderWords<'_> {
    /// The sentences that say who lands first and why, and what the reader does, starting in lower
    /// case. `now` goes before `lands first` (`now ` for an order that changed), `which` is how a
    /// tie names what cannot be decided, and `files` how the claim landing second is asked to
    /// resolve the shared files.
    fn sentences(&self, standing: Standing, now: &str, which: &str, files: &str) -> String {
        let other = self.other;
        match standing {
            Standing::First(rule) => format!(
                "yours {now}lands first, because {}. {} {} once it merges; go on with your work.",
                rule.why(),
                capitalized(other),
                self.words.follows
            ),
            Standing::Second(rule) => format!(
                "{other} {now}lands first, because {}. {}",
                rule.why(),
                capitalized(&(self.words.second)(files))
            ),
            Standing::Tied {
                in_review,
                settling,
            } => {
                format!(
                    "{} {}",
                    undecided(which, in_review),
                    settling.then(self.write)
                )
            }
            Standing::Unordered => format!("{} to agree which lands first.", self.write),
        }
    }
}

/// Why the order rule cannot decide `which`: both claims are in review or both at work, with the
/// same priority and claimed at the same time.
fn undecided(which: &str, in_review: bool) -> String {
    let state = if in_review { "in review" } else { "at work" };
    format!(
        "the order rule cannot decide {which}: both are {state}, their work items have the same priority and their claims were made at the same time."
    )
}

/// The text with its first letter in upper case.
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// The names joined with `and`.
fn joined(names: &[impl ToString]) -> String {
    names
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" and ")
}

/// The overlap's paths for a sentence: the first `most` by name, then how many more.
fn listed(paths: &BTreeSet<RepoPath>, most: usize) -> String {
    let named: Vec<String> = paths.iter().take(most).map(ToString::to_string).collect();
    let more = paths.len().saturating_sub(most);
    if more == 0 {
        named.join(", ")
    } else {
        format!("{} and {more} more", named.join(", "))
    }
}

/// The other claims a notice names, described once and then named again, and how to write to
/// their holders. The holders do not share a conversation, so a collision says how to write to
/// the others.
fn others_words(others: &[&Side]) -> (String, String, String) {
    if others.is_empty() {
        return (
            "another claim".to_owned(),
            "the other claim".to_owned(),
            "write to its holder".to_owned(),
        );
    }
    let described: Vec<String> = others.iter().map(|side| side.described()).collect();
    let named: Vec<String> = others.iter().map(|side| side.named()).collect();
    let ways: Vec<String> = others
        .iter()
        .map(|side| format!("{} (dibs send {})", side.holder, side.holder))
        .collect();
    (
        joined(&described),
        joined(&named),
        format!("write to {}", joined(&ways)),
    )
}

/// What the holder of the claim `reader` is told of the overlap, which names the claims `others`
/// too: it opens with what happened and says what the overlap is from the reader's side. A
/// collision then says which claim lands first and why, and what the reader does: go on with the
/// work, or rebase once the other merges; a tie says who decides. A moved base asks for a rebase
/// when the claim is next worked on.
fn wording(overlap: &Overlap, reader: &Side, others: &[&Side], standing: Standing) -> String {
    let yours = reader.yours();
    let paths = listed(overlap.paths(), LISTED_PATHS);
    let Some(words) = collision_words(overlap.kind()) else {
        return format!(
            "Base moved: pull request {} was merged into the base after {yours} took its base, and it changed files that branch changes too: {paths}. Rebase onto the base when you next work on this claim.",
            overlap.pr().map(ToString::to_string).unwrap_or_default()
        );
    };
    let (theirs, other, write) = others_words(others);
    let order = OrderWords {
        words,
        other: &other,
        write: &write,
    };
    format!(
        "Overlap: {yours} and the branch of {theirs} {}. {}",
        (words.meet)(&paths),
        capitalized(&order.sentences(standing, "", "which lands first", "these files"))
    )
}

/// What the holder of a claim is told when its collision's order changed: who lands first now and
/// why, and what to do, or that the rule ties now and who decides.
fn reordered_wording(
    words: &CollisionWords,
    overlap: &Overlap,
    others: &[&Side],
    standing: Standing,
) -> String {
    let (_, other, write) = others_words(others);
    let order = OrderWords {
        words,
        other: &other,
        write: &write,
    };
    let which = format!("whether yours or {other} lands first");
    let files = format!(
        "the shared files ({})",
        listed(overlap.paths(), LISTED_PATHS)
    );
    format!(
        "The order changed: {}",
        order.sentences(standing, "now ", &which, &files)
    )
}

/// The most paths an escalation of a tie lists by name.
const REASON_PATHS: usize = 5;

/// Why dibs raises a tie with the arbiters, as the escalation opens.
const TIE_WHY: &str = "the order rule cannot tell two colliding claims apart, so the arbiters of this project decide which one lands first.";

/// What the holders of a tie do until an arbiter answers, as the escalation dibs raised says.
const TIE_MEANWHILE: &str = "Only the order is undecided, not the area: both holders read this conversation, go on with their work and follow the arbiter's answer here.";

/// How dibs opens a tie it adds to an escalation that stands.
const TIE_ADDED: &str = "dibs adds a question to this escalation; nobody asked: the order rule cannot tell the two colliding claims named below apart, so the arbiters of this project decide which of them lands first as well. ";

/// What the holders of a tie added to an escalation do until an arbiter answers.
const TIE_ADDED_MEANWHILE: &str =
    "Only the order is undecided, not the area: both holders go on with their work.";

/// The sentence naming the holders of a tie added to an escalation who do not read its
/// conversation, with how to write to them; empty when every holder reads it.
fn names_outside(names: &Names, outside: &BTreeSet<MemberId>) -> Result<String, AppError> {
    let handles = outside
        .iter()
        .map(|member| names.handle(*member))
        .collect::<Result<Vec<_>, _>>()?;
    if handles.is_empty() {
        return Ok(String::new());
    }
    let ways: Vec<String> = handles
        .iter()
        .map(|handle| format!("dibs send {handle}"))
        .collect();
    let verb = if handles.len() == 1 { "does" } else { "do" };
    Ok(format!(
        "{} {verb} not read this conversation: write the answer to them as well ({}). ",
        joined(&handles),
        ways.join(", ")
    ))
}

/// Why dibs raises a tie with the arbiters: which two work items, where their changes meet, and
/// that the order rule cannot decide because both are in review or both at work, with the same
/// priority and claimed at the same time. Its first paths are named while the reason stays within
/// its limit; otherwise they are only counted.
fn tie_reason(
    overlap: &Overlap,
    items: &[ItemName],
    in_review: bool,
) -> Result<EscalationReason, AppError> {
    let Some(words) = collision_words(overlap.kind()) else {
        return Err(unwritable());
    };
    let named: Vec<String> = items
        .iter()
        .map(|item| format!("{} \"{}\"", item.label(), item.title))
        .collect();
    let text = |paths: &str| {
        format!(
            "Work items {} {}. {}",
            joined(&named),
            (words.meet)(paths),
            capitalized(&undecided("which lands first", in_review))
        )
    };
    let count = match overlap.paths().len() {
        1 => "one path, too long to name here".to_owned(),
        many => format!("{many} paths, too long to name here"),
    };
    EscalationReason::parse(&text(&listed(overlap.paths(), REASON_PATHS)))
        .or_else(|_| EscalationReason::parse(&text(&count)))
        .map_err(|_| unwritable())
}

/// A reason dibs writes for an escalation that does not make one.
fn unwritable() -> AppError {
    AppError::Store(StoreError::Corrupt {
        what: "the reason of an escalation dibs writes is not valid".to_owned(),
    })
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
