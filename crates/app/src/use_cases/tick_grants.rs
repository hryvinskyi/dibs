//! The sweep that decides, on their holders' behalf, the requests for grants and the wide claims'
//! announcements that still wait.

#[cfg(test)]
mod tests;

use std::sync::Arc;

use dibs_domain::grant::{OnBehalf, revoke_outlived};
use dibs_domain::hot_rule::HotRuleRegistry;
use dibs_domain::ids::ProjectId;
use dibs_domain::ref_scheme::RefSchemeRegistry;
use dibs_domain::wide::AckOnBehalf;

use crate::dependencies::Dependencies;
use crate::error::AppError;
use crate::ports::{Clock, EventNotifier, IdGen, Store, StoreError};
use crate::use_cases::on_behalf::OnBehalfOf;
use crate::use_cases::wide_asks::{AckFacts, DibsAcks};
use crate::work::Work;

/// What one sweep of the requests for grants did.
#[derive(Debug, Default)]
pub struct GrantSweep {
    /// How many projects were swept; the ones that failed are in `failed` instead.
    pub projects: usize,
    /// How many requests dibs granted on their holders' behalf.
    pub granted: usize,
    /// How many requests were settled without a grant: their claim had ended, their holder holds
    /// the area through no claim any more, or the grant dibs would give existed already or was
    /// refused by the rules of every grant. Requests left to their holders count here once they
    /// close, or once the revoke that left them no longer applies.
    pub settled: usize,
    /// How many wide claims' announcements dibs acknowledged on their holders' behalf.
    pub acknowledged: usize,
    /// How many requests and announcements dibs left to their holders for good, because a revoke
    /// between the same two claims sticks.
    pub left_to_holders: usize,
    /// How many announcements dibs settled because they closed: their wide claim ended, or their
    /// holder holds none of their areas through any claim. Announcements left to their holders
    /// count here once they close, or once the revoke that left them no longer applies.
    pub announcements_closed: usize,
    /// The projects the sweep could not sweep, each with why; the others were swept all the same.
    pub failed: Vec<(ProjectId, AppError)>,
}

/// Decides, in every project, each request for a grant that is not settled yet, on its holder's
/// behalf and from what dibs stores, as the request was decided when it was asked: whether or not
/// the holder answered in its conversation, a request is granted once its holder's claim is in
/// review, the area lies idle on that claim, or the claim's branch changes no path of the area,
/// and otherwise once the project's `grant_after` has passed since it was asked. A grant dibs
/// gives is the holder's grant, given as every grant is given, and dibs says so, why, and how to
/// undo it in the request's conversation.
///
/// dibs decides a request once: a request it grants is settled, and so is one whose claim has
/// ended, which is never granted to a later claim, or whose holder holds the area through no claim
/// any more, which is never granted when they hold it again. A request a revoke between the same
/// two claims stands against is left to the holder for good: dibs marks it so and never grants it,
/// and the holder still sees it; the sweep only settles it, once it closes as any other or once
/// every revoke recorded between the two members in its area has stopped applying, because the
/// holder holds the area through another claim than the one the revoke names, so that a holder is
/// not shown it on a claim the old revoke does not reach. One left with no revoke recorded at all
/// keeps its mark until it closes. For such a request no footprint is read. A request that waits
/// or has lapsed for now is left as it is. Several requests of one asker for one area are granted
/// once: the grant given first answers the others.
///
/// A wide claim's announcement that neither its holder nor dibs has acknowledged is judged by the
/// same test as the requests: dibs acknowledges it on the holder's behalf once every
/// area of the holder it names passes it, or once `grant_after` has passed since it was sent. One a
/// revoke between the same two claims in one of its areas stands against is left to the holder for
/// good: dibs never acknowledges it and reads no footprint for it; it is settled too once every
/// revoke recorded between the two members in its areas has stopped applying. One whose wide
/// claim has ended, or whose holder holds none of its areas through any claim, is settled, left to
/// its holder or not, so that it no longer counts as asking and a holder who holds its areas again
/// is announced to anew; the holder may still acknowledge it. One that only stale claims keep is
/// left as it is.
///
/// Each project is swept in a transaction of its own, which holds the store's write lock from its
/// start, so a request being asked and the sweep never decide the same request at once; the
/// requests, claims and grants are read inside it; the footprints only when a request or an
/// announcement is not left to its holder. A project with nothing to decide, or whose only
/// requests and announcements are left to their holders and none of them is settled, is committed
/// without a write. The service runs it on a schedule; it acts for nobody.
pub struct TickGrants {
    store: Arc<dyn Store>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGen>,
    notifier: Arc<dyn EventNotifier>,
    hot_rules: Arc<HotRuleRegistry>,
    ref_schemes: Arc<RefSchemeRegistry>,
}

