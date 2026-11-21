//! The sweep that releases claims whose work stopped showing, and gives back the areas of live
//! claims their work never showed in.

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::sync::Arc;

use time::{Duration, OffsetDateTime};

use dibs_domain::area::QualifiedArea;
use dibs_domain::claim::{
    Claim, FootprintPaths, IdleAreas, decide_return, due_for_release, idle_areas,
};
use dibs_domain::event::{AreasReturned, EventKind, ReleasedStale};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::{ItemId, ProjectId};
use dibs_domain::item::ItemStatus;
use dibs_domain::message::Importance;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::repo::RepoId;
use dibs_domain::settings::{ProjectSettings, format_duration};

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::named::ItemName;
use crate::ports::{Clock, EventNotifier, IdGen, Store};
use crate::use_cases::claim_ending::{Ending, end_claim};
use crate::use_cases::claim_notice::ClaimNotice;
use crate::use_cases::common::existing_item;
use crate::use_cases::handover::{Handovers, give_areas_up};
use crate::use_cases::writing::titled_body;
use crate::work::Work;

/// What one sweep of the claims did.
#[derive(Debug, Default)]
pub struct ClaimSweep {
    /// How many projects were swept; the ones that failed are in `failed` instead.
    pub projects: usize,
    /// How many claims were released.
    pub released: usize,
    /// How many live claims gave idle areas back.
    pub returned: usize,
    /// The projects the sweep could not sweep, each with why; the others were swept all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Releases, in every project, each working claim that went the project's `release_after` without
/// a sign of its own work: the item is open again, `released_stale` is recorded, what the claim
/// held or waited for is let go as any ending lets it go, and its holder is told in the claim's
/// conversation.
///
/// Then it gives back, from each live claim (worked on and seen within `stale_after`, or in
/// review), the areas idle for the project's `idle_area_after`: held, their clock older than
/// that, no path of the claim's stored footprints in them, and the footprint of their repository,
/// when there is one, not truncated. The claim no longer holds them, `areas_returned` is recorded,
/// the grants its holder gave in them end, an area another claim waited for moves to that claim,
/// and its holder is told in the claim's conversation. Giving areas back is no sign of the claim's
/// work, so when the claim was last seen stays as it was. Only a claim with an area whose clock is
/// past the delay has its footprints read. An area past the delay that its footprints keep — a
/// path of them in it, or its repository's footprint truncated — has its clock started again at
/// the sweep, in the same transaction, so that it is not read again before the delay has passed
/// once more; that too leaves when the claim was last seen as it was.
///
/// Each project is swept in a transaction of its own, and the claims are read inside it, so a
/// request that showed a claim's work before the sweep took the project keeps that claim and its
/// areas. A project with no claim due and no area idle is left without a write; an idle area a
/// footprint keeps has its clock restarted, which is written. The service runs it on a schedule;
/// it acts for nobody.
pub struct TickClaims {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl TickClaims {
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

    /// Sweeps every project once.
    ///
    /// # Errors
    /// [`AppError::Store`] when the projects cannot be listed; a project that fails, its stored
    /// settings not parsing included, is in the report instead.
    pub async fn execute(&self) -> Result<ClaimSweep, AppError> {
        let mut tx = self.store.read().await?;
        let projects = tx.project_ids().await?;
        drop(tx);
        let mut report = ClaimSweep::default();
        for project in projects {
            match self.sweep(project).await {
                Ok(swept) => {
                    report.projects += 1;
                    report.released += swept.released;
                    report.returned += swept.returned;
                }
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    /// Releases the project's due claims and gives back its idle areas, answering how many claims
    /// each touched.
    async fn sweep(&self, project: ProjectId) -> Result<Swept, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let claims = work.claims(project).await?;
        if claims.is_empty() {
            work.commit_quietly().await?;
            return Ok(Swept::default());
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let release_after = settings.release_after();
        let due: Vec<_> = due_for_release(&claims, now, release_after)
            .into_iter()
            .map(|claim| claim.item)
            .collect();
        let idle: Vec<ItemId> = claims
            .iter()
            .filter(|claim| !claim.idle_candidates(now, &settings).is_empty())
            .map(|claim| claim.item)
            .collect();
        if due.is_empty() && idle.is_empty() {
            work.commit_quietly().await?;
            return Ok(Swept::default());
        }
        let handovers = Handovers {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        let released = self
            .release(&mut work, due, release_after, &handovers)
            .await?;
        let returned = self.give_back(&mut work, idle, &handovers).await?;
        if released == 0 && returned == 0 {
            work.commit_quietly().await?;
        } else {
            work.commit(&*self.notifier).await?;
        }
        Ok(Swept { released, returned })
    }

    /// Releases each claim of `due` that is still due when its turn comes, answering how many.
    async fn release(
        &self,
        work: &mut Work,
        due: Vec<ItemId>,
        release_after: Duration,
        handovers: &Handovers<'_>,
    ) -> Result<usize, AppError> {
        let project = work.project_id();
        let now = handovers.now;
        let mut released = 0;
        for item in due {
            // Ending one claim may hand areas to another, so each is read again before it ends.
            let Some(claim) = work
                .claim(project, item)
                .await?
                .filter(|claim| claim.is_due_for_release(now, release_after))
            else {
                continue;
            };
            let ending = Ending {
                status: ItemStatus::Open,
                event: EventKind::ReleasedStale(ReleasedStale {
                    item: claim.item,
                    member: claim.member,
                    last_seen: claim.last_seen,
                }),
            };
            end_claim(work, &claim, ending, handovers).await?;
            let named = ItemName::of(&existing_item(work, item).await?, &self.ref_schemes);
            ClaimNotice {
                claim: &claim,
                item: &named,
                body: titled_body(&released_stale(&named, release_after), None)?,
                importance: Importance::Normal,
            }
            .send(work, &*self.ids, now)
            .await?;
            released += 1;
        }
        Ok(released)
    }

    /// Gives back the idle areas of each claim of `candidates`, read again when its turn comes,
    /// answering how many claims gave areas back.
    async fn give_back(
        &self,
        work: &mut Work,
        candidates: Vec<ItemId>,
        handovers: &Handovers<'_>,
    ) -> Result<usize, AppError> {
        let project = work.project_id();
        let now = handovers.now;
        let settings = handovers.settings;
        if candidates.is_empty() {
            return Ok(0);
        }
        let footprints = work.footprints(project).await?;
        let mut returned = 0;
        for item in candidates {
            // Releasing a claim or giving areas back may hand areas to another, so each is read
            // again before it is decided.
            let Some(mut claim) = work.claim(project, item).await? else {
                continue;
            };
            let repos = footprints
                .iter()
                .filter(|footprint| footprint.item == item)
                .map(|footprint| &footprint.repo);
            let found = idle_of(work, &claim, repos, settings, now).await?;
            found.restart_kept(&mut claim, now);
            if found.idle.is_empty() {
                if !found.kept.is_empty() {
                    work.save_claim(project, &claim).await?;
                }
                continue;
            }
            let narrowed = decide_return(&claim, &found.idle);
            let event = EventKind::AreasReturned(AreasReturned {
                item,
                member: claim.member,
                areas: narrowed.areas.clone(),
            });
            give_areas_up(work, &narrowed, event, handovers).await?;
            let named = ItemName::of(&existing_item(work, item).await?, &self.ref_schemes);
            let text = areas_returned(&named, &narrowed.areas, settings.idle_area_after());
            ClaimNotice {
                claim: &claim,
                item: &named,
                body: titled_body(&text, None)?,
                importance: Importance::Normal,
            }
            .send(work, &*self.ids, now)
            .await?;
            returned += 1;
        }
        Ok(returned)
    }
}

/// How many claims one project's sweep released, and how many gave idle areas back.
#[derive(Debug, Clone, Copy, Default)]
struct Swept {
    released: usize,
    returned: usize,
}

/// What the idle rule finds in the claim at `now`: nothing without a candidate, else decided on
/// its stored footprints, those of `repos`, read with their paths only then.
async fn idle_of(
    work: &mut Work,
    claim: &Claim,
    repos: impl Iterator<Item = &RepoId>,
    settings: &ProjectSettings,
    now: OffsetDateTime,
) -> Result<IdleAreas, AppError> {
    if claim.idle_candidates(now, settings).is_empty() {
        return Ok(IdleAreas::default());
    }
    let project = work.project_id();
    let mut stored = Vec::new();
    for repo in repos {
        if let Some(footprint) = work.footprint(project, claim.item, repo).await? {
            stored.push(footprint);
        }
    }
    let footprints: Vec<FootprintPaths<'_>> = stored
        .iter()
        .map(|(footprint, paths)| FootprintPaths {
            repo: &footprint.repo,
            paths,
            truncated: footprint.truncated,
        })
        .collect();
    Ok(idle_areas(claim, &footprints, settings, now))
}

/// What the holder of a claim the sweep gave idle areas of back is told: which areas, why — no path
/// of its branch in them and no edit there under the claim for how long — and how to take them
/// back while they are free.
fn areas_returned(item: &ItemName, areas: &BTreeSet<QualifiedArea>, after: Duration) -> String {
    let listed: Vec<String> = areas.iter().map(ToString::to_string).collect();
    let flags: Vec<String> = listed.iter().map(|area| format!("--area {area}")).collect();
    format!(
        "Areas given back: dibs gave back {areas} of your claim on work item {label} \"{title}\": no path of its branch is in them and nothing was edited there under the claim for {after}. To take them back while they are free, declare them again (dibs claim {key} {flags}).",
        areas = listed.join(", "),
        label = item.label(),
        title = item.title,
        after = format_duration(after),
        key = item.key(),
        flags = flags.join(" "),
    )
}

/// What the holder of a claim the sweep released is told: that dibs released it, after how long
/// without a sign of its work and which signs those are, that the item is open again, and how to
/// claim it again.
fn released_stale(item: &ItemName, after: Duration) -> String {
    format!(
        "Released: dibs released your claim on work item {label} \"{title}\" because nothing of its work showed for {after}: no edit made under it, no push of its branch, no renewal. The item is open again. If the work goes on, claim it again (dibs claim {key}).",
        label = item.label(),
        title = item.title,
        after = format_duration(after),
        key = item.key(),
    )
}