impl TickGrants {
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
    pub async fn execute(&self) -> Result<GrantSweep, AppError> {
        let mut tx = self.store.read().await?;
        let projects = tx.project_ids().await?;
        drop(tx);
        let mut report = GrantSweep::default();
        for project in projects {
            match self.sweep(project).await {
                Ok(swept) => {
                    report.projects += 1;
                    report.granted += swept.granted;
                    report.settled += swept.settled;
                    report.acknowledged += swept.acknowledged;
                    report.left_to_holders += swept.left_to_holders;
                    report.announcements_closed += swept.announcements_closed;
                }
                Err(error) => report.failed.push((project, error)),
            }
        }
        Ok(report)
    }

    /// Decides the project's unsettled requests and open announcements, and settles those left to
    /// their holders that closed or whose revoke no longer applies, answering what it did.
    async fn sweep(&self, project: ProjectId) -> Result<Swept, AppError> {
        let now = self.clock.now();
        let mut work = Work::begin_unattended(&*self.store, project).await?;
        let requests = work.unsettled_asked_grants(project).await?;
        let announcements = work.open_wide_asks_with_time(project).await?;
        if requests.is_empty() && announcements.is_empty() {
            work.commit_quietly().await?;
            return Ok(Swept::default());
        }
        let settings = work.project_settings(&self.hot_rules).await?;
        let claims = work.claims(project).await?;
        let mut records = work.grants(project).await?;
        let blocks = work.grant_blocks(project).await?;
        let judging = requests.iter().any(|(record, _)| !record.left_to_holder)
            || announcements
                .iter()
                .any(|(record, _)| !record.left_to_holder);
        let listed = if judging {
            work.footprints(project).await?
        } else {
            Vec::new()
        };
        let behalf = OnBehalfOf {
            claims: &claims,
            blocks: &blocks,
            listed: &listed,
            settings: &settings,
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            now,
        };
        let mut swept = Swept::default();
        for (record, asked_at) in requests {
            let asked = record.asked;
            if record.left_to_holder {
                let outlived = revoke_outlived(
                    &blocks,
                    asked.ask.asker,
                    asked.holder,
                    |area| *area == asked.ask.area,
                    &claims,
                );
                if outlived || behalf.closed(&asked, asked_at, &records) {
                    work.settle_asked_grant(project, asked.message, now).await?;
                    swept.settled += 1;
                }
                continue;
            }
            match behalf.decide(&mut work, &asked, asked_at, &records).await? {
                OnBehalf::Grant { because, .. } => {
                    let thread = work
                        .message(project, asked.message)
                        .await?
                        .ok_or_else(|| StoreError::Corrupt {
                            what: "the message that asked an unsettled request is not stored"
                                .to_owned(),
                        })?
                        .thread;
                    match behalf
                        .grant(&mut work, &asked, asked_at, because, thread, &records)
                        .await?
                    {
                        Some(given) => {
                            records.push(given.record);
                            swept.granted += 1;
                        }
                        None => swept.settled += 1,
                    }
                }
                OnBehalf::Closed => {
                    work.settle_asked_grant(project, asked.message, now).await?;
                    swept.settled += 1;
                }
                OnBehalf::LeftToHolder => {
                    work.leave_asked_grant_to_holder(project, asked.message)
                        .await?;
                    swept.left_to_holders += 1;
                }
                OnBehalf::Wait { .. } | OnBehalf::Lapsed => {}
            }
        }
        let acks = DibsAcks {
            ids: &*self.ids,
            schemes: &self.ref_schemes,
            settings: &settings,
            now,
        };
        let facts = AckFacts {
            claims: &claims,
            blocks: &blocks,
            listed: &listed,
        };
        for (record, sent_at) in announcements {
            let ask = record.ask;
            if record.left_to_holder {
                let outlived = revoke_outlived(
                    &blocks,
                    ask.from,
                    ask.to,
                    |area| ask.areas.contains(area),
                    &claims,
                );
                if outlived {
                    work.settle_wide_ask(project, ask.message, now).await?;
                    swept.announcements_closed += 1;
                } else if acks
                    .settle_if_closed(&mut work, &ask, sent_at, &facts)
                    .await?
                {
                    swept.announcements_closed += 1;
                }
                continue;
            }
            match acks.judge(&mut work, &ask, sent_at, &facts).await? {
                AckOnBehalf::Ack { .. } => swept.acknowledged += 1,
                AckOnBehalf::LeftToHolder => swept.left_to_holders += 1,
                AckOnBehalf::Closed => swept.announcements_closed += 1,
                AckOnBehalf::Wait { .. } | AckOnBehalf::Lapsed => {}
            }
        }
        if swept.wrote_nothing() {
            work.commit_quietly().await?;
        } else {
            work.commit(&*self.notifier).await?;
        }
        Ok(swept)
    }
}

/// How many requests one project's sweep granted and settled without a grant, how many
/// announcements it acknowledged and closed, and how many of either it left to their holders.
#[derive(Debug, Clone, Copy, Default)]
struct Swept {
    granted: usize,
    settled: usize,
    acknowledged: usize,
    left_to_holders: usize,
    announcements_closed: usize,
}

impl Swept {
    /// Whether the sweep changed nothing in the project.
    const fn wrote_nothing(&self) -> bool {
        self.granted == 0
            && self.settled == 0
            && self.acknowledged == 0
            && self.left_to_holders == 0
            && self.announcements_closed == 0
    }
}
